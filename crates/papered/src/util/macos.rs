//! macOS system integration helpers.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Maximum time to wait for a `defaults read` subprocess.
///
/// `defaults` talks to the `cfprefsd` daemon, which can hang indefinitely for
/// a corrupted or locked preferences domain. A plain blocking `.output()`
/// would then freeze the caller forever — this froze daemon startup on the
/// main thread (the process sat in `poll` waiting on the child's output pipe).
/// We instead poll with `try_wait` and kill the child once the deadline
/// passes, so a stuck domain degrades to "value not found" rather than a hang.
const DEFAULTS_READ_TIMEOUT: Duration = Duration::from_secs(3);

/// Read a value from macOS `defaults` system.
pub(crate) fn read_defaults(domain: &str, key: &str) -> Option<String> {
    let mut child = Command::new("defaults")
        .args(["read", domain, key])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let deadline = Instant::now() + DEFAULTS_READ_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    tracing::warn!(
                        "defaults read {domain} {key} timed out after {DEFAULTS_READ_TIMEOUT:?}; killing it"
                    );
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => return None,
        }
    }

    let mut stdout = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Discover a port from macOS defaults, falling back to env var then default.
pub fn discover_port(domain: &str, key: &str, env_var: &str, default: u16) -> u16 {
    if let Some(port_str) = read_defaults(domain, key)
        && let Ok(port) = port_str.parse::<u16>()
    {
        return port;
    }
    if let Ok(port) = std::env::var(env_var)
        && let Ok(p) = port.parse::<u16>()
    {
        return p;
    }
    default
}
