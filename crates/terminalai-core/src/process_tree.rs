//! Windows process-tree containment shared by agent and environment workers.

#[cfg(windows)]
use std::mem::size_of;
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(windows)]
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    OpenProcess, ProcessMemoryPriority, ProcessPowerThrottling, SetProcessInformation,
    MEMORY_PRIORITY_INFORMATION, MEMORY_PRIORITY_LOW, MEMORY_PRIORITY_NORMAL,
    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_STATE, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
};

#[cfg(windows)]
#[derive(Debug)]
pub(crate) struct ProcessJob {
    handle: OwnedHandle,
}

/// What a job is allowed to consume.
///
/// Containment and limitation are different things: the job has always reaped
/// the process tree, but `KILL_ON_JOB_CLOSE` alone lets one leaking agent take
/// the machine down with every other session still nominally healthy. These are
/// the two limits Windows enforces without a completion port.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JobLimits {
    /// Commit charge for the whole job. Exceeding it fails the *allocation*
    /// inside the agent rather than terminating it, which is why a session that
    /// trips this is reported rather than silently killed.
    pub memory_bytes: Option<u64>,
    /// How many processes the job may hold at once. An agent that fork-bombs
    /// stops being able to spawn instead of exhausting the machine.
    pub active_processes: Option<u32>,
}

#[cfg(windows)]
impl ProcessJob {
    pub(crate) fn assign(process: RawHandle) -> Result<Self, String> {
        Self::assign_with_limits(process, JobLimits::default())
    }

    pub(crate) fn assign_with_limits(
        process: RawHandle,
        limits_config: JobLimits,
    ) -> Result<Self, String> {
        let job = Self::create(limits_config)?;
        job.adopt(process)?;
        Ok(job)
    }

    /// Create and fully configure the job *before* the process it will hold
    /// exists.
    ///
    /// Windows offers no way to create a process already inside a job unless the
    /// creator owns the `CreateProcessW` call — `PROC_THREAD_ATTRIBUTE_JOB_LIST`
    /// is set on the attribute list, and `portable-pty` builds that list itself
    /// with room for exactly one entry (the pseudoconsole). So an assignment
    /// after creation is the only reachable option, and the useful thing to
    /// control is how long it takes. Creating and configuring the job up front
    /// leaves a single syscall between the process existing and it being
    /// contained, instead of three.
    pub(crate) fn create(limits_config: JobLimits) -> Result<Self, String> {
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }

        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if let Some(bytes) = limits_config.memory_bytes.filter(|bytes| *bytes > 0) {
            limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
            limits.JobMemoryLimit = bytes as usize;
        }
        if let Some(count) = limits_config.active_processes.filter(|count| *count > 0) {
            limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            limits.BasicLimitInformation.ActiveProcessLimit = count;
        }
        let configured = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } != 0;
        if !configured {
            let error = std::io::Error::last_os_error();
            unsafe {
                CloseHandle(job);
            }
            return Err(error.to_string());
        }

        Ok(Self {
            handle: unsafe { OwnedHandle::from_raw_handle(job as RawHandle) },
        })
    }

    /// Put an already-created process inside this job.
    ///
    /// The membership is read back rather than inferred from the return value.
    /// `AssignProcessToJobObject` succeeding is not the same claim as the
    /// process being in *this* job — a process already in another job that
    /// forbids breakaway is the documented case — and a containment guarantee
    /// that quietly did not apply is worse than one that failed loudly, because
    /// every teardown path downstream trusts it.
    pub(crate) fn adopt(&self, process: RawHandle) -> Result<(), String> {
        let job = self.handle.as_raw_handle() as HANDLE;
        if unsafe { AssignProcessToJobObject(job, process as HANDLE) } == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let mut member: i32 = 0;
        if unsafe { IsProcessInJob(process as HANDLE, job, &mut member) } == 0 {
            return Err(format!(
                "could not confirm job membership: {}",
                std::io::Error::last_os_error()
            ));
        }
        if member == 0 {
            return Err("the process was not in the job after assignment".into());
        }
        Ok(())
    }

    pub(crate) fn terminate(&self) -> Result<(), String> {
        let terminated =
            unsafe { TerminateJobObject(self.handle.as_raw_handle() as HANDLE, 1) } != 0;
        if terminated {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error().to_string())
        }
    }
}

/// Apply Windows' background execution policy to the process at the root of a
/// ConPTY job. The policy is deliberately reversible: focused or pinned rows
/// run at normal priority, while unfocused and unpinned rows use EcoQoS and a
/// lower memory priority.
#[cfg(windows)]
pub(crate) fn set_background_priority(pid: u32, background: bool) -> Result<(), String> {
    let process = unsafe {
        OpenProcess(
            PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        )
    };
    if process.is_null() {
        return Err(std::io::Error::last_os_error().to_string());
    }

    let power = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: if background {
            PROCESS_POWER_THROTTLING_EXECUTION_SPEED
        } else {
            0
        },
    };
    let power_error = unsafe {
        SetProcessInformation(
            process,
            ProcessPowerThrottling,
            (&power as *const PROCESS_POWER_THROTTLING_STATE).cast(),
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        ) != 0
    }
    .then_some(())
    .map_or_else(
        || Some(std::io::Error::last_os_error().to_string()),
        |_| None,
    );

    let memory = MEMORY_PRIORITY_INFORMATION {
        MemoryPriority: if background {
            MEMORY_PRIORITY_LOW
        } else {
            MEMORY_PRIORITY_NORMAL
        },
    };
    let memory_error = unsafe {
        SetProcessInformation(
            process,
            ProcessMemoryPriority,
            (&memory as *const MEMORY_PRIORITY_INFORMATION).cast(),
            size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
        ) != 0
    }
    .then_some(())
    .map_or_else(
        || Some(std::io::Error::last_os_error().to_string()),
        |_| None,
    );
    let error = power_error
        .or(memory_error)
        .map(|error| format!("{error}; process priority may be only partially applied"));
    unsafe {
        CloseHandle(process);
    }
    error.map_or(Ok(()), Err)
}

/// Private commit charge for one process, in bytes.
///
/// `PrivateUsage` rather than `WorkingSetSize`: working set is what is resident
/// right now and drops when Windows trims it under pressure, so a leaking agent
/// can show a *falling* working set while its commit climbs. Private commit is
/// the figure that actually tracks the leak.
///
/// `None` means the question could not be answered — the process is gone, or the
/// handle could not be opened. It never means zero, because reporting a healthy
/// number from the absence of a signal is the failure this whole surface exists
/// to avoid.
#[cfg(windows)]
pub fn private_bytes(pid: u32) -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };

    if pid == 0 {
        return None;
    }
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return None;
    }
    let mut counters: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        GetProcessMemoryInfo(
            process,
            (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
            size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
    } != 0;
    unsafe {
        CloseHandle(process);
    }
    ok.then_some(counters.PrivateUsage as u64)
}

/// Platforms without a job-object equivalent report nothing rather than a
/// plausible-looking zero.
#[cfg(not(windows))]
pub fn private_bytes(_pid: u32) -> Option<u64> {
    None
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::os::windows::io::AsRawHandle;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    /// Measure what is left of the spawn-to-job race.
    ///
    /// The job cannot hold the process from the instant of creation — that
    /// needs `PROC_THREAD_ATTRIBUTE_JOB_LIST` on the attribute list, which
    /// `portable-pty` owns and sizes for one entry. What is controllable is the
    /// length of the gap, and this pins it: with the job created and configured
    /// beforehand, containment costs one `AssignProcessToJobObject`. Measured
    /// 2026-08-04 on Windows 11 26100 at **34.8 µs**. The ceiling is deliberately
    /// two orders of magnitude above that, so this reports a regression in
    /// *kind* — a syscall creeping back in front of the assignment — rather than
    /// machine load.
    #[test]
    fn containment_costs_one_syscall_after_the_process_exists() {
        let job = ProcessJob::create(JobLimits::default()).expect("create job");
        let mut child = Command::new("cmd.exe")
            .args(["/c", "pause"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn child");

        let started = Instant::now();
        job.adopt(child.as_raw_handle()).expect("adopt child");
        let window = started.elapsed();

        assert!(
            window < std::time::Duration::from_millis(5),
            "the uncontained window grew to {window:?}"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn membership_is_read_back_rather_than_assumed() {
        // A job whose assignment silently did not apply would report every
        // teardown as successful while leaving the tree alive.
        let first = ProcessJob::create(JobLimits::default()).expect("create first job");
        let second = ProcessJob::create(JobLimits::default()).expect("create second job");
        let mut child = Command::new("cmd.exe")
            .args(["/c", "pause"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn child");

        first.adopt(child.as_raw_handle()).expect("adopt child");
        // Nested jobs are permitted on this Windows, so the second assignment
        // may succeed; what must never happen is a report of success while the
        // process sits outside the job that claims it.
        if second.adopt(child.as_raw_handle()).is_ok() {
            let mut member: i32 = 0;
            let confirmed = unsafe {
                IsProcessInJob(
                    child.as_raw_handle() as HANDLE,
                    second.handle.as_raw_handle() as HANDLE,
                    &mut member,
                )
            };
            assert!(confirmed != 0 && member != 0);
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}
