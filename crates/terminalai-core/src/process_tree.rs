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
