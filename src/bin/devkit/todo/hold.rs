//! Whose stop the todo hold refuses: the mode a session set with
//! `devkit todo hold`, over [`HOLD_VAR`], over `[todo] hold_stop`.

use std::fmt;

use anyhow::{Result, bail};
use devkit_config::{HoldStop, TodoConfig};
use devkit_todo::{Holder, hold::mode_path, node};

/// Overrides `[todo] hold_stop`, so an unattended launcher can hold every
/// agent while the project's own config still loads.
pub(crate) const HOLD_VAR: &str = "DEVKIT_TODO_HOLD_STOP";

/// The layer the hold mode in effect came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HoldSource {
    Session,
    Env,
    Config,
    Default,
}

impl fmt::Display for HoldSource {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            Self::Session => "this session",
            Self::Env => HOLD_VAR,
            Self::Config => "[todo] hold_stop",
            Self::Default => "the default",
        })
    }
}

/// The mode `session` set, when it set one that still parses.
fn session_mode(session: &Holder) -> Option<HoldStop> {
    std::fs::read_to_string(mode_path(session))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The hold mode in effect for `session`: its own mode, else `env` (the
/// value of [`HOLD_VAR`]), else `config`. An unknown `env` value is an error
/// naming the variable and the accepted spellings.
pub(crate) fn effective(
    session: Option<&Holder>,
    env: Option<&str>,
    config: Option<&TodoConfig>,
) -> Result<(HoldStop, HoldSource)> {
    if let Some(mode) = session.and_then(session_mode) {
        return Ok((mode, HoldSource::Session));
    }
    if let Some(name) = env.map(str::trim).filter(|v| !v.is_empty()) {
        let Ok(mode) = name.parse() else {
            bail!("{HOLD_VAR}: unknown mode `{name}`, expected `always`, `subagents` or `never`");
        };
        return Ok((mode, HoldSource::Env));
    }
    Ok(match config.map(|c| c.hold_stop) {
        Some(mode) if mode != HoldStop::default() => (mode, HoldSource::Config),
        _ => (HoldStop::default(), HoldSource::Default),
    })
}

/// `devkit todo hold`: sets `mode` for the calling session, or drops its
/// mode with `clear`, then prints the mode in effect and where it came from.
pub(crate) fn run(mode: Option<HoldStop>, clear: bool, config: Option<&TodoConfig>) -> Result<()> {
    let get = |key: &str| std::env::var(key).ok();
    let session = node::session_from_env(get).map(|s| Holder::new(s.id));
    if mode.is_some() || clear {
        let Some(session) = &session else {
            bail!("devkit todo hold needs an agent session to set or clear a session's mode");
        };
        let path = mode_path(session);
        match mode {
            Some(mode) => {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&path, mode.to_string())?;
            }
            None => match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            },
        }
    }
    let env = std::env::var(HOLD_VAR).ok();
    let (mode, source) = effective(session.as_ref(), env.as_deref(), config)?;
    println!("{mode} ({source})");
    Ok(())
}
