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
