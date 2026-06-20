//! Unix implementations of the primitives declared in `super`.

pub(super) fn process_alive(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    // Pids that do not fit in a positive i32 are invalid on Linux/macOS.
    let Ok(signed) = i32::try_from(pid) else {
        return false;
    };
    if signed <= 0 {
        return false;
    }
    kill(Pid::from_raw(signed), None).is_ok()
}

pub(super) fn terminate(pid: u32) {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
}

pub(super) fn detach(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // Start a new session so the child outlives the launching shell and is
    // insulated from its controlling terminal's signals.
    // SAFETY: setsid only mutates the child after fork; it is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| nix::unistd::setsid().map(|_| ()).map_err(|e| e.into()));
    }
}
