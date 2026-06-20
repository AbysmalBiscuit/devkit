use devkit_ports::registry::{self, Role};
use std::process::Command;

fn main_like() {}

#[test]
fn concurrent_alloc_never_collides() {
    // Use this test binary itself as the worker via an env switch.
    if let Ok(holder) = std::env::var("DEVKIT_TEST_ALLOC") {
        let port =
            registry::with_lock(|d| Ok(d.alloc_one(&holder, "api", 9100, Role::Issue))).unwrap();
        print!("{port}");
        std::process::exit(0);
    }

    let tmp = std::env::temp_dir().join(format!("devkit-race-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let exe = std::env::current_exe().unwrap();

    let mut kids = Vec::new();
    for i in 0..16 {
        let holder = tmp.join(format!("w{i}")); // distinct, existing holder dirs
        std::fs::create_dir_all(&holder).unwrap();
        kids.push(
            Command::new(&exe)
                // Pin both state-home inputs into tmp so the worker's registry is
                // isolated: state_dir() prefers $XDG_STATE_HOME, then falls back to
                // $HOME. Setting only one leaks to the developer's real registry when
                // the other is present in the environment.
                .env("HOME", &tmp)
                .env("XDG_STATE_HOME", &tmp) // registry under tmp/devkit
                .env("DEVKIT_TEST_ALLOC", &holder)
                .args(["--exact", "concurrent_alloc_never_collides", "--nocapture"])
                .output()
                .unwrap(),
        );
    }
    let mut ports: Vec<String> = kids
        .into_iter()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    ports.sort();
    let n = ports.len();
    ports.dedup();
    assert_eq!(ports.len(), n, "ports collided: {ports:?}");
    assert_eq!(n, 16, "expected 16 allocations");
    let _ = main_like;
}
