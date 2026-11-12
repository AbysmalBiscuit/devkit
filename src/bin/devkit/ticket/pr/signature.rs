//! The IO behind a `pr waive-verify --signed` check: paths under the account
//! home, verifier programs from the system directories, and a process
//! environment that the caller cannot set.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use devkit_common::cmd;
use devkit_config::SignatureTools;

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
