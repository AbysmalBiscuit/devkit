use std::path::Path;
use std::process::Command;

fn git(args: &[&str], cwd: &Path) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("git runs")
        .success();
    assert!(ok, "git {args:?} failed");
}

#[test]
fn gather_local_returns_offline_rows_without_network() {
    let base = std::env::temp_dir().join(format!("devkit-gl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let main = base.join("main");
    std::fs::create_dir_all(&main).unwrap();

    git(&["init", "-q", "-b", "main"], &main);
    git(&["config", "user.email", "t@t"], &main);
    git(&["config", "user.name", "t"], &main);
    std::fs::write(main.join("f"), "x").unwrap();
    git(&["add", "."], &main);
    git(&["commit", "-qm", "init"], &main);

    let wt = base.join("eng-1-foo");
    git(
        &["worktree", "add", "-q", "-b", "lev/eng-1-bar", wt.to_str().unwrap()],
        &main,
    );

    let report = devkit_issue::status::gather_local(main.to_str().unwrap(), &[]).unwrap();
    let row = report
        .worktrees
        .iter()
        .find(|r| r.issue_id == "ENG-1")
        .expect("eng-1 row present");
    assert_eq!(row.pr_state, "NO_PR");
    assert_eq!(row.pr_number, None);
    assert_eq!(row.linear_kind, None);
    assert!(!row.dirty);

    let _ = std::fs::remove_dir_all(&base);
}
