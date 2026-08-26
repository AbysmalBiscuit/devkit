use anyhow::Result;
use clap::Subcommand;
use devkit::completions::Shell;
use devkit_locks::hook::{self, HookEvent};
use devkit_locks::model::{Conflict, LockEntry, WriteDecision};

#[derive(clap::Args)]
pub struct LocksCli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand)]
pub(crate) enum Cmd {
    /// Claim one or more paths (files or directories). Fails if any is held by another session.
    Acquire {
        /// Files or directories to claim.
        paths: Vec<String>,
        /// Session identity to hold the claim under. Defaults to
        /// $DEVKIT_SESSION, then $TMUX_PANE, then the controlling tty, then the
        /// parent pid — pass the same value to acquire and release.
        #[arg(long = "as")]
        holder: Option<String>,
        /// Why you hold these paths; shown to whoever the claim blocks.
        #[arg(long)]
        note: Option<String>,
        /// Lock lifetime, seconds (0 = no expiry). Default 1800 (30 min).
        #[arg(long, default_value_t = 1800)]
        ttl: u64,
        /// Emit the result as JSON instead of a human-readable line.
        #[arg(long)]
        json: bool,
    },
    /// Read-only: would `acquire` of these paths succeed?
    Check {
        /// Files or directories to test.
        paths: Vec<String>,
        /// Session identity to hold the claim under. Defaults to
        /// $DEVKIT_SESSION, then $TMUX_PANE, then the controlling tty, then the
        /// parent pid — pass the same value to acquire and release.
        #[arg(long = "as")]
        holder: Option<String>,
        /// Emit the result as JSON instead of a human-readable line.
        #[arg(long)]
        json: bool,
    },
    /// Release your claims on the named paths (or all of them with --all).
    Release {
        /// Paths to release; ignored with --all.
        paths: Vec<String>,
        /// Session identity to hold the claim under. Defaults to
        /// $DEVKIT_SESSION, then $TMUX_PANE, then the controlling tty, then the
        /// parent pid — pass the same value to acquire and release.
        #[arg(long = "as")]
        holder: Option<String>,
        /// Release every path this holder claims.
        #[arg(long)]
        all: bool,
        /// Release even a path held by another session.
        #[arg(long)]
        force: bool,
    },
    /// Show held locks for this project (or every project with --all).
    #[command(visible_alias = "list")]
    Status {
        /// List every project's locks, not only this repo's.
        #[arg(long)]
        all: bool,
        /// Emit the rows as JSON instead of a table.
        #[arg(long)]
        json: bool,
        /// Not accepted — `status` lists everything; `check <paths…>` tests paths.
        #[arg(hide = true)]
        paths: Vec<String>,
    },
    /// Drop expired/dead locks.
    Prune,
    /// Print a shell-completion script (bash, zsh, fish, …) to stdout.
    Completions {
        /// Shell to emit the script for.
        shell: Shell,
    },
    /// Internal: evaluate a coding-agent hook payload (stdin JSON) and emit a
    /// PreToolUse decision (stdout). Events: pretooluse | subagent-stop | session-end.
    #[command(hide = true)]
    Hook { event: String },
}

fn print_conflicts(conflicts: &[Conflict]) {
    eprintln!(
        "conflict: {} path(s) held by another session:",
        conflicts.len()
    );
    for c in conflicts {
        let note = c
            .note
            .as_deref()
            .map(|n| format!(" — {n}"))
            .unwrap_or_default();
        eprintln!(
            "  {} held by {} ({}s ago){}",
            c.path, c.held_by, c.age_secs, note
        );
    }
}

/// Anchor a write target to the session's own cwd. `apply_patch` names paths
/// relative to the session, not to wherever the harness spawned the hook process.
fn resolve_against(payload: &serde_json::Value, path: &str) -> String {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return path.to_string();
    }
    payload
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(|cwd| {
            std::path::Path::new(cwd)
                .join(p)
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_else(|| path.to_string())
}

/// Map a write decision to the optional stdout envelope. `None` = allow silently.
fn write_output(d: &WriteDecision) -> Option<serde_json::Value> {
    match d {
        WriteDecision::Acquired | WriteDecision::AllowedByOwnership => None,
        WriteDecision::Denied(conflicts) => {
            let who = conflicts
                .iter()
                .map(|c| format!("{} (held by {})", c.path, c.held_by))
                .collect::<Vec<_>>()
                .join(", ");
            Some(hook::deny_json(&format!(
                "devkit write-harness: {who} — locked by another agent; \
                 coordinate or wait for it to finish"
            )))
        }
    }
}

fn run_hook(event: &str) {
    use std::io::Read;
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        return; // can't read payload → allow
    }
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&buf) else {
        return; // malformed → allow
    };

    let cwd = payload
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    if !hook::enforcement_enabled(&cwd) {
        return; // no opt-in (env, project layers, or global config) → no enforcement
    }

    match hook::parse_event(event, &payload) {
        HookEvent::Write {
            file_paths, holder, ..
        } => {
            let mut conflicts = Vec::new();
            let mut resolver = devkit_locks::WriteResolver::new();
            for path in &file_paths {
                let target = resolve_against(&payload, path);
                match resolver.decide_write(&target, &holder, Some("write-harness"), 1800) {
                    Ok(WriteDecision::Denied(c)) => conflicts.extend(c),
                    Ok(_) => {}
                    Err(e) => {
                        // fail closed: a registry error must not silently reopen the window
                        let out = hook::deny_json(&format!(
                            "devkit write-harness: registry error (fail-closed): {e:#}"
                        ));
                        println!("{out}");
                        return;
                    }
                }
            }
            if !conflicts.is_empty()
                && let Some(out) = write_output(&WriteDecision::Denied(conflicts))
            {
                println!("{out}");
            }
        }
        HookEvent::ReleaseSubagent { holder } | HookEvent::ReleaseSession { holder } => {
            let _ = devkit_locks::release_prefix(&holder);
        }
        HookEvent::Ignore => {}
    }
}

pub fn run(cli: LocksCli) -> Result<()> {
    match cli.cmd {
        Cmd::Acquire {
            paths,
            holder,
            note,
            ttl,
            json,
        } => {
            let out = devkit_locks::acquire(&paths, holder.as_deref(), note.as_deref(), ttl)?;
            if json {
                let ok = out.conflicts.is_empty();
                let payload = serde_json::json!({ "ok": ok, "acquired": out.acquired, "conflicts": out.conflicts });
                println!("{}", serde_json::to_string(&payload)?);
            } else if out.conflicts.is_empty() {
                for a in &out.acquired {
                    println!("locked {} (ttl {}s)", a.path, a.ttl_secs);
                }
            } else {
                print_conflicts(&out.conflicts);
            }
            if !out.conflicts.is_empty() {
                std::process::exit(1);
            }
            Ok(())
        }
        Cmd::Check {
            paths,
            holder,
            json,
        } => {
            let conflicts = devkit_locks::check(&paths, holder.as_deref())?;
            if json {
                let payload =
                    serde_json::json!({ "ok": conflicts.is_empty(), "conflicts": conflicts });
                println!("{}", serde_json::to_string(&payload)?);
            } else if conflicts.is_empty() {
                println!("available");
            } else {
                print_conflicts(&conflicts);
            }
            if !conflicts.is_empty() {
                std::process::exit(1);
            }
            Ok(())
        }
        Cmd::Release {
            paths,
            holder,
            all,
            force,
        } => {
            if all {
                let freed = devkit_locks::release_all(holder.as_deref())?;
                println!("released {} lock(s)", freed.len());
            } else {
                let (released, refused) = devkit_locks::release(&paths, holder.as_deref(), force)?;
                println!("released {} lock(s)", released.len());
                if !refused.is_empty() {
                    eprintln!(
                        "refused (held by another session; use --force): {}",
                        refused.join(", ")
                    );
                    std::process::exit(1);
                }
            }
            Ok(())
        }
        Cmd::Status { all, json, paths } => {
            if !paths.is_empty() {
                eprintln!(
                    "status takes no paths — to test whether specific paths are free, \
                     use `lockm check {}`",
                    paths.join(" ")
                );
                std::process::exit(2);
            }
            let locks = devkit_locks::status(all)?;
            if json {
                println!("{}", serde_json::to_string(&status_json(&locks))?);
            } else {
                print!("{}", status_table(&locks, all));
            }
            Ok(())
        }
        Cmd::Prune => {
            let n = devkit_locks::prune()?;
            println!("pruned {n} lock(s)");
            Ok(())
        }
        Cmd::Completions { shell } => {
            crate::emit_completions(shell, "locks", "lockm");
            Ok(())
        }
        Cmd::Hook { event } => {
            run_hook(&event);
            Ok(())
        }
    }
}

fn status_json(locks: &[LockEntry]) -> serde_json::Value {
    serde_json::json!({
        "locks": locks.iter().map(|e| serde_json::json!({
            "path": e.path, "root": e.root, "holder": e.holder,
            "pid": e.pid, "note": e.note, "ts": e.ts, "ttl": e.ttl,
        })).collect::<Vec<_>>()
    })
}

fn status_table(locks: &[LockEntry], all: bool) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let headers: Vec<&str> = if all {
        vec!["ROOT", "PATH", "HOLDER", "AGE", "TTL-LEFT", "PID", "NOTE"]
    } else {
        vec!["PATH", "HOLDER", "AGE", "TTL-LEFT", "PID", "NOTE"]
    };
    let mut t = devkit_common::ui::table(&headers);
    for e in locks {
        let age = format!("{}s", now.saturating_sub(e.ts));
        let ttl_left = if e.ttl == 0 {
            "∞".to_string()
        } else {
            format!("{}s", e.ttl.saturating_sub(now.saturating_sub(e.ts)))
        };
        let pid = e.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into());
        let note = e.note.clone().unwrap_or_default();
        let mut row = Vec::new();
        if all {
            row.push(
                devkit_common::paths::leaf(&e.root)
                    .unwrap_or(&e.root)
                    .to_string(),
            );
        }
        row.extend([e.path.clone(), e.holder.clone(), age, ttl_left, pid, note]);
        t.add_row(row);
    }
    format!("{t}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use devkit_locks::model::{Conflict, WriteDecision};

    #[test]
    fn allowed_decisions_emit_nothing() {
        assert_eq!(write_output(&WriteDecision::Acquired), None);
        assert_eq!(write_output(&WriteDecision::AllowedByOwnership), None);
    }

    #[test]
    fn denied_decision_emits_deny_with_holder() {
        let d = WriteDecision::Denied(vec![Conflict {
            path: "src/a.rs".into(),
            held_by: "S/b2".into(),
            age_secs: 5,
            note: None,
        }]);
        let out = write_output(&d).expect("deny json");
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
        let reason = out["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(reason.contains("S/b2"), "reason names the holder: {reason}");
        assert!(
            reason.contains("src/a.rs"),
            "reason names the path: {reason}"
        );
    }
}
