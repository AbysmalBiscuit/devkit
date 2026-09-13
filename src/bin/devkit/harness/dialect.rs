//! Which shell's syntax a hook command is read in.

#![allow(dead_code)]

use devkit_command::Dialect;
use devkit_common::harness::Harness;
use devkit_config::ShellSetting;

/// Resolve the dialect from what the payload and the platform establish.
///
/// The hook process's own environment is deliberately not an input: on
/// Claude Code's Windows `PowerShell` tool the hook runs under Git Bash, so
/// `SHELL` and `MSYSTEM` describe devkit's process, not the command's.
pub fn resolve(
    setting: ShellSetting,
    harness: Harness,
    tool_name: Option<&str>,
    windows: bool,
) -> Dialect {
    match setting {
        ShellSetting::Bash => Dialect::Bash,
        ShellSetting::Powershell => Dialect::PowerShell,
        ShellSetting::Auto => match (tool_name, harness) {
            (Some("PowerShell"), _) => Dialect::PowerShell,
            (_, Harness::Codex) if windows => Dialect::PowerShell,
            (_, Harness::ClaudeCode | Harness::Codex | Harness::Cursor) => Dialect::Bash,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_setting_wins() {
        assert_eq!(
            resolve(ShellSetting::Bash, Harness::Codex, Some("PowerShell"), true),
            Dialect::Bash
        );
        assert_eq!(
            resolve(
                ShellSetting::Powershell,
                Harness::ClaudeCode,
                Some("Bash"),
                false
            ),
            Dialect::PowerShell
        );
    }

    #[test]
    fn auto_follows_tool_name_then_harness_and_platform() {
        assert_eq!(
            resolve(
                ShellSetting::Auto,
                Harness::ClaudeCode,
                Some("PowerShell"),
                true
            ),
            Dialect::PowerShell
        );
        assert_eq!(
            resolve(ShellSetting::Auto, Harness::Codex, Some("Bash"), true),
            Dialect::PowerShell
        );
        assert_eq!(
            resolve(ShellSetting::Auto, Harness::Codex, Some("Bash"), false),
            Dialect::Bash
        );
        assert_eq!(
            resolve(ShellSetting::Auto, Harness::ClaudeCode, Some("Bash"), true),
            Dialect::Bash
        );
    }
}
