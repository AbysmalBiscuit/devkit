//! A server `devkit run up` launches lives on its own: `up` returns while it
//! keeps running, and `down` ends it. Drives `devkit run` directly with an
//! isolated HOME/XDG_STATE_HOME so the port registry never touches the real
//! one. The server is this test binary, relaunched to run
//! `serve_port_when_launched_as_an_app`.

use std::{
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::Path,
    process::{Command, Output},
    time::{Duration, Instant},
};

const SERVE_PORT_ENV: &str = "DEVKIT_TEST_SERVE_PORT";

/// Serves the port `DEVKIT_TEST_SERVE_PORT` names, for a minute, when this
/// test binary is launched as an app's server; a no-op otherwise.
#[test]
fn serve_port_when_launched_as_an_app() {
    let Ok(port) = std::env::var(SERVE_PORT_ENV) else {
        return;
    };
    let _listener = TcpListener::bind(("127.0.0.1", port.parse::<u16>().unwrap())).unwrap();
    std::thread::sleep(Duration::from_secs(60));
}

/// A git repository whose one app, `srv`, is this binary serving its port.
fn setup(base_port: u16) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    devkit_git::Git::fixture(dir.path())
        .args(["init", "-q"])
        .output()
        .unwrap();
    let exe = std::env::current_exe().unwrap();
    // A TOML literal string: a Windows path's backslashes are not escapes.
    std::fs::write(
        dir.path().join("devkit.toml"),
        format!(
            "[apps.srv]\nbase_port = {base_port}\npath = \".\"\n\
             launch = ['{}', '--exact', 'serve_port_when_launched_as_an_app', '--nocapture']\n\
             static_env = {{ {SERVE_PORT_ENV} = \"{{{{ port }}}}\" }}\n",
            exe.display()
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
        .env("LOCALAPPDATA", &state)
        .env("USERPROFILE", dir)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .output()
        .unwrap()
}

fn assert_ok(out: &Output) {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn accepts(port: u16) -> bool {
    TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        Duration::from_millis(500),
    )
    .is_ok()
}

#[test]
fn up_returns_while_its_server_keeps_running() {
    let dir = setup(39361);
    let start = Instant::now();
    let up = run(dir.path(), &["up", "srv"]);
    let took = start.elapsed();
    assert!(
        took < Duration::from_secs(30),
        "up held its caller for {took:?}"
    );
    assert_ok(&up);
    assert!(accepts(39361), "the server is up");
    assert_ok(&run(dir.path(), &["down", "--app", "srv"]));
}

#[test]
fn down_ends_the_server_up_launched() {
    let dir = setup(39371);
    assert_ok(&run(dir.path(), &["up", "srv"]));
    assert!(accepts(39371), "the server is up");
    assert_ok(&run(dir.path(), &["down", "--app", "srv"]));
    let deadline = Instant::now() + Duration::from_secs(10);
    while accepts(39371) {
        assert!(Instant::now() < deadline, "the server outlived down");
        std::thread::sleep(Duration::from_millis(100));
    }
}
