//! The `<bin> completions <shell>` argument every devkit CLI takes.

use std::io::{Error, Write};

use clap::{Command, ValueEnum};
use clap_complete::Generator;

/// A shell devkit can emit a completion script for.
///
/// `clap_complete::Shell` is closed and carries no nushell variant, so this is
/// the superset the CLIs accept; each variant forwards to whichever crate owns
/// that shell's generator. The value strings match `clap_complete::Shell`'s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Shell {
    /// Bourne Again SHell (bash)
    Bash,
    /// Elvish shell
    Elvish,
    /// Friendly Interactive SHell (fish)
    Fish,
    /// Nushell (nu)
    Nushell,
    /// PowerShell
    #[value(name = "powershell")]
    PowerShell,
    /// Z SHell (zsh)
    Zsh,
}

impl Shell {
    fn generator(self) -> &'static dyn Generator {
        match self {
            Self::Bash => &clap_complete::Shell::Bash,
            Self::Elvish => &clap_complete::Shell::Elvish,
            Self::Fish => &clap_complete::Shell::Fish,
            Self::Nushell => &clap_complete_nushell::Nushell,
            Self::PowerShell => &clap_complete::Shell::PowerShell,
            Self::Zsh => &clap_complete::Shell::Zsh,
        }
    }
}

impl Generator for Shell {
    fn file_name(&self, name: &str) -> String {
        self.generator().file_name(name)
    }

    fn generate(&self, cmd: &Command, buf: &mut dyn Write) {
        self.generator().generate(cmd, buf);
    }

    fn try_generate(&self, cmd: &Command, buf: &mut dyn Write) -> Result<(), Error> {
        self.generator().try_generate(cmd, buf)
    }
}

/// Write each `(command, bin_name)` completion script for `shell` to stdout,
/// in order, under one lock so a multi-script run cannot interleave.
///
/// `clap_complete::generate` panics when the write fails, which turns a reader
/// that closes the pipe early (`… | head`, `… | grep -q`) into a crash. This
/// runs the same sequence against the fallible generator and treats a broken
/// pipe as the reader being done, not as a failure.
pub fn emit(
    shell: Shell,
    scripts: impl IntoIterator<Item = (Command, &'static str)>,
) -> Result<(), Error> {
    let mut out = std::io::stdout().lock();
    for (mut cmd, bin_name) in scripts {
        cmd.set_bin_name(bin_name);
        cmd.build();
        if let Err(e) = shell.try_generate(&cmd, &mut out) {
            return match e.kind() {
                std::io::ErrorKind::BrokenPipe => Ok(()),
                _ => Err(e),
            };
        }
    }
    Ok(())
}
