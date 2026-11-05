//! A Windows Job Object that ends every supervised child when TopVNC exits,
//! including when it crashes or is killed: Windows closes the job's last
//! handle with the process, and `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` then
//! terminates everything in the job.

use std::os::windows::io::AsRawHandle;
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};

struct Job(HANDLE);

// The handle is only passed to thread-safe Win32 calls and never closed:
// it must live as long as the process.
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

fn create() -> Result<Job, String> {
    // SAFETY: null attributes and name create an unnamed job with default
    // security; the result is checked.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return Err(format!(
            "could not create a job object: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: a zeroed structure is a valid "no limits" value.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: the pointer and size describe `limits`, which outlives the call.
    let set = unsafe {
        SetInformationJobObject(
            job,
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
    Ok(Job(job))
}

/// Put `child` in TopVNC's job. A child that cannot be adopted is not
/// started, so it can never outlive TopVNC.
pub fn adopt(child: &std::process::Child) -> Result<(), String> {
    adopt_handle(child.as_raw_handle())
}

/// Like [`adopt`], for a process started without `std::process`.
pub fn adopt_handle(process: HANDLE) -> Result<(), String> {
    static JOB: OnceLock<Result<Job, String>> = OnceLock::new();
    let job = JOB.get_or_init(create).as_ref().map_err(Clone::clone)?;
    // SAFETY: both handles are valid for the duration of the call.
    let assigned = unsafe { AssignProcessToJobObject(job.0, process) };
    if assigned == 0 {
        return Err(format!(
            "could not tie the child process to TopVNC: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}
