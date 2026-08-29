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

    /// Lines this shell only accepts at the top of a file, which its generator
    /// repeats at the head of every script it writes. Concatenating scripts
    /// has to lift these out and emit them once.
    ///
    /// PowerShell rejects a `using` statement that follows any other statement,
    /// so without this a combined file fails to parse from the second script
    /// on. No other generator writes a file-level prologue.
    fn prologue_prefix(self) -> Option<&'static [u8]> {
        match self {
            Self::PowerShell => Some(b"using namespace "),
            Self::Bash | Self::Elvish | Self::Fish | Self::Nushell | Self::Zsh => None,
        }
    }
}

/// Move every `prefix` line out of `script` into `prologue`, keeping the first
/// occurrence of each and returning what remains.
fn take_prologue(script: Vec<u8>, prefix: &[u8], prologue: &mut Vec<Vec<u8>>) -> Vec<u8> {
    let mut body = Vec::with_capacity(script.len());
    for line in script.split_inclusive(|b| *b == b'\n') {
        if line.starts_with(prefix) {
            if !prologue.iter().any(|held| held == line) {
                prologue.push(line.to_vec());
            }
        } else {
            body.extend_from_slice(line);
        }
    }
    body
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
/// in order, under one lock so a multi-script run cannot interleave. Any
/// file-level prologue `shell` demands is emitted once, ahead of every body.
///
/// `clap_complete::generate` panics when the write fails, which turns a reader
/// that closes the pipe early (`... | head`, `... | grep -q`) into a crash.
/// This runs the same sequence against the fallible generator and treats a
/// broken pipe as the reader being done, not as a failure.
pub fn emit(
    shell: Shell,
    scripts: impl IntoIterator<Item = (Command, &'static str)>,
) -> Result<(), Error> {
    let prefix = shell.prologue_prefix();
    let mut prologue = Vec::new();
    let mut bodies = Vec::new();
    for (mut cmd, bin_name) in scripts {
        cmd.set_bin_name(bin_name);
        cmd.build();
        let mut script = Vec::new();
        shell.try_generate(&cmd, &mut script)?;
        bodies.push(match prefix {
            Some(prefix) => take_prologue(script, prefix, &mut prologue),
            None => script,
        });
    }
    let mut out = std::io::stdout().lock();
    match prologue
        .iter()
        .chain(bodies.iter())
        .try_for_each(|chunk| out.write_all(chunk))
    {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => other,
    }
}
