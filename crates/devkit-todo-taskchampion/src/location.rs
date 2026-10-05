use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

const MAX_INCLUDE_DEPTH: usize = 10;
const PACKAGE_RC_DIRS: [&str; 4] = [
    "/usr/share/taskwarrior",
    "/usr/share/doc/task/rc",
    "/usr/local/share/doc/task/rc",
    "/opt/homebrew/share/doc/task/rc",
];

/// Where the replica directory came from.
#[derive(Debug)]
pub enum ReplicaSource {
    Config,
    Taskdata,
    Taskrc(PathBuf),
    Default,
}

/// The replica directory and its provenance.
#[derive(Debug)]
pub struct ReplicaLocation {
    pub path: PathBuf,
    pub source: ReplicaSource,
}

/// The configured directory, then `TASKDATA`, then taskrc's `data.location`,
/// then devkit's todo state. A malformed taskrc warns and uses the default.
pub fn replica_location(config: Option<&str>) -> ReplicaLocation {
    if let Some(path) = config {
        return ReplicaLocation {
            path: expand_tilde(Path::new(path)),
            source: ReplicaSource::Config,
        };
    }
    if let Some(path) = std::env::var_os("TASKDATA").filter(|v| !v.is_empty()) {
        return ReplicaLocation {
            path: expand_tilde(Path::new(&path)),
            source: ReplicaSource::Taskdata,
        };
    }
    if let Some(taskrc) = taskrc_path() {
        match read_taskrc(&taskrc) {
            Ok(Some(path)) => {
                return ReplicaLocation {
                    path: expand_tilde(&path),
                    source: ReplicaSource::Taskrc(taskrc),
                };
            }
            Ok(None) => {}
            Err(error) => eprintln!("devkit todo: {error:#}; using the default replica"),
        }
    }
    ReplicaLocation {
        path: devkit_todo::state_dir().join("taskchampion"),
        source: ReplicaSource::Default,
    }
}

fn taskrc_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("TASKRC").filter(|v| !v.is_empty()) {
        return Some(expand_tilde(Path::new(&path)));
    }
    let home = devkit_common::paths::try_home();
    let home_taskrc = home.as_ref().map(|home| home.join(".taskrc"));
    if home_taskrc.as_ref().is_some_and(|path| path.exists()) {
        return home_taskrc;
    }
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(|path| expand_tilde(Path::new(&path)))
        .or_else(|| home.map(|home| home.join(".config")));
    config_home
        .map(|home| home.join("task/taskrc"))
        .filter(|path| path.exists())
        .or(home_taskrc)
}

fn read_taskrc(path: &Path) -> Result<Option<PathBuf>> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read taskrc {}", path.display()));
        }
    };
    let mut data = None;
    parse_taskrc(path, &contents, 0, &mut data)?;
    Ok(data.filter(|path: &PathBuf| !path.as_os_str().is_empty()))
}

fn parse_taskrc(
    path: &Path,
    contents: &str,
    depth: usize,
    data: &mut Option<PathBuf>,
) -> Result<()> {
    for (index, raw) in contents.lines().enumerate() {
        let location = || format!("{}:{}", path.display(), index + 1);
        let line = raw.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            if key.is_empty() {
                bail!("{}: malformed taskrc entry", location());
            }
            if key == "data.location" {
                *data = Some(PathBuf::from(expand(value.trim())));
            }
        } else if let Some(target) = line
            .strip_prefix("include")
            .filter(|rest| rest.starts_with(char::is_whitespace))
        {
            if depth + 1 == MAX_INCLUDE_DEPTH {
                bail!(
                    "{}: taskrc includes nested more than {MAX_INCLUDE_DEPTH} deep",
                    location()
                );
            }
            let included = find_include(path, target.trim())
                .with_context(|| format!("{}: cannot find include '{target}'", location()))?;
            let contents = fs::read_to_string(&included).with_context(|| {
                format!("{}: cannot read include {}", location(), included.display())
            })?;
            parse_taskrc(&included, &contents, depth + 1, data)?;
        } else {
            bail!("{}: malformed taskrc entry", location());
        }
    }
    Ok(())
}

fn find_include(including: &Path, target: &str) -> Result<PathBuf> {
    let target = PathBuf::from(expand(target));
    if target.is_absolute() {
        return Ok(target);
    }
    let including_dir = fs::canonicalize(including)
        .ok()
        .and_then(|real| real.parent().map(Path::to_path_buf));
    std::env::current_dir()
        .ok()
        .into_iter()
        .chain(including_dir)
        .chain(PACKAGE_RC_DIRS.map(PathBuf::from))
        .map(|dir| dir.join(&target))
        .find(|candidate| candidate.exists())
        .context("not in the working directory, the taskrc's directory or a package rc directory")
}

fn expand(text: &str) -> String {
    let text = expand_tilde(Path::new(text)).to_string_lossy().into_owned();
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(dollar) = rest.find('$') {
        out.push_str(&rest[..dollar]);
        let after = &rest[dollar + 1..];
        let name_len = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(after.len());
        if name_len == 0 {
            out.push('$');
        } else if let Ok(value) = std::env::var(&after[..name_len]) {
            out.push_str(&value);
        }
        rest = &after[name_len..];
    }
    out.push_str(rest);
    out
}

fn expand_tilde(path: &Path) -> PathBuf {
    if let Ok(rest) = path.strip_prefix("~")
        && let Some(home) = devkit_common::paths::try_home()
    {
        return home.join(rest);
    }
    path.to_path_buf()
}
