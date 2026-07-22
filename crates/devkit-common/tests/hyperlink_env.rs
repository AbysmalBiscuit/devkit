use devkit_common::ui;

/// End-to-end: `DEVKIT_HYPERLINKS` drives `ui::link`'s output regardless of
/// terminal detection. Tests run without a TTY, so detection alone yields plain
/// text — proving `always` forces the OSC 8 sequence on and `never` forces it
/// off. Kept as one sequential test because the env var is process-global;
/// parallel cases would race on it.
#[test]
fn devkit_hyperlinks_env_overrides_detection() {
    unsafe { std::env::set_var("DEVKIT_HYPERLINKS", "always") };
    assert_eq!(
        ui::link("PR #1", "https://x"),
        "\x1b]8;;https://x\x1b\\PR #1\x1b]8;;\x1b\\",
        "always must emit OSC 8 even off-TTY"
    );

    unsafe { std::env::set_var("DEVKIT_HYPERLINKS", "never") };
    assert_eq!(
        ui::link("PR #1", "https://x"),
        "PR #1",
        "never must suppress OSC 8"
    );

    unsafe { std::env::remove_var("DEVKIT_HYPERLINKS") };
}
