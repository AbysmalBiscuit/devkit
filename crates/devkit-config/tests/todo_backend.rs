use devkit_config::Config;

#[test]
fn the_removed_backend_names_taskchampion() {
    let error = Config::parse("[todo]\nbackend = \"taskwarrior\"\n").unwrap_err();
    assert!(format!("{error:#}").contains("taskchampion"), "{error:#}");
}
