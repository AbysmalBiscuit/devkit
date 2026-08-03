use devkit_docs::names;

#[test]
fn encodes_slashes_and_round_trips() {
    assert_eq!(
        names::encode("@hey-api/client-fetch"),
        "@hey-api~client-fetch"
    );
    assert_eq!(
        names::decode("@hey-api~client-fetch"),
        "@hey-api/client-fetch"
    );
    assert_eq!(names::encode("release/2.x"), "release~2.x");
}

#[test]
fn lib_names_reject_tilde_so_encoding_is_injective() {
    assert!(names::validate_lib("a~b").is_err());
    assert!(names::validate_lib("a/b").is_ok());
    assert_eq!(names::lib_dir("a/b").unwrap(), "a~b");
}

#[test]
fn lib_names_reject_the_registry_stem_however_it_is_cased() {
    for name in [
        "registry",
        "registry.json",
        "registry.lock",
        "registry.json.tmp",
        "registry.json.bak",
        "registry.anything-added-later",
        "REGISTRY.JSON",
        "Registry.Locks",
    ] {
        assert!(
            names::validate_lib(name).is_err(),
            "{name} must be reserved"
        );
    }
    assert!(names::validate_lib("registryfoo").is_ok());
}

#[test]
fn names_that_would_escape_the_cache_root_are_rejected() {
    for name in ["..", ".", "../../etc", "a/../../b"] {
        assert!(
            names::validate_lib(name).is_err(),
            "{name} must not traverse"
        );
        assert!(
            names::validate_ref(name).is_err(),
            "{name} must not traverse"
        );
    }
}

#[test]
fn a_branch_ref_containing_a_slash_encodes_rather_than_erroring() {
    assert_eq!(names::checkout_dir("release/2.x").unwrap(), "release~2.x");
    assert_eq!(
        names::checkout_dir("refs/tags/v1.0.0").unwrap(),
        "refs~tags~v1.0.0"
    );
    assert_eq!(
        names::lib_dir("@hey-api/client-fetch").unwrap(),
        "@hey-api~client-fetch"
    );
}

#[test]
fn checkout_names_reject_control_files_but_not_the_registry_stem() {
    assert!(names::validate_ref("repo.git").is_err());
    assert!(names::validate_ref("meta.toml").is_err());
    assert!(names::validate_ref("registry.json").is_ok());
}

#[test]
fn rejects_names_the_host_filesystem_cannot_represent() {
    assert!(names::validate_ref(&"v".repeat(256)).is_err());
    if cfg!(windows) {
        for name in [
            "a|b", "a<b", "a>b", "a\"b", "NUL", "con", "COM1", "LPT9.txt",
        ] {
            assert!(
                names::validate_ref(name).is_err(),
                "{name} must be rejected"
            );
        }
    }
}

#[test]
fn tilde_is_illegal_in_a_git_ref_so_checkout_encoding_is_injective() {
    assert!(names::validate_ref("release~2.x").is_err());
}

#[test]
fn case_folding_keys_let_a_caller_spot_a_host_collision() {
    assert_eq!(names::fold_key("V1.0"), names::fold_key("v1.0"));
    assert_ne!(names::fold_key("v1.0"), names::fold_key("v1.1"));
}

#[test]
fn a_manifest_holding_both_a_slash_b_and_a_tilde_b_is_rejected_on_load() {
    let dir = std::env::temp_dir().join(format!("docm-names-load-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let global = dir.join("docs.toml");
    std::fs::write(
        &global,
        "[[libs]]\nname = \"a~b\"\n\n[[libs]]\nname = \"a/b\"\n",
    )
    .unwrap();

    let err = devkit_docs::manifest::discover(&dir, Some(&global)).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("a~b"), "error must name both entries: {msg}");
    assert!(msg.contains("a/b"), "error must name both entries: {msg}");
}
