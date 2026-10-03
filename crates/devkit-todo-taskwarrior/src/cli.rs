//! The only code that runs `task`. Every call declares the UDAs itself, turns
//! off prompts, since there is no terminal to answer them, and closes stdin,
//! which answers any prompt taskwarrior still raises.

use std::{
    fmt, io,
    process::{Command, Output, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, bail};

use crate::schema::{self, Exported};

/// taskchampion takes an immediate write lock with no busy timeout, so a
/// second writer fails at once instead of waiting its turn.
const BUSY_RETRIES: [Duration; 3] = [
    Duration::from_millis(50),
    Duration::from_millis(150),
    Duration::from_millis(400),
];

/// The configured `task` program could not be found.
#[derive(Debug)]
pub struct TaskwarriorNotFound {
    pub program: String,
}

impl fmt::Display for TaskwarriorNotFound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "taskwarrior not found: {} ([todo.taskwarrior] path)",
            self.program
        )
    }
}

impl std::error::Error for TaskwarriorNotFound {}

pub(crate) struct Cli<'a> {
    pub program: &'a str,
    pub env: &'a [(String, String)],
}

impl Cli<'_> {
    fn spawn(&self, args: &[String]) -> Result<Output> {
        // `output` drains both pipes while the child runs, which an export
        // larger than a pipe buffer needs.
        let output = Command::new(self.program)
            .args(schema::UDAS.map(|(key, value)| format!("rc.{key}={value}")))
            // `bulk=0` because a bulk edit asks for confirmation whatever
            // `confirmation` says.
            .args(["rc.confirmation=off", "rc.bulk=0"])
            .args(args)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .output();
        match output {
            Ok(output) => Ok(output),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(TaskwarriorNotFound {
                program: self.program.to_string(),
            }
            .into()),
            Err(e) => Err(e).with_context(|| format!("running {}", self.program)),
        }
    }

    /// Runs `task` with `args`, retrying while the database is locked, and
    /// returns its stdout.
    pub fn run(&self, args: &[String]) -> Result<Vec<u8>> {
        let mut retries = BUSY_RETRIES.iter();
        loop {
            let output = self.spawn(args)?;
            if output.status.success() {
                return Ok(output.stdout);
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = match stderr.trim() {
                "" => String::from_utf8_lossy(&output.stdout).trim().to_string(),
                stderr => stderr.to_string(),
            };
            match retries.next() {
                Some(delay) if is_busy(&message) => std::thread::sleep(*delay),
                _ => bail!("{} failed: {message}", self.program),
            }
        }
    }

    /// Every task the filter words in `filter` match, as exported.
    pub fn export(&self, filter: &[String]) -> Result<Vec<Exported>> {
        let mut args = vec!["rc.verbose=nothing".to_string(), "rc.json.array=on".into()];
        args.extend(filter.iter().cloned());
        args.push("export".into());
        let stdout = self.run(&args)?;
        serde_json::from_slice(&stdout)
            .with_context(|| format!("reading {} export output", self.program))
    }

    /// Runs `<uuids> <words>`, a write addressed by full uuid.
    pub fn write(&self, uuids: &[&str], words: &[String]) -> Result<()> {
        let mut args = vec!["rc.verbose=nothing".to_string()];
        args.extend(uuids.iter().map(|u| u.to_string()));
        args.extend(words.iter().cloned());
        self.run(&args).map(drop)
    }

    /// Runs `add <words>` and returns the new task's uuid.
    pub fn add(&self, words: &[String]) -> Result<String> {
        let mut args = vec!["rc.verbose=new-uuid".to_string(), "add".into()];
        args.extend(words.iter().cloned());
        let stdout = self.run(&args)?;
        let stdout = String::from_utf8_lossy(&stdout);
        created_uuid(&stdout)
            .with_context(|| format!("{} add printed no uuid: {}", self.program, stdout.trim()))
    }
}

fn is_busy(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("database is locked") || lower.contains("sqlite_busy")
}

fn created_uuid(stdout: &str) -> Option<String> {
    stdout.lines().find_map(|line| {
        let rest = line
            .trim()
            .strip_prefix("Created task ")?
            .trim_end_matches('.');
        (rest.len() == 36 && rest.matches('-').count() == 4).then(|| rest.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_stderr_is_retryable_and_others_are_not() {
        assert!(is_busy("database is locked"));
        assert!(is_busy("Error: SQLITE_BUSY"));
        assert!(!is_busy("No matches."));
    }

    #[test]
    fn the_new_uuid_is_read_from_verbose_output() {
        let out = "Created task 8e4a5a4e-1f2b-4c3d-9e8f-0a1b2c3d4e5f.\n";
        assert_eq!(
            created_uuid(out).as_deref(),
            Some("8e4a5a4e-1f2b-4c3d-9e8f-0a1b2c3d4e5f")
        );
        assert_eq!(created_uuid("Created task 5."), None);
    }
}
