//! Windows process-tree containment shared by agent and environment workers.

#[cfg(windows)]
use std::mem::size_of;
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(windows)]
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
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

        let assigned = unsafe { AssignProcessToJobObject(job, process as HANDLE) } != 0;
        if !assigned {
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
