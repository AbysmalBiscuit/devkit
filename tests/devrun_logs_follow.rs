//! `devrun logs --follow` ends the way its `tail -f` ended, driven through
//! `devkit run` directly against a seeded port registry.
#![cfg(unix)]

use std::{os::unix::process::ExitStatusExt, process::Command};

#[test]
fn a_follow_whose_tail_a_signal_kills_gives_that_signal_status() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let state = home.path().join("state");
    let devkit_dir = state.join("devkit");
    std::fs::create_dir_all(devkit_dir.join("logs")).unwrap();
    let wt = home.path().join("wt");
    std::fs::create_dir_all(&wt).unwrap();
    devkit_git::Git::fixture(&wt)
        .args(["init", "-q"])
        .output()
        .unwrap();
    let wt = wt.canonicalize().unwrap();
    let log = devkit_dir.join("logs/api.log");
    std::fs::write(&log, "").unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(
        devkit_dir.join("ports.json"),
        format!(
            r#"{{"version":1,"entries":{{"9100":{{"app":"api","holder":{:?},"role":"issue","pid":null,"logfile":{:?},"ts":{now}}}}}}}"#,
            wt.to_string_lossy(),
            log.to_string_lossy(),
        ),
    )
    .unwrap();
    let bin = home.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let tail = bin.join("tail");
    std::fs::write(&tail, "#!/bin/sh\nkill -TERM $$\n").unwrap();
    std::fs::set_permissions(&tail, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());

    let out = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .arg("run")
        .arg("-C")
        .arg(&wt)
        .args(["logs", "api", "--follow"])
        .env("HOME", home.path())
        .env("XDG_STATE_HOME", &state)
        .env("PATH", path)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();

    assert_eq!(out.status.signal(), Some(15), "{out:?}");
}
