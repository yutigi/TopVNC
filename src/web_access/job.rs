//! Windows Job Objects that end supervised children when TopVNC exits,
//! including when it crashes or is killed: Windows closes the job's last
//! handle with the process, and `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` then
//! terminates everything in the job.
//!
//! A job belongs to the session of the first process put in it, and a
//! process in another session cannot join it later. Children that may run
//! in different sessions, like the host service's agents, each get their own
//! [`Job`]; [`adopt`] uses one job shared by the whole process, for
//! children in its own session.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};

/// A job that terminates its processes when it is dropped or when TopVNC
/// exits.
pub struct Job(OwnedHandle);

impl Job {
    pub fn new() -> Result<Self, String> {
        // SAFETY: null attributes and name create an unnamed job with default
        // security; the result is checked.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(format!(
                "could not create a job object: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: a new handle owned from here on, closed on every path.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(job) });
        // SAFETY: a zeroed structure is a valid "no limits" value.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the pointer and size describe `limits`, which outlives the call.
        let set = unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if set == 0 {
            return Err(format!(
                "could not configure the job object: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(job)
    }

    /// Put `process` in this job.
    pub fn assign(&self, process: HANDLE) -> Result<(), String> {
        // SAFETY: both handles are valid for the duration of the call.
        let assigned = unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), process) };
        if assigned == 0 {
            return Err(format!(
                "could not tie the child process to TopVNC: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
}

/// Put `child` in TopVNC's job. A child that cannot be adopted is not
/// started, so it can never outlive TopVNC.
pub fn adopt(child: &std::process::Child) -> Result<(), String> {
    // Never dropped: the job must live as long as the process.
    static JOB: OnceLock<Result<Job, String>> = OnceLock::new();
    JOB.get_or_init(Job::new)
        .as_ref()
        .map_err(Clone::clone)?
        .assign(child.as_raw_handle())
}
