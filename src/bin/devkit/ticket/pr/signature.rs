//! The IO behind a `pr waive-verify --signed` check: paths under the account
//! home, verifier programs from the system directories, and a process
//! environment that the caller cannot set.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use devkit_common::cmd;
use devkit_config::{PrConfig, SignatureFormat, SignatureTools, SignatureVerifier};

/// The directories a verifier program comes from when the config names none.
/// `PATH` is the caller's, so it never picks the program.
#[cfg(unix)]
const SYSTEM_DIRS: &[&str] = &["/usr/bin", "/bin"];
#[cfg(windows)]
const SYSTEM_DIRS: &[&str] = &[r"C:\Windows\System32\OpenSSH"];

/// [`SignatureTools`] for the account whose home is `home`.
pub(crate) struct AccountTools {
    pub(crate) home: PathBuf,
}

impl SignatureTools for AccountTools {
    fn path(&self, configured: &str) -> Result<PathBuf> {
        match configured.strip_prefix("~/") {
            Some(rest) => Ok(self.home.join(rest)),
            None if Path::new(configured).is_absolute() => Ok(PathBuf::from(configured)),
            None => bail!(
                "`{configured}` is relative, so the caller's directory would pick it: write an \
                 absolute path or one that starts with `~/`"
            ),
        }
    }

    fn program(&self, tool: &str, configured: Option<&str>) -> Result<PathBuf> {
        if let Some(configured) = configured {
            if !Path::new(configured).is_absolute() {
                bail!("`program = \"{configured}\"` is relative: write an absolute path");
            }
            return Ok(PathBuf::from(configured));
        }
        let name = format!("{tool}{}", std::env::consts::EXE_SUFFIX);
        SYSTEM_DIRS
            .iter()
            .map(|dir| Path::new(dir).join(&name))
            .find(|path| path.is_file())
            .ok_or_else(|| {
                anyhow!(
                    "no `{name}` in {}: set `program` in `[pr.signed_waivers]` to its absolute \
                     path",
                    SYSTEM_DIRS.join(", ")
                )
            })
    }

    fn run(&self, program: &Path, args: &[&str], stdin: &[u8]) -> Result<String> {
        cmd::capture_isolated(program, args, stdin).map_err(|e| {
            anyhow!(
                "{}",
                cmd::failed_stderr(&e).map_or_else(|| format!("{e:#}"), str::to_string)
            )
        })
    }
}

/// The user config under the account `home` that holds `[pr.signed_waivers]`.
pub(crate) fn trust_config(home: &Path) -> PathBuf {
    home.join(".config/devkit/config.toml")
}

/// The verifier that `[pr.signed_waivers]` configures in [`trust_config`],
/// where `home` comes from the account database. Neither `$DEVKIT_CONFIG`,
/// `$HOME` nor a project layer moves it, so a caller cannot trust a key of
/// its own.
pub(crate) fn trusted_signers(home: &Path) -> Result<SignatureFormat> {
    let path = trust_config(home);
    let unset = || {
        anyhow!(
            "no trusted waiver signers: set `[pr.signed_waivers]` in `{}`",
            path.display()
        )
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(unset()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    PrConfig::from_layer(&text)
        .with_context(|| format!("reading `[pr]` in {}", path.display()))?
        .signed_waivers
        .ok_or_else(unset)
}

/// Every file that decides what a signed waiver verifies against: the trust
/// config, whether or not it exists, and the trusted keys and program it
/// names.
pub(crate) fn trust_files(home: &Path) -> Vec<PathBuf> {
    let tools = AccountTools {
        home: home.to_path_buf(),
    };
    let named: Vec<PathBuf> = trusted_signers(home)
        .map(|signers| {
            signers
                .configured_paths()
                .into_iter()
                .filter_map(|path| tools.path(path).ok())
                .collect()
        })
        .unwrap_or_default();
    std::iter::once(trust_config(home)).chain(named).collect()
}
