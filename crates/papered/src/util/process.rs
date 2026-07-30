//! Cross-platform process-liveness checks used for daemon PID-file handling.

/// Returns `true` when a process with `pid` currently exists.
///
/// Used together with the daemon PID file
/// ([`crate::routes::daemon_pid_file`]) to distinguish a live daemon from a
/// stale PID file left behind by an ungraceful exit (`kill -9`, crash). As
/// with any PID-file scheme, a recycled PID belonging to an unrelated process
/// reads as "alive"; the database lock remains the final arbiter of daemon
/// exclusivity.
#[cfg(unix)]
pub fn process_alive(pid: u32) -> bool {
    // kill(pid, 0) performs error checking without sending a signal: 0 means
    // the process exists and we may signal it, EPERM means it exists but is
    // owned by another user, ESRCH means no such process.
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if ret == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Windows counterpart of [`process_alive`].
#[cfg(windows)]
pub fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut exit_code: u32 = 0;
        // A process object can outlive the process itself while handles
        // remain open, so check the exit code rather than handle validity.
        // STILL_ACTIVE is an NTSTATUS (i32); the exit code out-param is u32.
        let alive =
            GetExitCodeProcess(handle, &mut exit_code) != 0 && exit_code == STILL_ACTIVE as u32;
        CloseHandle(handle);
        alive
    }
}

/// Returns the PID of a currently-live daemon recorded in the PID file, or
/// `None` when no daemon is running (file missing, unparsable, or stale).
pub fn running_daemon_pid() -> Option<u32> {
    let content = std::fs::read_to_string(crate::routes::daemon_pid_file()).ok()?;
    let pid = content.trim().parse::<u32>().ok()?;
    process_alive(pid).then_some(pid)
}
