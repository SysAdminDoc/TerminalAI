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

#[cfg(windows)]
impl ProcessJob {
    pub(crate) fn assign(process: RawHandle) -> Result<Self, String> {
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }

        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
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
