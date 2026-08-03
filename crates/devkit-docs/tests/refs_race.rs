use devkit_docs::refs::RefStore;
use std::process::Command;

#[test]
fn concurrent_records_never_lose_rows() {
    // The test binary re-execs itself as the worker via an env switch.
    if let Ok(project) = std::env::var("DEVKIT_DOCS_TEST_RECORD") {
        let store = RefStore::at(&devkit_docs::cache::docs_root());
        store
            .commit(|d| {
                d.record(&project, "tokio", "1.0.0", "v1.0.0", "aaa");
                Ok(())
            })
            .unwrap();
        std::process::exit(0);
    }

    let tmp = std::env::temp_dir().join(format!("devkit-docs-race-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let exe = std::env::current_exe().unwrap();

    let mut kids = Vec::new();
    for i in 0..16 {
        let project = tmp.join(format!("p{i}"));
        std::fs::create_dir_all(&project).unwrap();
        kids.push(
            Command::new(&exe)
                // Pin both cache-home inputs so all workers flock the same
                // isolated registry: cache_dir() prefers $XDG_CACHE_HOME and
                // falls back to $HOME/.cache.
                .env("HOME", &tmp)
                .env("XDG_CACHE_HOME", &tmp)
                .env("XDG_DATA_HOME", &tmp)
                .env("DEVKIT_DOCS_TEST_RECORD", &project)
                .args([
                    "--exact",
                    "concurrent_records_never_lose_rows",
                    "--nocapture",
                ])
                .output()
                .unwrap(),
        );
    }
    for k in &kids {
        assert!(
            k.status.success(),
            "worker failed: {}",
            String::from_utf8_lossy(&k.stderr)
        );
    }
    let data: devkit_docs::refs::Data = {
        let raw = std::fs::read_to_string(tmp.join("devkit/docs/registry.json")).unwrap();
        serde_json::from_str(&raw).unwrap()
    };
    assert_eq!(data.rows.len(), 16, "rows lost to a race: {:?}", data.rows);
}
