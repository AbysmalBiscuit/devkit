//! Which shell's syntax a hook command is read in.

use devkit_command::Dialect;
use devkit_config::ShellSetting;
use pabal::{AnyHarness, ShellKind};

/// Resolve the dialect from what the payload and the platform establish.
///
/// The hook process's own environment is deliberately not an input: on
/// Claude Code's Windows `PowerShell` tool the hook runs under Git Bash, so
/// `SHELL` and `MSYSTEM` describe devkit's process, not the command's.
pub fn resolve(
    setting: ShellSetting,
    harness: AnyHarness,
    shell: Option<&ShellKind>,
    windows: bool,
) -> Dialect {
    match setting {
        ShellSetting::Bash => Dialect::Bash,
        ShellSetting::Powershell => Dialect::PowerShell,
        ShellSetting::Auto => match (shell, harness) {
            (Some(ShellKind::PowerShell), _) => Dialect::PowerShell,
            // Codex names every shell tool `Bash` and Antigravity names its
            // `run_command`, so only the platform says which shell ran it.
            (_, AnyHarness::Codex | AnyHarness::Antigravity) if windows => Dialect::PowerShell,
            (
                _,
                AnyHarness::ClaudeCode
                | AnyHarness::Codex
                | AnyHarness::Cursor
                | AnyHarness::Antigravity,
            ) => Dialect::Bash,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_setting_wins() {
        assert_eq!(
            resolve(
                ShellSetting::Bash,
                AnyHarness::Codex,
                Some(&ShellKind::PowerShell),
                true
            ),
            Dialect::Bash
        );
        assert_eq!(
            resolve(
                ShellSetting::Powershell,
                AnyHarness::ClaudeCode,
                Some(&ShellKind::Bash),
                false
            ),
            Dialect::PowerShell
        );
    }

    #[test]
    fn auto_follows_the_named_shell_then_harness_and_platform() {
        assert_eq!(
            resolve(
                ShellSetting::Auto,
                AnyHarness::ClaudeCode,
                Some(&ShellKind::PowerShell),
                true
            ),
            Dialect::PowerShell
        );
        assert_eq!(
            resolve(ShellSetting::Auto, AnyHarness::Codex, None, true),
            Dialect::PowerShell
        );
        assert_eq!(
            resolve(ShellSetting::Auto, AnyHarness::Codex, None, false),
            Dialect::Bash
        );
        assert_eq!(
            resolve(
                ShellSetting::Auto,
                AnyHarness::ClaudeCode,
                Some(&ShellKind::Bash),
                true
            ),
            Dialect::Bash
        );
    }
}
