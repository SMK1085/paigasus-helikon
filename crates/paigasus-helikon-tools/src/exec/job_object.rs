//! A Windows Job Object — the platform equivalent of the unix process-group
//! kill [`super::spawn_capped`] performs on timeout (SMA-613).
//!
//! Deliberately minimal: the job is created with **no limit flags**, so it kills
//! its members only when [`JobObject::terminate`] is called explicitly. In
//! particular `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` is *not* set, because that
//! would reap survivors of a normally-completed run — which unix does not do.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::ptr;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
};

/// Owns an anonymous job object handle.
///
/// The wrapped handle is always the **job** handle. The process handle passed to
/// [`JobObject::assign`] is borrowed from tokio's `Child` and is never closed
/// here — closing it would pull the rug from under tokio's own reaping.
///
/// `OwnedHandle` is `Send + Sync` and closes on drop, which is why this type
/// needs neither an `unsafe impl Send` (required, since `spawn_capped`'s future
/// is `Send`-bounded by `#[async_trait]`) nor a hand-written `Drop`.
pub(crate) struct JobObject(OwnedHandle);

impl JobObject {
    /// Create an anonymous job object and assign `process` to it.
    ///
    /// Note the ordering: the handle is wrapped in an `OwnedHandle` *before* the
    /// assignment is attempted, so a failing assign drops the wrapper and closes
    /// the handle rather than leaking it. That path is reachable — it is exactly
    /// the locked-down-runner case the caller degrades on.
    pub(crate) fn assign(process: RawHandle) -> io::Result<Self> {
        // SAFETY: a NULL `lpJobAttributes` selects default security and, more to
        // the point, leaves `bInheritHandle` FALSE — an inheritable job handle
        // would leak into every later spawn, since std spawns with
        // `bInheritHandles = TRUE`. A NULL `lpName` makes the job anonymous.
        let raw = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if raw.is_null() {
            // CreateJobObjectW reports failure as NULL, not INVALID_HANDLE_VALUE.
            return Err(io::Error::last_os_error());
        }

        // SAFETY: `raw` is a fresh, non-null handle we exclusively own, so
        // transferring ownership to `OwnedHandle` is sound.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(raw as RawHandle) });

        // SAFETY: both handles are valid — the job was just created, and
        // `process` is borrowed from a live tokio `Child`.
        if unsafe { AssignProcessToJobObject(raw, process as HANDLE) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    /// Terminate every process in the job. Returns `Err` if the call failed,
    /// which the caller must treat as "nothing was killed" and fall back.
    pub(crate) fn terminate(&self) -> io::Result<()> {
        // SAFETY: the handle is valid for the lifetime of `self`. The exit code
        // becomes every member's, which is harmless: the timeout path reports
        // `exit_code: None` regardless, per `ExecOutput::exit_code`.
        if unsafe { TerminateJobObject(self.0.as_raw_handle() as HANDLE, 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
