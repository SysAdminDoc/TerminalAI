//! Windows process-tree containment shared by agent and environment workers.
//!
//! An agent spawns compilers, test runners, package managers and language
//! servers. If the supervisor dies without containing them, they survive as
//! orphans holding the operator's ports, file locks and CPU — so every session
//! is created inside a job object with kill-on-close, and closing the handle is
//! what guarantees teardown even when this process is terminated rather than
//! asked to exit.
//!
//! # The gap between existing and being contained is 34.8 µs, and that is a
//! # decision rather than an oversight
//!
//! A process can only be *born* inside a job via
//! `PROC_THREAD_ATTRIBUTE_JOB_LIST` on the `CreateProcessW` attribute list, and
//! `portable-pty` 0.9 builds that list itself — `ProcThreadAttributeList::
//! with_capacity(1)`, one entry, hard-coded to the pseudoconsole — so nothing
//! short of forking the crate or reimplementing ConPTY can reach it. Both were
//! rejected: a forked pty crate is a permanent maintenance cost on the one code
//! path the whole fleet's teardown depends on.
//!
//! What *was* reachable is the length of the gap. The job is created and fully
//! configured before `spawn_command`, leaving exactly one
//! `AssignProcessToJobObject` between the process existing and it being
//! contained — measured 2026-08-04 at **34.8 µs**, down from three syscalls,
//! and pinned by `containment_costs_one_syscall_after_the_process_exists`.
//!
//! Membership is read back with `IsProcessInJob` rather than inferred from the
//! assignment's return value, because "the call returned success" and "this
//! process is in this job" are different claims and only the second one is the
//! thing teardown depends on.
//!
//! Residual exposure, stated rather than hidden: a grandchild spawned by the
//! agent inside those 34.8 µs escapes kill-on-close. No agent reaches its own
//! entry point that fast. Revisit if `portable-pty` ever exposes the attribute
//! list.

#[cfg(windows)]
use std::mem::size_of;
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(windows)]
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JobObjectBasicProcessIdList,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
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

/// What a session's whole process tree is currently holding.
///
/// The pair travels together deliberately. A byte figure with no process count
/// cannot be read: 900 MB across one process and 900 MB across a lead plus six
/// teammates are different situations, and the row has to be able to say which
/// one it is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobUsage {
    /// Private commit summed over every process in the job.
    pub private_bytes: u64,
    /// How many processes that figure covers.
    pub processes: u32,
}

/// Largest process list this will read back, so a runaway job cannot make the
/// sampler allocate without bound. Well past `ActiveProcessLimit` in any
/// configuration an operator would set, and a job over it is reported from the
/// first slice rather than not reported at all.
#[cfg(windows)]
const MAX_JOB_PROCESSES: usize = 1024;

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

    /// Every process currently in this job.
    ///
    /// `None` means the question could not be answered, never that the job is
    /// empty — the same rule `private_bytes` follows, and for the same reason.
    pub(crate) fn process_ids(&self) -> Option<Vec<u32>> {
        let job = self.handle.as_raw_handle() as HANDLE;
        // The struct is variable-length: a header plus one `usize` per process,
        // declared with a one-element array. Ask for room for a few, and grow
        // to the ceiling once if the job turns out to be larger.
        let mut capacity = 64usize;
        loop {
            let header = size_of::<JOBOBJECT_BASIC_PROCESS_ID_LIST>();
            let bytes = header + capacity.saturating_sub(1) * size_of::<usize>();
            let mut buffer = vec![0u8; bytes];
            let ok = unsafe {
                QueryInformationJobObject(
                    job,
                    JobObjectBasicProcessIdList,
                    buffer.as_mut_ptr().cast(),
                    bytes as u32,
                    std::ptr::null_mut(),
                )
            } != 0;
            // Safe: the buffer is at least one whole header, and every field
            // read below is inside it.
            let list = unsafe { &*(buffer.as_ptr() as *const JOBOBJECT_BASIC_PROCESS_ID_LIST) };
            let listed = list.NumberOfProcessIdsInList as usize;
            let assigned = list.NumberOfAssignedProcesses as usize;
            if !ok {
                // A partial answer is still an answer: the call fills what fits
                // and reports how many there were. Only a call that produced
                // nothing at all is a failure.
                if listed == 0 {
                    return None;
                }
            }
            if ok && assigned > listed && capacity < MAX_JOB_PROCESSES {
                capacity = (assigned.saturating_add(16)).min(MAX_JOB_PROCESSES);
                continue;
            }
            let listed = listed.min(capacity);
            let ids = unsafe { std::slice::from_raw_parts(list.ProcessIdList.as_ptr(), listed) };
            return Some(ids.iter().map(|pid| *pid as u32).collect());
        }
    }

    /// Private commit summed over the job, with the number of processes it
    /// covers.
    ///
    /// **Live, not peak.** `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` carries
    /// `PeakJobMemoryUsed` for one cheap call, and it was rejected: the cap this
    /// figure is compared against is enforced live, so a peak reading would mark
    /// a session limited forever after one spike and could never come back down
    /// — a row that cannot recover is worse than one that costs a handful of
    /// syscalls per sampling interval. The peak is still the right figure for a
    /// post-mortem, and this is not one.
    pub(crate) fn usage(&self) -> Option<JobUsage> {
        let ids = self.process_ids()?;
        let mut total = 0u64;
        let mut counted = 0u32;
        for pid in &ids {
            // A process that exited between the listing and the read is not a
            // failure — the job simply shrank. It is left out of both figures so
            // the count always describes what the bytes cover.
            if let Some(bytes) = private_bytes(*pid) {
                total = total.saturating_add(bytes);
                counted = counted.saturating_add(1);
            }
        }
        (counted > 0).then_some(JobUsage {
            private_bytes: total,
            processes: counted,
        })
    }

    /// Apply Windows' background execution policy to every process in the job.
    ///
    /// The whole job, not just the root: since agent teams, a supervised session
    /// can be a lead plus several separate agent instances, and demoting only
    /// the lead leaves every teammate at foreground priority while the operator
    /// is looking at something else. Errors are counted rather than returned one
    /// by one — a teammate that exited mid-walk is not a failure — and the call
    /// fails only when nothing could be reached at all.
    pub(crate) fn set_background(&self, background: bool) -> Result<(), String> {
        let Some(ids) = self.process_ids() else {
            return Err("could not list the job's processes".into());
        };
        let mut applied = 0usize;
        let mut last_error = None;
        for pid in ids {
            match set_background_priority(pid, background) {
                Ok(()) => applied += 1,
                Err(error) => last_error = Some(error),
            }
        }
        if applied > 0 {
            return Ok(());
        }
        Err(last_error.unwrap_or_else(|| "the job holds no reachable process".into()))
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

    /// Spawn a second process into the job and watch the single-process
    /// reading come up short.
    ///
    /// This is the shape the fleet actually runs: since agent teams, one
    /// supervised session is a lead plus separate agent instances, all inside
    /// the job the per-session cap is enforced over. `private_bytes(pid)` sees
    /// only the lead, so a team could be killed by its own
    /// `JOB_OBJECT_LIMIT_JOB_MEMORY` while the row read "not limited".
    #[test]
    fn a_jobs_memory_covers_every_process_in_it_not_just_the_one_being_supervised() {
        let job = ProcessJob::create(JobLimits::default()).expect("create job");
        let mut children: Vec<std::process::Child> = (0..2)
            .map(|_| {
                Command::new("cmd.exe")
                    .args(["/c", "pause"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("spawn child")
            })
            .collect();
        for child in &children {
            job.adopt(child.as_raw_handle()).expect("adopt child");
        }

        let usage = job.usage().expect("a job holding two live processes reports usage");
        assert_eq!(usage.processes, 2, "both processes are in the job");

        let lead = private_bytes(children[0].id()).expect("the lead is readable");
        let second = private_bytes(children[1].id()).expect("the teammate is readable");
        assert!(
            usage.private_bytes > lead,
            "the job total {} is not above the lead's own {lead}, so this is still              the single-process reading",
            usage.private_bytes
        );
        // Not an equality: both processes keep allocating between the two
        // readings. What must hold is that the job total accounts for both.
        assert!(
            usage.private_bytes >= lead.max(second),
            "job {} < the larger of {lead} and {second}",
            usage.private_bytes
        );

        for child in &mut children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    #[test]
    fn an_empty_job_reports_nothing_rather_than_zero_bytes() {
        // Zero is what a healthy idle session looks like, and "we could not
        // measure this" must never render as that.
        let job = ProcessJob::create(JobLimits::default()).expect("create job");
        assert_eq!(job.process_ids().expect("an empty list is still an answer").len(), 0);
        assert_eq!(job.usage(), None);
    }

    /// Every process in the job gets the background policy, not just the root.
    ///
    /// The failure this guards is silent by construction: demoting only the
    /// lead leaves teammates at foreground priority, and nothing about the row
    /// would look wrong.
    #[test]
    fn the_background_policy_reaches_every_process_in_the_job() {
        let job = ProcessJob::create(JobLimits::default()).expect("create job");
        let mut children: Vec<std::process::Child> = (0..2)
            .map(|_| {
                Command::new("cmd.exe")
                    .args(["/c", "pause"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("spawn child")
            })
            .collect();
        for child in &children {
            job.adopt(child.as_raw_handle()).expect("adopt child");
        }
        let ids = job.process_ids().expect("list");
        assert_eq!(ids.len(), 2);
        for child in &children {
            assert!(ids.contains(&child.id()), "{} is not in the job", child.id());
        }
        job.set_background(true).expect("demote the whole job");
        job.set_background(false).expect("restore the whole job");

        for child in &mut children {
            let _ = child.kill();
            let _ = child.wait();
        }
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
