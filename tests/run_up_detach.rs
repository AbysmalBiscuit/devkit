//! A server `devkit run up` launches is detached from the caller's terminal:
//! it leads a session and a process group of its own, so the caller's Ctrl-C
//! and hangups miss it and a `/dev/tty` read fails instead of stopping it.
//! The server is this test binary, relaunched to run
//! `record_ids_when_launched_as_an_app`.
#![cfg(unix)]

use std::{
    net::TcpListener,
    path::Path,
    process::{Command, Output},
    time::{Duration, Instant},
};

use nix::unistd::{Pid, getpgid, getpgrp, getsid};

const SERVE_PORT_ENV: &str = "DEVKIT_TEST_SERVE_PORT";
const IDS_OUT_ENV: &str = "DEVKIT_TEST_IDS_OUT";

/// Writes `<pid> <process group> <session>` to the file
/// `DEVKIT_TEST_IDS_OUT` names, then serves `DEVKIT_TEST_SERVE_PORT` for a
/// minute, when this test binary is launched as an app's server; a no-op
/// otherwise.
#[test]
fn record_ids_when_launched_as_an_app() {
    let (Ok(port), Ok(out)) = (std::env::var(SERVE_PORT_ENV), std::env::var(IDS_OUT_ENV)) else {
        return;
    };
    let pid = Pid::this();
    let ids = format!(
        "{} {} {}",
        pid,
        getpgid(None).unwrap(),
        getsid(None).unwrap()
    );
    std::fs::write(out, ids).unwrap();
    let _listener = TcpListener::bind(("127.0.0.1", port.parse::<u16>().unwrap())).unwrap();
    std::thread::sleep(Duration::from_secs(60));
}

/// A git repository whose one app, `srv`, runs `launch` on `base_port`.
fn setup(base_port: u16, launch: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    devkit_git::Git::fixture(dir.path())
        .args(["init", "-q"])
        .output()
        .unwrap();
    std::fs::write(
        dir.path().join("devkit.toml"),
        format!(
            "[apps.srv]\nbase_port = {base_port}\npath = \".\"\nlaunch = {launch}\n\
             static_env = {{ {SERVE_PORT_ENV} = \"{{{{ port }}}}\", {IDS_OUT_ENV} = '{}' }}\n",
            dir.path().join("ids").display()
        ),
    )
    .unwrap();
    dir
}

/// `devkit run ARGS` in `dir`, its output read to the end.
fn run(dir: &Path, args: &[&str]) -> Output {
    let state = dir.join("state");
    Command::new(env!("CARGO_BIN_EXE_devkit"))
        .arg("run")
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("XDG_STATE_HOME", &state)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .output()
        .unwrap()
}

/// The `(pid, process group, session)` the launched server recorded.
fn launched_ids(base_port: u16) -> (Pid, Pid, Pid) {
    let launch = format!(
        "['{}', '--exact', 'record_ids_when_launched_as_an_app', '--nocapture']",
        std::env::current_exe().unwrap().display()
    );
    let dir = setup(base_port, &launch);
    let up = run(dir.path(), &["up", "srv"]);
    let ids_file = dir.path().join("ids");
    let deadline = Instant::now() + Duration::from_secs(30);
    let ids = loop {
        if let Ok(ids) = std::fs::read_to_string(&ids_file)
            && ids.split(' ').count() == 3
        {
            break ids;
        }
        assert!(
            Instant::now() < deadline,
            "the server recorded no ids: {}",
            String::from_utf8_lossy(&up.stderr)
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = run(dir.path(), &["down", "--app", "srv"]);
    let ids: Vec<Pid> = ids
        .split(' ')
        .map(|id| Pid::from_raw(id.trim().parse().unwrap()))
        .collect();
    (ids[0], ids[1], ids[2])
}

#[test]
fn a_detached_child_leads_its_own_session() {
    let (pid, _, session) = launched_ids(39381);
    assert_eq!(session, pid);
    assert_ne!(session, getsid(None).unwrap());
}

#[test]
fn a_detached_child_leads_its_own_process_group() {
    let (pid, group, _) = launched_ids(39391);
    assert_eq!(group, pid);
    assert_ne!(group, getpgrp());
}

#[test]
fn a_missing_program_fails_the_launch_with_its_name() {
    let dir = setup(39401, "['devkit-no-such-program-406']");
    let up = run(dir.path(), &["up", "srv"]);
    let stderr = String::from_utf8_lossy(&up.stderr);
    assert!(!up.status.success(), "up succeeded: {stderr}");
    assert!(
        stderr.contains("devkit-no-such-program-406"),
        "the error does not name the program: {stderr}"
    );
}

/// The program exists and is executable, but exec fails: its interpreter
/// does not exist. The launch fails with the program's name instead of
/// reporting a server that then exits.
#[test]
fn a_program_that_cannot_exec_fails_the_launch_with_its_name() {
    use std::os::unix::fs::PermissionsExt as _;
    let scripts = tempfile::tempdir().unwrap();
    let script = scripts.path().join("no-interpreter-406");
    std::fs::write(&script, "#!/nonexistent/interpreter\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let dir = setup(39411, &format!("['{}']", script.display()));
    let up = run(dir.path(), &["up", "srv"]);
    let stderr = String::from_utf8_lossy(&up.stderr);
    let _ = run(dir.path(), &["down", "--app", "srv"]);
    assert!(!up.status.success(), "up succeeded: {stderr}");
    assert!(
        stderr.contains("no-interpreter-406"),
        "the error does not name the program: {stderr}"
    );
}
