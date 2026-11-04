//! End-to-end: `DEVKIT_HYPERLINKS` drives `ui::link`'s output regardless of
//! terminal detection. Tests run without a TTY, so detection alone yields plain
//! text.

use devkit_common::ui;
use devkit_git::child_env::child_env;

#[test]
fn devkit_hyperlinks_always_forces_osc8_on() {
    child_env().set("DEVKIT_HYPERLINKS", "always").run(|| {
        assert_eq!(
            ui::link("PR #1", "https://x"),
            "\x1b]8;;https://x\x1b\\PR #1\x1b]8;;\x1b\\",
            "always must emit OSC 8 even off-TTY"
        );
    });
}

#[test]
fn devkit_hyperlinks_never_forces_osc8_off() {
    child_env().set("DEVKIT_HYPERLINKS", "never").run(|| {
        assert_eq!(
            ui::link("PR #1", "https://x"),
            "PR #1",
            "never must suppress OSC 8"
        );
    });
}
