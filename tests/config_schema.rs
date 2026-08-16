//! The committed `schema/devkit-config.json` must match what `devkit schema`
//! generates. A stale schema is worse than none: editors would report valid
//! config as invalid, and miss the keys it does not know about.

use std::process::Command;

fn generated() -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .arg("schema")
        .output()
        .unwrap();
    assert!(out.status.success(), "`devkit schema` failed");
    String::from_utf8(out.stdout).unwrap()
}

fn committed() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/schema/devkit-config.json"
    ))
    .unwrap()
}

#[test]
fn the_committed_schema_matches_the_config_types() {
    assert_eq!(
        committed(),
        generated(),
        "schema/devkit-config.json is stale — regenerate with \
         `cargo run --bin devkit -- schema > schema/devkit-config.json`"
    );
}

#[test]
fn the_schema_states_what_resolution_actually_requires() {
    let s: serde_json::Value = serde_json::from_str(&generated()).unwrap();

    // An `[apps.x]` entry missing either key is the breakage users hit first:
    // the merged config stops deserializing and every devkit binary goes quiet
    // about why.
    let app = &s["$defs"]["AppConfig"]["required"];
    assert_eq!(app.as_array().unwrap(), &["base_port", "launch"]);

    // Nothing is required at the top level, because a layer file carries a
    // subset — this repository's own devkit.toml is `[harness]` alone. Editors
    // validate one file, never the merged stack.
    assert!(
        s.get("required").is_none(),
        "a partial layer file must not be an error"
    );

    // Doc comments are the hover text; losing them would leave a schema that
    // validates but teaches nothing.
    assert!(
        s["$defs"]["BriefConfig"]["properties"]["locks"]["description"]
            .as_str()
            .unwrap()
            .contains("lockm"),
    );

    // Every table a devkit.toml may carry, including the three read outside
    // `Config` by their own crates.
    let props = s["properties"].as_object().unwrap();
    for table in [
        "defaults",
        "apps",
        "people",
        "daemon",
        "linear",
        "templates",
        "tasks",
        "brief",
        "config",
        "docs",
        "harness",
    ] {
        assert!(props.contains_key(table), "missing table: {table}");
    }
}
