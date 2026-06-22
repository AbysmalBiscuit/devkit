//! `devkitd install-service`: writes a `systemd --user` unit with `Delegate=yes`
//! so a systemd-launched daemon lands in a delegated cgroup-v2 subtree (no sudo).
//! Linux + systemd only; other platforms reject the subcommand.

use anyhow::Result;

/// The `systemd --user` unit text for `devkitd`. `Restart=on-failure` lets a clean
/// idle-exit (exit 0) stay down rather than being fought by systemd.
// Only called from the Linux install() path; test-only usage does not suppress dead_code.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn unit_file_contents(exec_path: &str) -> String {
    format!(
        "[Unit]\n\
         Description=devkit supervisor daemon\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exec_path}\n\
         Delegate=yes\n\
         Restart=on-failure\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

#[cfg(target_os = "linux")]
pub(crate) fn install() -> Result<()> {
    use anyhow::Context as _;
    let exe = std::env::current_exe().context("resolving current devkitd path")?;
    let unit = devkit_common::paths::systemd_user_unit();
    std::fs::create_dir_all(unit.parent().unwrap())
        .with_context(|| format!("creating {}", unit.parent().unwrap().display()))?;
    std::fs::write(&unit, unit_file_contents(&exe.to_string_lossy()))
        .with_context(|| format!("writing {}", unit.display()))?;
    run_systemctl(&["daemon-reload"])?;
    // Stop any running ad-hoc daemon so the systemd-launched one can take the lock.
    // Shutdown is best-effort: there may be no daemon running.
    let _ = devkit_ports::daemon::client::try_existing()
        .map(|mut c| {
            c.request::<devkit_ports::daemon::proto::Request, devkit_ports::daemon::proto::Response>(
                &devkit_ports::daemon::proto::Request::Shutdown,
            )
        });
    run_systemctl(&["enable", "--now", "devkitd.service"])?;
    println!("Installed devkitd as a systemd user service (Delegate=yes).");
    println!("Verify:  systemctl --user status devkitd");
    println!("For headless persistence:  loginctl enable-linger \"$USER\"  (may require privilege)");
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn uninstall() -> Result<()> {
    let _ = run_systemctl(&["disable", "--now", "devkitd.service"]);
    let unit = devkit_common::paths::systemd_user_unit();
    let _ = std::fs::remove_file(&unit);
    run_systemctl(&["daemon-reload"])?;
    println!("Removed the devkitd systemd user service.");
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_systemctl(args: &[&str]) -> Result<()> {
    use anyhow::Context as _;
    let status = std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .with_context(|| format!("running systemctl --user {}", args.join(" ")))?;
    anyhow::ensure!(status.success(), "systemctl --user {} failed", args.join(" "));
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn install() -> Result<()> {
    anyhow::bail!("install-service requires Linux with systemd --user")
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn uninstall() -> Result<()> {
    anyhow::bail!("uninstall-service requires Linux with systemd --user")
}

#[cfg(test)]
mod tests {
    use super::unit_file_contents;

    #[test]
    fn unit_has_delegate_and_exec_and_restart() {
        let u = unit_file_contents("/home/exampleuser/.cargo/bin/devkitd");
        assert!(u.contains("ExecStart=/home/exampleuser/.cargo/bin/devkitd"));
        assert!(u.contains("Delegate=yes"));
        assert!(u.contains("Restart=on-failure"));
        assert!(u.contains("WantedBy=default.target"));
    }
}
