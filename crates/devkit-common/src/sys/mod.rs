//! Platform abstraction boundary. Every OS-specific primitive lives behind this
//! module so the rest of the workspace stays platform-agnostic. The `unix`
//! implementation is the only backend today; a `windows` backend is added later.

#[cfg(unix)]
#[path = "unix.rs"]
mod imp;

/// True if a process with `pid` currently exists.
pub fn process_alive(pid: u32) -> bool {
    imp::process_alive(pid)
}

/// Ask `pid` to terminate, gracefully where the platform supports it.
pub fn terminate(pid: u32) {
    imp::terminate(pid)
}

/// Configure `cmd` to start detached from the caller's session/process group.
/// Must be called before `spawn`.
pub fn detach(cmd: &mut std::process::Command) {
    imp::detach(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_is_alive() {
        assert!(process_alive(std::process::id()));
    }

    #[test]
    fn pid_zero_is_not_alive() {
        assert!(!process_alive(0));
    }
}
