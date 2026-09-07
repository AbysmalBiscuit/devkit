# Lock holder identity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the write hook, the `lockm` CLI and the MCP server agree on who holds a lock, and fix the four lock-lifecycle defects that sit in the same code.

**Architecture:** The hook takes its holder from the harness payload's `session_id`; `lockm` and the MCP server detect the same id from a table of harness environment variables and rank it above `$DEVKIT_SESSION`. Ambiguity from nested harnesses is refused rather than guessed. Alongside that, lifecycle release stops being gated on enforcement, an unusable write payload fails closed, and the acquire path adopts the ancestry rule the write path already uses.

**Tech Stack:** Rust (edition 2024), `anyhow`, `clap`, `serde_json`, `tempfile`, `cargo nextest`.

**Spec:** `docs/superpowers/specs/2026-09-07-lock-holder-identity-design.md`

## Global Constraints

- TDD throughout: write the failing test, watch it fail for the right reason, then implement. `cargo nextest run --workspace --no-fail-fast` is the merge gate.
- `cargo clippy --workspace --all-targets -- -D warnings` must stay clean. Zero-warning policy.
- `cargo fmt --all` before every commit.
- Conventional Commits, imperative mood, subject 50 chars or fewer, lowercase after the colon, no trailing period.
- Comments are one line and explain a non-obvious why, never a restatement of the code. Default to no comment.
- Match exhaustively on the enums touched here. No `_ =>` catch-all arms.
- Test scratch comes from `tempfile`. Never build a path by hand from `std::env::temp_dir()`.
- The harness session variables are exactly `CLAUDE_CODE_SESSION_ID` and `CODEX_SESSION_ID`. `CODEX_THREAD_ID` is deliberately excluded: it carries the same value as `CODEX_SESSION_ID`, and listing both would manufacture a false ambiguity out of a single harness.
- Holder ids are `/`-separated segments: `session`, or `session/agent` for a subagent. `is_ancestor_or_self` matches on segment boundaries.
- The write hook fails closed. The command guard (`devkit harness shell`) fails open. Do not blur the two.

---

## File Structure

| File | Responsibility in this plan |
|---|---|
| `crates/devkit-locks/src/ident.rs` | Harness session detection, the variable table, and the `Identity` resolution result. Pure and table-driven. |
| `crates/devkit-locks/src/model.rs` | `check` ancestry, no-clobber acquire, ancestor lock renewal. Pure data operations. |
| `crates/devkit-locks/src/hook.rs` | Payload classification: the `Unusable` variant and the `SubagentStop` agent-id requirement. |
| `crates/devkit-locks/src/lib.rs` | Propagates `Identity` from `ident` to the CLI-facing entry points. |
| `src/bin/devkit/locks.rs` | Event parsing ahead of the enforcement gate; ambiguity refusal on `acquire` and `release`. |
| `crates/devkit-mcp/src/lib.rs` | Deferred ambiguity: resolve once at startup, fail per call only when `holder` is absent. |
| `src/bin/devkit/doctor.rs` | The `harness_identity` row comparing the resolved id against live `write-harness` rows. |
| `tests/common/testenv.rs` | One shared identity scrub for child processes, replacing seven hand-written copies. |

Tasks 1 through 5 are lock-lifecycle fixes independent of identity. Task 6 onwards is identity. Do them in order: task 6's tests depend on release actually working.

---

### Task 1: Lifecycle release runs regardless of the enforcement gate

`run_hook` evaluates `enforcement_enabled` before it parses the event, so a `SessionEnd` fired from a directory where enforcement is off never reaches `release_prefix` and the session's locks wait out their TTL. Releasing locks a session already holds is correct whether or not enforcement is currently on.

One expected side effect: every `SessionEnd` now reaches `FlockStore::commit`, which creates `<state>/devkit/devkitd.lock` and an empty `locks.json` on machines where enforcement was never enabled. That is the store's normal first-write behaviour, not a regression.

**Files:**
- Modify: `src/bin/devkit/locks.rs` (`run_hook`)
- Test: `tests/locks.rs`

**Interfaces:**
- Consumes: `devkit_locks::hook::{parse_event, HookEvent, enforcement_enabled}`, `devkit_locks::release_prefix`.
- Produces: nothing new. Behaviour change only.

- [ ] **Step 1: Write the failing test**

Add to `tests/locks.rs`:

```rust
#[test]
fn session_end_releases_even_when_enforcement_is_off() {
    let (_dir, link) = shimtest::linked("lockm");
    let proj = project();
    let state = tempfile::tempdir().unwrap();

    let a = run(
        &link,
        proj.path(),
        state.path(),
        &["acquire", "src/a.rs", "--as", "S"],
    );
    assert!(a.status.success(), "S should acquire");

    let payload = r#"{"session_id":"S","hook_event_name":"SessionEnd"}"#;
    let out = Command::new(&link)
        .args(["hook", "session-end"])
        .current_dir(proj.path())
        .env("XDG_STATE_HOME", state.path())
        .env("HOME", state.path())
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env("DEVKIT_ENFORCE_WRITES", "0")
        .env_remove("DEVKIT_SESSION")
        .env_remove("TMUX_PANE")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            c.stdin.as_mut().unwrap().write_all(payload.as_bytes())?;
            c.wait_with_output()
        })
        .expect("run session-end hook");
    assert!(out.status.success(), "hook exits 0");

    let s = run(&link, proj.path(), state.path(), &["status", "--json"]);
    let text = String::from_utf8_lossy(&s.stdout);
    assert!(
        !text.contains("\"S\""),
        "S's locks are released even with enforcement off: {text}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run --test locks session_end_releases_even_when_enforcement_is_off`

Expected: FAIL. The lock is still listed, because `run_hook` returned at the enforcement gate before parsing the event.

- [ ] **Step 3: Move parsing ahead of the gate**

In `src/bin/devkit/locks.rs`, replace the early return with a gate that applies to write events only:

```rust
    let event = hook::parse_event(event, &payload);
    match event {
        HookEvent::Write {
            file_paths, holder, ..
        } => {
            if !hook::enforcement_enabled(&cwd) {
                return; // no opt-in (env, project layers, or global config) → no enforcement
            }
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
```

Delete the standalone `if !hook::enforcement_enabled(&cwd) { return; }` that preceded the match.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run --test locks session_end_releases_even_when_enforcement_is_off`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/bin/devkit/locks.rs tests/locks.rs
git commit -m "fix(locks): release on session end without the write gate"
```

---

### Task 2: SubagentStop without an agent id releases nothing

`parse_event` composes the holder before it knows the event, so a `SubagentStop` carrying no `agent_id` yields the bare session holder and `release_prefix` frees the whole session, including the parent's claims and every sibling's.

**Files:**
- Modify: `crates/devkit-locks/src/hook.rs` (`parse_event`)
- Test: `crates/devkit-locks/src/hook.rs` (unit tests at the bottom of the file)

**Interfaces:**
- Consumes: `HookEvent`, `holder_from_fields`.
- Produces: no signature change. `parse_event("subagent-stop", p)` returns `HookEvent::Ignore` when `agent_id` is absent or empty.

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `crates/devkit-locks/src/hook.rs`:

```rust
    #[test]
    fn subagent_stop_without_agent_id_releases_nothing() {
        let p = json!({ "session_id": "S" });
        assert!(matches!(
            parse_event("subagent-stop", &p),
            HookEvent::Ignore
        ));
    }

    #[test]
    fn subagent_stop_with_agent_id_releases_that_subagent() {
        let p = json!({ "session_id": "S", "agent_id": "a1" });
        match parse_event("subagent-stop", &p) {
            HookEvent::ReleaseSubagent { holder } => assert_eq!(holder, "S/a1"),
            other => panic!("expected ReleaseSubagent, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p devkit-locks subagent_stop_without_agent_id`

Expected: FAIL. The current arm returns `ReleaseSubagent { holder: "S" }`, which would free the entire session.

- [ ] **Step 3: Require the agent id on that arm**

In `parse_event`, replace the `"subagent-stop"` arm:

```rust
        "subagent-stop" => match agent {
            // Releasing the bare session holder here would free the parent's and
            // every sibling's locks, so an unattributable stop releases nothing.
            Some(a) => HookEvent::ReleaseSubagent {
                holder: holder_from_fields(session, Some(a)),
            },
            None => HookEvent::Ignore,
        },
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p devkit-locks subagent_stop`

Expected: PASS, both tests.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-locks/src/hook.rs
git commit -m "fix(locks): ignore a subagent stop with no agent id"
```

---

### Task 3: An unusable write payload fails closed

A write payload with no `session_id`, or one whose targets cannot be extracted, currently returns `Ignore`, which `run_hook` treats as allow. That is a fail-open path inside the component whose contract is to fail closed, and a payload format change would disable enforcement with no signal.

**Files:**
- Modify: `crates/devkit-locks/src/hook.rs` (`HookEvent`, `parse_event`)
- Modify: `src/bin/devkit/locks.rs` (`run_hook`)
- Test: `crates/devkit-locks/src/hook.rs`

**Interfaces:**
- Consumes: `HookEvent`, `WRITE_TOOLS`, `deny_json`.
- Produces: `HookEvent::Unusable { reason: String }`, returned only for a write tool whose payload cannot be used. Non-write tools keep returning `Ignore`.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/devkit-locks/src/hook.rs`:

```rust
    #[test]
    fn write_without_session_id_is_unusable_not_ignored() {
        let p = json!({
            "tool_name": "Edit",
            "tool_input": { "file_path": "/repo/src/a.rs" }
        });
        match parse_event("pretooluse", &p) {
            HookEvent::Unusable { reason } => assert!(reason.contains("session_id")),
            other => panic!("expected Unusable, got {other:?}"),
        }
    }

    #[test]
    fn write_with_no_extractable_target_is_unusable() {
        let p = json!({ "session_id": "S", "tool_name": "Edit", "tool_input": {} });
        match parse_event("pretooluse", &p) {
            HookEvent::Unusable { reason } => assert!(reason.contains("target")),
            other => panic!("expected Unusable, got {other:?}"),
        }
    }

    #[test]
    fn non_write_tool_without_session_id_is_still_ignored() {
        let p = json!({ "tool_name": "Bash", "tool_input": { "command": "ls" } });
        assert!(matches!(parse_event("pretooluse", &p), HookEvent::Ignore));
    }
```

Three existing tests in the same block assert the fail-open behaviour this task removes. Handle each:

- `parse_write_event_ignores_missing_file_path` (line 172) — delete. Its payload is exactly the second case above.
- `parse_apply_patch_event_ignores_a_patch_naming_no_file` (line 227) — delete. An empty patch now yields `Unusable`, and `write_with_no_extractable_target_is_unusable` covers the same ground for the non-patch tool. If you would rather keep patch coverage, rewrite its assertion to match on `HookEvent::Unusable { reason }` and assert `reason.contains("target")`.
- `parse_event_ignores_missing_session_id` (line 266) — rewrite rather than delete. Its `write`-tool sub-cases (missing and empty `session_id`) now yield `Unusable`, so drop them; its `subagent-stop`-without-session sub-case still yields `Ignore` and must stay.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p devkit-locks unusable`

Expected: FAIL to compile, with "no variant named `Unusable`".

- [ ] **Step 3: Add the variant and reorder the checks**

In `crates/devkit-locks/src/hook.rs`, add to `HookEvent`:

```rust
    /// A write this hook cannot evaluate. Denied rather than allowed: the write
    /// path fails closed, so an unreadable payload must not open the window.
    Unusable {
        reason: String,
    },
```

Then restructure `parse_event` so the tool is classified before the session id is required:

```rust
pub fn parse_event(event: &str, p: &Value) -> HookEvent {
    let session = str_field(p, "session_id");
    let agent = str_field(p, "agent_id");
    match event {
        "pretooluse" => {
            let tool = str_field(p, "tool_name").unwrap_or("");
            if !WRITE_TOOLS.contains(&tool) {
                return HookEvent::Ignore;
            }
            let Some(session) = session else {
                return HookEvent::Unusable {
                    reason: "write payload carries no session_id".into(),
                };
            };
            let input = p.get("tool_input");
            let file_paths = if tool == APPLY_PATCH {
                input
                    .and_then(|ti| str_field(ti, "command"))
                    .map(apply_patch_paths)
                    .unwrap_or_default()
            } else {
                input
                    .and_then(|ti| str_field(ti, "file_path"))
                    .map(|fp| vec![fp.to_string()])
                    .unwrap_or_default()
            };
            if file_paths.is_empty() {
                return HookEvent::Unusable {
                    reason: format!("write payload for {tool} names no target"),
                };
            }
            HookEvent::Write {
                tool_name: tool.to_string(),
                file_paths,
                holder: holder_from_fields(session, agent),
            }
        }
        "subagent-stop" => match (session, agent) {
            // Releasing the bare session holder here would free the parent's and
            // every sibling's locks, so an unattributable stop releases nothing.
            (Some(s), Some(a)) => HookEvent::ReleaseSubagent {
                holder: holder_from_fields(s, Some(a)),
            },
            (None, _) | (Some(_), None) => HookEvent::Ignore,
        },
        "session-end" => match session {
            Some(s) => HookEvent::ReleaseSession {
                holder: s.to_string(),
            },
            None => HookEvent::Ignore,
        },
        _ => HookEvent::Ignore,
    }
}
```

This supersedes the arm written in Task 2; the `subagent-stop` behaviour is identical.

- [ ] **Step 4: Deny on the new variant**

In `src/bin/devkit/locks.rs`, add an arm to the match in `run_hook`, alongside the existing ones:

```rust
        HookEvent::Unusable { reason } => {
            if hook::enforcement_enabled(&cwd) {
                println!(
                    "{}",
                    hook::deny_json(&format!("devkit write-harness: {reason} (fail-closed)"))
                );
            }
        }
```

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p devkit-locks && cargo nextest run --test locks`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-locks/src/hook.rs src/bin/devkit/locks.rs
git commit -m "fix(locks): deny a write payload the hook cannot read"
```

---

### Task 4: An ancestor lock is renewed by writes underneath it

`decide_write` renews only the exact-path row for the writer, so a claim over a directory is never renewed by writes to files under it and expires mid-session while its holder is still working inside it. That undercuts the documented advice to lock a subtree.

**Files:**
- Modify: `crates/devkit-locks/src/model.rs` (`Data::decide_write`)
- Test: `crates/devkit-locks/src/model.rs`

**Interfaces:**
- Consumes: `is_ancestor_or_self`, `paths_overlap`, `key_for`, `LockEntry`.
- Produces: no signature change. `decide_write` bumps `ts` on an overlapping row whose holder is the writer or an ancestor of the writer, and never rewrites that row's holder.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/devkit-locks/src/model.rs`:

```rust
    #[test]
    fn decide_write_renews_an_ancestor_directory_lock() {
        let mut d = Data::default();
        d.locks
            .extend([entry("/repo", "src/auth", "S", 1, 0, None)]);
        let r = d.decide_write("/repo", "src/auth/mod.rs", "S/a1", None, None, 1800, 900);
        assert_eq!(r, WriteDecision::AllowedByOwnership);
        let e = &d.locks[&key_for("/repo", "src/auth")];
        assert_eq!(e.ts, 900, "the directory lock's timestamp is bumped");
        assert_eq!(e.holder, "S", "the ancestor's holder is never rewritten");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p devkit-locks decide_write_renews_an_ancestor`

Expected: FAIL with `assertion failed: left == right` on `ts`, still `1`, because only the exact-path row is renewed.

- [ ] **Step 3: Renew the overlapping row instead of the exact-path row**

In `Data::decide_write`, replace the renewal block inside the `overlaps` branch:

```rust
        if overlaps {
            for e in self.locks.values_mut() {
                if e.root == root
                    && !entry_dead(e, now)
                    && paths_overlap(&e.path, path)
                    && is_ancestor_or_self(&e.holder, writer)
                {
                    e.ts = now;
                }
            }
            return WriteDecision::AllowedByOwnership;
        }
```

The holder is never assigned, so an ancestor's row keeps its own narrower identity.

Rewrite the one-line doc comment above `decide_write`, which still says it renews "the writer's own exact-path lock":

```rust
    /// Decide one write, renewing the overlapping row that permits it: the
    /// writer's own, or an ancestor's. The row's holder is never rewritten.
```

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p devkit-locks decide_write`

Expected: PASS, including the pre-existing `decide_write_self_is_allowed_and_renews` and `decide_write_ancestor_allowed_without_clobber`.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-locks/src/model.rs
git commit -m "fix(locks): renew a directory lock from writes beneath it"
```

---

### Task 5: Acquire adopts the ancestry rule, without clobbering

`Data::check` compares holders with exact inequality while `decide_write` uses `is_ancestor_or_self`, so the acquire path is stricter than the write path. Once identity detection lands, a subagent resolves to the bare session id, and a subagent that writes a file through the hook first is then refused when it claims that same file by hand. `try_acquire` also inserts unconditionally at `key_for(root, path)`, so permitting that acquire without care would replace `S/a1` with `S` and silently widen the claim until every sibling could write it.

**Files:**
- Modify: `crates/devkit-locks/src/model.rs` (`Data::check`, `Data::try_acquire`, `AcquireOutcome`)
- Modify: `src/bin/devkit/locks.rs` (the `Acquire` arm's text and `--json` output)
- Test: `crates/devkit-locks/src/model.rs`

**Interfaces:**
- Consumes: `is_ancestor_or_self`, `paths_overlap`, `key_for`, `Conflict`, `AcquireOutcome`.
- Produces: no signature change. `check` blocks unless the two holders lie on one ancestry line in either direction; `try_acquire` leaves an existing row untouched when the acquire was permitted by ancestry rather than an exact holder match.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/devkit-locks/src/model.rs`:

```rust
    #[test]
    fn check_allows_both_directions_of_one_session() {
        let mut d = Data::default();
        d.locks.extend([entry("/repo", "a", "S/a1", 1, 0, None)]);
        let parent = d.check("/repo", &["a".to_string()], "S", 5);
        assert!(parent.is_empty(), "a parent may claim over its subagent");

        let mut d2 = Data::default();
        d2.locks.extend([entry("/repo", "a", "S", 1, 0, None)]);
        let child = d2.check("/repo", &["a".to_string()], "S/a1", 5);
        assert!(child.is_empty(), "a subagent may claim under its parent");
    }

    #[test]
    fn check_still_blocks_siblings_and_other_sessions() {
        let mut d = Data::default();
        d.locks.extend([entry("/repo", "a", "S/a1", 1, 0, None)]);
        assert_eq!(
            d.check("/repo", &["a".to_string()], "S/a2", 5).len(),
            1,
            "siblings stay isolated"
        );
        assert_eq!(
            d.check("/repo", &["a".to_string()], "S2", 5).len(),
            1,
            "other sessions stay blocked"
        );
    }

    #[test]
    fn acquire_permitted_by_ancestry_does_not_rewrite_the_holder() {
        let mut d = Data::default();
        d.locks.extend([entry("/repo", "a", "S/a1", 1, 0, None)]);
        let out = d.try_acquire("/repo", &["a".to_string()], "S", None, None, 1800, 5);
        assert!(out.conflicts.is_empty(), "permitted by ancestry");
        assert_eq!(
            d.locks[&key_for("/repo", "a")].holder,
            "S/a1",
            "the narrower claim survives; widening it would unblock siblings"
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p devkit-locks check_allows_both_directions acquire_permitted_by_ancestry`

Expected: FAIL. `check` reports a conflict in both directions, and the acquire test never reaches its assertion.

- [ ] **Step 3: Widen the check to one ancestry line**

In `Data::check`, replace the holder comparison:

```rust
                if e.root == root
                    && !on_one_ancestry_line(&e.holder, holder)
                    && !entry_dead(e, now)
                    && paths_overlap(&e.path, req)
```

Rewrite the doc comment above `Data::check`, which says "any live lock by a *different* holder":

```rust
    /// Conflicts that would block acquiring `paths` for `holder` in `root`: any
    /// live lock on another session line whose path overlaps a requested path.
    /// A holder's own line — itself, its parent, its sub-agents — never conflicts.
```

Add the helper beside `is_ancestor_or_self`:

```rust
/// True when two holders are the same session line: equal, or one an ancestor of
/// the other. Siblings (`S/a1` against `S/a2`) are not.
pub fn on_one_ancestry_line(a: &str, b: &str) -> bool {
    is_ancestor_or_self(a, b) || is_ancestor_or_self(b, a)
}
```

- [ ] **Step 4: Stop clobbering a row the acquire did not exactly match**

In `Data::try_acquire`, replace the unconditional insert inside the loop:

```rust
        for req in paths {
            let key = key_for(root, req);
            // An acquire permitted by ancestry must not widen somebody's narrower
            // claim: only an exact holder match is rewritten.
            let keep = self
                .locks
                .get(&key)
                .is_some_and(|e| e.holder != holder && !entry_dead(e, now));
            if !keep {
                self.locks.insert(
                    key,
                    LockEntry {
                        path: req.clone(),
                        root: root.into(),
                        holder: holder.into(),
                        pid,
                        note: note.map(str::to_string),
                        ts: now,
                        ttl,
                    },
                );
            }
            if keep {
                already_held.push(req.clone());
            } else {
                acquired.push(Acquired {
                    path: req.clone(),
                    ttl_secs: ttl,
                });
            }
        }
```

Rewrite the doc comment above `try_acquire`, whose "(or renew, for the same holder+path)" no longer describes the loop:

```rust
    /// All-or-nothing acquire: if any requested path conflicts, acquire none and
    /// return the conflicts. Otherwise insert, or renew when the row is this
    /// holder's own. A live row held by another holder on the same session line
    /// permits the acquire but is left alone.
```

A path kept this way was not claimed by this call, so reporting it as `Acquired` would print `locked <path>` for a row this holder cannot release without `--force`. It needs its own field.

In `crates/devkit-locks/src/model.rs`, `AcquireOutcome` (line 86) gains it:

```rust
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AcquireOutcome {
    pub acquired: Vec<Acquired>,
    /// Paths a live row on this holder's own session line already covers. The
    /// caller may write them; releasing them needs the row's own holder.
    #[serde(default)]
    pub already_held: Vec<String>,
    pub conflicts: Vec<Conflict>,
}
```

`#[serde(default)]` keeps a payload written by an older `devkitd` readable. `try_acquire` builds `AcquireOutcome` at two points, the early conflict return (line 154) and the success return (line 178); both need the field, and `already_held: Vec::new()` on the conflict path is right, since a conflicting call acquires and keeps nothing.

In `src/bin/devkit/locks.rs`, the `Acquire` arm prints it before the `locked` lines:

```rust
                for p in &out.already_held {
                    println!("already held on this session line: {p}");
                }
```

and the `--json` payload gains `"already_held": out.already_held`. Exit status stays 0: the caller may write the path, which is the question `acquire` answers.

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p devkit-locks`

Expected: PASS, including `release_all_clears_only_callers_locks_in_root` and `no_overlap_sibling_prefix`.

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-locks/src/model.rs src/bin/devkit/locks.rs
git commit -m "fix(locks): let one session line acquire its own paths"
```

---

### Task 6: Detect the harness session id

The hook holds locks under the payload's `session_id`, which on both supported harnesses is exported into the environment of the commands that session runs. Reading it is what makes `lockm` agree with the hook. The scrub helper ships in this task because without it seven existing test files inherit the developer's own session id and resolve a different default identity than CI does.

**Files:**
- Modify: `crates/devkit-locks/src/ident.rs`
- Modify: `crates/devkit-locks/src/lib.rs:171` (the `ctx()` holder line)
- Create: `tests/common/testenv.rs`
- Modify: `tests/locks.rs`, `tests/mcp.rs`, `tests/cli_ergonomics.rs`, `tests/sync_includes.rs`, `tests/brief_main_checkout.rs`, `tests/issue_setup_root.rs`, `tests/issue_bare_main_root.rs`
- Test: `crates/devkit-locks/src/ident.rs`

**Interfaces:**
- Consumes: `devkit_common::sys::{controlling_tty, parent_pid}`.
- Produces:
  - `pub const HARNESS_SESSION_VARS: [&str; 2]`
  - `pub enum Identity { Resolved(String), Ambiguous(Vec<Candidate>) }` and `pub struct Candidate { pub var: &'static str, pub value: String }`, with `pub fn or_first(self) -> String`
  - `pub fn resolve_identity(as_flag: Option<&str>, env: &Env) -> Identity`
  - `pub fn identity(as_flag: Option<&str>) -> Identity`
  - `Env` gains `pub harness_sessions: Vec<Candidate>`
  - `pub fn harness_candidates() -> Vec<Candidate>` and `pub const HARNESS_ENV_PREFIXES: [&str; 2]`, both read by `doctor` in Task 9
  - `tests/common/testenv.rs`: `pub fn scrub_identity(cmd: &mut std::process::Command) -> &mut std::process::Command`

- [ ] **Step 1: Write the failing test**

Replace the `env` helper and add cases in `mod tests` in `crates/devkit-locks/src/ident.rs`. The helper gains a parameter, so update the existing four calls to pass `vec![]`:

```rust
    fn env(
        harness: Vec<(&'static str, &str)>,
        session: Option<&str>,
        pane: Option<&str>,
        tty: Option<&str>,
        ppid: Option<&str>,
    ) -> Env {
        Env {
            harness_sessions: harness
                .into_iter()
                .map(|(var, value)| Candidate {
                    var,
                    value: value.to_string(),
                })
                .collect(),
            devkit_session: session.map(str::to_string),
            tmux_pane: pane.map(str::to_string),
            tty: tty.map(str::to_string),
            ppid: ppid.map(str::to_string),
        }
    }

    #[test]
    fn harness_session_beats_devkit_session_and_tmux() {
        let e = env(
            vec![("CLAUDE_CODE_SESSION_ID", "sess-1")],
            Some("envsess"),
            Some("%3"),
            None,
            Some("42"),
        );
        assert_eq!(resolve_identity(None, &e), Identity::Resolved("sess-1".into()));
    }

    #[test]
    fn explicit_flag_still_beats_a_harness_session() {
        let e = env(
            vec![("CODEX_SESSION_ID", "sess-1")],
            None,
            None,
            None,
            Some("42"),
        );
        assert_eq!(resolve_identity(Some("flag"), &e), Identity::Resolved("flag".into()));
    }

    #[test]
    fn equal_harness_values_are_one_candidate() {
        let e = env(
            vec![
                ("CLAUDE_CODE_SESSION_ID", "sess-1"),
                ("CODEX_SESSION_ID", "sess-1"),
            ],
            None,
            None,
            None,
            Some("42"),
        );
        assert_eq!(resolve_identity(None, &e), Identity::Resolved("sess-1".into()));
    }

    #[test]
    fn two_distinct_harness_values_are_ambiguous() {
        let e = env(
            vec![
                ("CLAUDE_CODE_SESSION_ID", "outer"),
                ("CODEX_SESSION_ID", "inner"),
            ],
            None,
            None,
            None,
            Some("42"),
        );
        match resolve_identity(None, &e) {
            Identity::Ambiguous(c) => {
                let shown: Vec<String> = c.iter().map(|c| c.to_string()).collect();
                assert_eq!(
                    shown,
                    vec![
                        "CLAUDE_CODE_SESSION_ID=outer".to_string(),
                        "CODEX_SESSION_ID=inner".to_string()
                    ]
                );
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn an_explicit_flag_resolves_an_ambiguous_environment() {
        let e = env(
            vec![
                ("CLAUDE_CODE_SESSION_ID", "outer"),
                ("CODEX_SESSION_ID", "inner"),
            ],
            None,
            None,
            None,
            None,
        );
        assert_eq!(resolve_identity(Some("mine"), &e), Identity::Resolved("mine".into()));
    }
```

Then add the test that exercises the defect at the layer it surfaced, in `tests/locks.rs`. The unit tests above cover the resolver; this one covers the hook and the CLI agreeing, which is the whole point of the change:

```rust
/// Runs `lockm` as a harness session would: the vendor variable set, and every
/// other identity source stripped so the resolved holder can only have come
/// from detection.
fn run_as_session(exe: &Path, project: &Path, state: &Path, session: &str, args: &[&str]) -> Output {
    Command::new(exe)
        .args(args)
        .current_dir(project)
        .env("XDG_STATE_HOME", state)
        .env("HOME", state)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env("CLAUDE_CODE_SESSION_ID", session)
        .env_remove("CODEX_SESSION_ID")
        .env_remove("DEVKIT_SESSION")
        .env_remove("TMUX_PANE")
        .output()
        .expect("spawn lockm")
}

/// The enforced project both directions of this test need.
fn enforced_project() -> (tempfile::TempDir, std::path::PathBuf) {
    let proj = project();
    std::fs::write(
        proj.path().join("devkit.toml"),
        "[harness]\nenforce_writes = true\n",
    )
    .unwrap();
    let target = proj.path().join("src/a.rs");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    (proj, target)
}

#[test]
fn a_hook_held_lock_is_not_a_conflict_for_its_own_session() {
    let (_dir, link) = shimtest::linked("lockm");
    let state = tempfile::tempdir().unwrap();
    let (proj, target) = enforced_project();

    let h = run_hook(&link, proj.path(), state.path(), "sess-e2e", &target);
    assert!(!is_deny("hook claim", &h), "the hook's own first write is allowed");

    let a = run_as_session(&link, proj.path(), state.path(), "sess-e2e", &["acquire", "src/a.rs"]);
    assert!(
        a.status.success(),
        "a session must not conflict with the lock its own write hook took; stdout: {} stderr: {}",
        String::from_utf8_lossy(&a.stdout),
        String::from_utf8_lossy(&a.stderr)
    );
}

#[test]
fn a_cli_held_lock_does_not_deny_its_own_sessions_write() {
    let (_dir, link) = shimtest::linked("lockm");
    let state = tempfile::tempdir().unwrap();
    let (proj, target) = enforced_project();

    let a = run_as_session(&link, proj.path(), state.path(), "sess-e2e", &["acquire", "src/a.rs"]);
    assert!(a.status.success(), "claim succeeds: {}", String::from_utf8_lossy(&a.stderr));

    let h = run_hook(&link, proj.path(), state.path(), "sess-e2e", &target);
    assert!(
        !is_deny("own write", &h),
        "a session must not be denied a write to the path it claimed by hand"
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p devkit-locks -- ident`

Expected: FAIL to compile: no field `harness_sessions`, no type `Identity`.

Run: `cargo nextest run --test locks own_session -E 'test(/own_session|its_own/)'`, or simply `cargo nextest run --test locks`.

Expected: both new integration tests FAIL. `a_hook_held_lock_is_not_a_conflict_for_its_own_session` exits 1 with `held by sess-e2e`, because `lockm` resolved the parent pid instead. `a_cli_held_lock_does_not_deny_its_own_sessions_write` gets a deny decision for the same reason, in the other direction.

- [ ] **Step 3: Implement detection and the resolution result**

In `crates/devkit-locks/src/ident.rs`:

```rust
/// Environment variables carrying the harness's own session id, one per harness.
/// `CODEX_THREAD_ID` is excluded deliberately: it holds the same value as
/// `CODEX_SESSION_ID`, so listing it would manufacture a false ambiguity.
pub const HARNESS_SESSION_VARS: [&str; 2] = ["CLAUDE_CODE_SESSION_ID", "CODEX_SESSION_ID"];

/// One harness's answer to "which session is this": the variable it came from and
/// the value it held. The variable name travels with the value so a refusal can
/// tell the reader which harness contributed which id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub var: &'static str,
    pub value: String,
}

impl std::fmt::Display for Candidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}={}", self.var, self.value)
    }
}

/// The outcome of resolving a holder. `Ambiguous` carries the candidates whose
/// values differ, in table order; each value is a string a caller pastes back
/// as `--as`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    Resolved(String),
    Ambiguous(Vec<Candidate>),
}

impl Identity {
    /// The id to label read-only output with; the first candidate under ambiguity.
    pub fn or_first(self) -> String {
        match self {
            Identity::Resolved(s) => s,
            Identity::Ambiguous(c) => c
                .into_iter()
                .next()
                .map(|c| c.value)
                .unwrap_or_else(|| "unknown".into()),
        }
    }
}
```

Add the field to `Env` and populate it:

```rust
pub struct Env {
    pub harness_sessions: Vec<Candidate>,
    pub devkit_session: Option<String>,
    pub tmux_pane: Option<String>,
    pub tty: Option<String>,
    pub ppid: Option<String>,
}

impl Env {
    pub fn from_process() -> Self {
        let nonempty = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
        Env {
            harness_sessions: harness_candidates(),
            devkit_session: nonempty("DEVKIT_SESSION"),
            tmux_pane: nonempty("TMUX_PANE"),
            tty: devkit_common::sys::controlling_tty(),
            ppid: devkit_common::sys::parent_pid().map(|p| p.to_string()),
        }
    }
}
```

Rewrite the resolver:

```rust
/// Resolve the holder identity by precedence: `--as` > a harness session id >
/// `$DEVKIT_SESSION` > `$TMUX_PANE` > controlling tty > parent pid.
///
/// The harness id outranks `$DEVKIT_SESSION` because the write hook ignores that
/// variable outright, so any value visible under a harness already disagrees with
/// what enforcement decided; it outranks tmux and the tty because those name a
/// terminal rather than a session.
pub fn resolve_identity(as_flag: Option<&str>, env: &Env) -> Identity {
    if let Some(f) = as_flag {
        return Identity::Resolved(f.to_string());
    }
    let mut distinct: Vec<Candidate> = Vec::new();
    for c in &env.harness_sessions {
        if !distinct.iter().any(|d| d.value == c.value) {
            distinct.push(c.clone());
        }
    }
    match distinct.len() {
        0 => {}
        1 => return Identity::Resolved(distinct.remove(0).value),
        _ => return Identity::Ambiguous(distinct),
    }
    let fallback = env
        .devkit_session
        .clone()
        .or_else(|| env.tmux_pane.clone())
        .or_else(|| env.tty.clone())
        .or_else(|| env.ppid.clone())
        .unwrap_or_else(|| "unknown".to_string());
    Identity::Resolved(fallback)
}

pub fn identity(as_flag: Option<&str>) -> Identity {
    resolve_identity(as_flag, &Env::from_process())
}
```

Candidates dedupe on the *value*, not the variable: two harnesses reporting the same id is one session seen twice, not a conflict.

Add the two readers `doctor` needs, beside `HARNESS_SESSION_VARS`:

```rust
/// Environment prefixes every known harness stamps on its variables. A process
/// carrying one of these but none of `HARNESS_SESSION_VARS` is running under a
/// harness whose session variable devkit no longer recognises.
pub const HARNESS_ENV_PREFIXES: [&str; 2] = ["CLAUDE_CODE_", "CODEX_"];

/// The harness session ids visible to this process, in table order.
pub fn harness_candidates() -> Vec<Candidate> {
    HARNESS_SESSION_VARS
        .iter()
        .filter_map(|var| {
            std::env::var(var)
                .ok()
                .filter(|s| !s.is_empty())
                .map(|value| Candidate { var, value })
        })
        .collect()
}

/// True when some variable carries a known harness prefix.
pub fn harness_env_present() -> bool {
    std::env::vars_os().any(|(k, _)| {
        k.to_str()
            .is_some_and(|k| HARNESS_ENV_PREFIXES.iter().any(|p| k.starts_with(p)))
    })
}
```

- [ ] **Step 4: Keep `lib.rs` compiling on the read path**

In `crates/devkit-locks/src/lib.rs`, line 171 currently reads `holder: ident::identity(as_flag),`. Change it to:

```rust
        holder: ident::identity(as_flag).or_first(),
```

Task 7 replaces this with a real refusal on the mutating paths. Using `or_first` here keeps every caller compiling and behaving as before for a non-ambiguous environment, which is every environment outside a nested harness.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo nextest run -p devkit-locks -- ident`

Expected: PASS.

- [ ] **Step 6: Add the shared scrub helper**

Create `tests/common/testenv.rs`:

```rust
//! One place to strip the identity a developer's own session would otherwise
//! leak into a test's child process. Without this a run started from inside a
//! coding agent resolves a different default holder than CI does.

use std::process::Command;

/// Variables that would let an ambient session own a test's locks.
const IDENTITY_VARS: [&str; 4] = [
    "DEVKIT_SESSION",
    "TMUX_PANE",
    "CLAUDE_CODE_SESSION_ID",
    "CODEX_SESSION_ID",
];

pub fn scrub_identity(cmd: &mut Command) -> &mut Command {
    for var in IDENTITY_VARS {
        cmd.env_remove(var);
    }
    cmd
}
```

- [ ] **Step 7: Use it everywhere an identity variable is stripped by hand**

There are eight files and nine call sites: `tests/locks.rs` has two (`run` and `run_hook`), and `tests/brief_main_checkout.rs` has two. In each top-level test file, add the module declaration next to the existing `#[path = ...]` ones:

```rust
#[path = "common/testenv.rs"]
mod testenv;
```

`tests/common/baselinetest.rs` already lives in `tests/common/`, so its declaration is relative to that directory:

```rust
#[path = "testenv.rs"]
mod testenv;
```

Then replace the hand-written pair. In `tests/locks.rs` the `run` helper becomes:

```rust
fn run(exe: &Path, project: &Path, state: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .current_dir(project)
        .env("XDG_STATE_HOME", state)
        // Override HOME too: the binary runs migrate_legacy_state() at startup, which
        // reads $HOME/.claude/state/devkit. Pointing HOME at the throwaway temp dir
        // keeps the test from ever touching the developer's real state home.
        .env("HOME", state)
        .env("DEVKIT_SKIP_AUTOLINK", "1");
    testenv::scrub_identity(&mut cmd);
    cmd.output().expect("spawn lockm")
}
```

Apply the same shape to `run_hook` in the same file and to the command builders that currently call `.env_remove("DEVKIT_SESSION")` in `tests/mcp.rs`, `tests/cli_ergonomics.rs`, `tests/sync_includes.rs`, `tests/brief_main_checkout.rs` (two call sites), `tests/issue_setup_root.rs`, `tests/issue_bare_main_root.rs`, and `tests/common/baselinetest.rs`. Find them with `rg -n 'env_remove\("DEVKIT_SESSION"\)' tests/` and confirm the count afterwards with the same command, which must return nothing.

`run_as_session` and the two integration tests added in Step 1 stay as written: they set `CLAUDE_CODE_SESSION_ID` deliberately and strip the rest by hand, which is the same scrub with one variable put back.

- [ ] **Step 8: Run the whole gate**

Run: `cargo nextest run --workspace --no-fail-fast`

Expected: PASS. A failure naming an unexpected holder means a scrub site was missed.

- [ ] **Step 9: Commit**

```bash
cargo fmt --all
git add crates/devkit-locks/src/ident.rs crates/devkit-locks/src/lib.rs tests/
git commit -m "feat(locks): resolve the holder from the harness session"
```

---

### Task 7: Refuse an ambiguous identity on acquire and release

A Claude session running `codex exec` exposes both harnesses' variables with different values, and no fixed table order is right in both nesting directions. Guessing on acquire reproduces the original defect. Guessing on release is worse: `release_all` filters on root and holder alone, so a wrong id frees the outer session's rows in that root while it is alive and mid-task.

**Files:**
- Modify: `crates/devkit-locks/src/lib.rs` (`ctx`, `acquire`, `check`, `release`, `release_all`)
- Test: `tests/locks.rs`

**Interfaces:**
- Consumes: `ident::{identity, Identity}`.
- Produces: `pub struct AmbiguousIdentity { pub candidates: Vec<ident::Candidate> }`, erroring under ambiguity with both variables and their values named. Read paths keep using `identity(as_flag).or_first()`.

- [ ] **Step 1: Write the failing test**

Add to `tests/locks.rs`:

```rust
#[test]
fn nested_harness_refuses_to_guess_on_acquire_and_release() {
    let (_dir, link) = shimtest::linked("lockm");
    let proj = project();
    let state = tempfile::tempdir().unwrap();

    let nested = |args: &[&str]| {
        let mut cmd = Command::new(&link);
        cmd.args(args)
            .current_dir(proj.path())
            .env("XDG_STATE_HOME", state.path())
            .env("HOME", state.path())
            .env("DEVKIT_SKIP_AUTOLINK", "1");
        // Scrub first, then put back exactly the two variables under test.
        testenv::scrub_identity(&mut cmd);
        cmd.env("CLAUDE_CODE_SESSION_ID", "outer-session")
            .env("CODEX_SESSION_ID", "inner-session");
        cmd.output().expect("spawn lockm")
    };

    let a = nested(&["acquire", "src/a.rs"]);
    assert_eq!(a.status.code(), Some(2), "acquire refuses to guess");
    let text = String::from_utf8_lossy(&a.stderr);
    assert!(
        text.contains("CLAUDE_CODE_SESSION_ID=outer-session"),
        "names the variable and its value: {text}"
    );
    assert!(
        text.contains("CODEX_SESSION_ID=inner-session"),
        "names the variable and its value: {text}"
    );

    let r = nested(&["release", "--all"]);
    assert_eq!(r.status.code(), Some(2), "release refuses to guess");

    let s = nested(&["status"]);
    assert!(s.status.success(), "status is read-only and proceeds");

    let ok = nested(&["acquire", "src/a.rs", "--as", "inner-session"]);
    assert!(ok.status.success(), "--as resolves it");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run --test locks nested_harness_refuses_to_guess`

Expected: FAIL. `acquire` succeeds under the first candidate instead of exiting 2.

- [ ] **Step 3: Add a typed error and a strictness flag on `ctx`**

In `crates/devkit-locks/src/lib.rs`, add the error type. It is a named type rather than a bare `anyhow!` string so the CLI can recognise it by downcast, the way `devkit_config::NoConfig` is recognised elsewhere in this workspace:

```rust
/// Two harnesses are nested and expose different session ids. Refused rather than
/// guessed: the wrong id claims rows the inner hook will not recognise, and
/// `release --all` under it frees the outer session's rows while it is still live.
#[derive(Debug)]
pub struct AmbiguousIdentity {
    pub candidates: Vec<ident::Candidate>,
}

impl std::fmt::Display for AmbiguousIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shown: Vec<String> = self.candidates.iter().map(|c| c.to_string()).collect();
        write!(
            f,
            "ambiguous session identity: {} disagree; pass --as <id> to choose one",
            shown.join(" and ")
        )
    }
}

impl std::error::Error for AmbiguousIdentity {}
```

`ctx` at line 162 gains a strictness flag. Its four callers are `acquire` (182), `check` (225), `release` (251) and `release_all` (280); the three mutating ones pass `true`:

```rust
fn ctx(paths_in: &[String], as_flag: Option<&str>, strict: bool) -> Result<Ctx> {
    let cwd = std::env::current_dir().context("getting current dir")?;
    let root = find_root_from(&cwd);
    let mut paths = Vec::with_capacity(paths_in.len());
    for a in paths_in {
        paths.push(normalize_arg(a, &cwd, &root)?);
    }
    let holder = match (ident::identity(as_flag), strict) {
        (ident::Identity::Resolved(s), _) => s,
        (ident::Identity::Ambiguous(candidates), true) => {
            return Err(AmbiguousIdentity { candidates }.into());
        }
        (id @ ident::Identity::Ambiguous(_), false) => id.or_first(),
    };
    Ok(Ctx {
        root: root.to_string_lossy().into_owned(),
        holder,
        paths,
    })
}
```

Then update the four call sites: `ctx(paths_in, as_flag, true)` in `acquire`, `release` and `release_all`; `ctx(paths_in, as_flag, false)` in `check`. `status` derives no holder and is untouched.

- [ ] **Step 4: Exit 2 rather than 1 on the CLI**

In `src/bin/devkit/locks.rs`, the `Acquire` and `Release` arms propagate with `?`. Ambiguity is a usage error, not a conflict, and exit 1 already means "held by another session". Add the helper:

```rust
/// Ambiguity is a usage error: exit 2, leaving 1 to mean a real conflict.
fn exit_on_ambiguity(e: anyhow::Error) -> anyhow::Error {
    if let Some(a) = e.downcast_ref::<devkit_locks::AmbiguousIdentity>() {
        eprintln!("{a}");
        std::process::exit(2);
    }
    e
}
```

Apply it where those two arms build their result, for example in `Acquire`:

```rust
            let outcome = devkit_locks::acquire(&paths, holder.as_deref(), note.as_deref(), ttl)
                .map_err(exit_on_ambiguity)?;
```

and on both `devkit_locks::release_all(...)` and `devkit_locks::release(...)` in the `Release` arm.

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run --test locks && cargo nextest run -p devkit-locks`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/devkit-locks/src/lib.rs src/bin/devkit/locks.rs tests/locks.rs
git commit -m "feat(locks): refuse an ambiguous nested-harness identity"
```

---

### Task 8: Route the MCP server through the same resolution

`mint_holder` returns `mcp-<pid>` when `$DEVKIT_SESSION` is unset, so an agent claiming a file through `locks.acquire` collides with its own write hook exactly as `lockm` did. Ambiguity has to be deferred: `mint_holder` runs once into `ServerCtx::default_holder`, so raising it at startup would fail the server's construction and take every unrelated action down with it.

**Files:**
- Modify: `crates/devkit-mcp/src/lib.rs` (`mint_holder`, `ServerCtx`)
- Modify: `crates/devkit-mcp/src/locks.rs` (`resolve_holder`)
- Modify: `src/bin/devkit/mcp.rs`
- Test: `crates/devkit-mcp/src/locks.rs`

**Interfaces:**
- Consumes: `devkit_locks::ident::{identity, Identity}`.
- Produces: `ServerCtx.default_holder` becomes `devkit_locks::ident::Identity`; `resolve_holder(ctx: &ServerCtx, given: Option<String>) -> Result<String>` errors only when `given` is `None` and the stored identity is `Ambiguous`.

Changing the field's type breaks every construction of `ServerCtx`. There are five, and Step 3 must touch all of them:

| Site | Current value | Becomes |
|---|---|---|
| `src/bin/devkit/mcp.rs:18` | `devkit_mcp::mint_holder()` | unchanged; `mint_holder` returns the new type |
| `crates/devkit-mcp/src/lib.rs:161` | `"test-session".to_string()` | `Identity::Resolved("test-session".into())` |
| `crates/devkit-mcp/src/locks.rs:227` | `format!("mcp-locks-test-{}", std::process::id())` | `Identity::Resolved(format!("mcp-locks-test-{}", std::process::id()))` |
| `crates/devkit-mcp/src/actions.rs:98` | `"t".to_string()` | `Identity::Resolved("t".into())` |
| `crates/devkit-mcp/tests/issue_status_tracker.rs:50` | `"test-session".to_string()` | `Identity::Resolved("test-session".into())` |

Each of those four files needs `use devkit_locks::ident::Identity;` in scope, or the path written out in full. In `locks.rs` the import belongs at module level, not inside the test module: `resolve_holder` matches on `Identity` too.

- [ ] **Step 1: Write the failing test**

Add to `crates/devkit-mcp/src/locks.rs`:

```rust
#[cfg(test)]
mod holder_tests {
    use super::*;
    use devkit_locks::ident::{Candidate, Identity};

    fn candidates() -> Vec<Candidate> {
        vec![
            Candidate {
                var: "CLAUDE_CODE_SESSION_ID",
                value: "a".into(),
            },
            Candidate {
                var: "CODEX_SESSION_ID",
                value: "b".into(),
            },
        ]
    }

    fn ctx(id: Identity) -> ServerCtx {
        ServerCtx {
            default_holder: id,
            own_worktree: None,
        }
    }

    #[test]
    fn an_explicit_holder_wins_over_an_ambiguous_default() {
        let c = ctx(Identity::Ambiguous(candidates()));
        assert_eq!(resolve_holder(&c, Some("mine".into())).unwrap(), "mine");
    }

    #[test]
    fn an_ambiguous_default_fails_only_the_call_that_omits_holder() {
        let c = ctx(Identity::Ambiguous(candidates()));
        let e = resolve_holder(&c, None).unwrap_err().to_string();
        assert!(
            e.contains("CLAUDE_CODE_SESSION_ID=a") && e.contains("CODEX_SESSION_ID=b"),
            "names both: {e}"
        );
    }

    #[test]
    fn a_resolved_default_is_used_when_no_holder_is_given() {
        let c = ctx(Identity::Resolved("sess".into()));
        assert_eq!(resolve_holder(&c, None).unwrap(), "sess");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p devkit-mcp holder_tests`

Expected: FAIL to compile: `default_holder` is a `String`, and `resolve_holder` returns a `String` rather than a `Result`.

- [ ] **Step 3: Store the identity and defer the failure**

In `crates/devkit-mcp/src/lib.rs`:

```rust
/// The identity every lock action is taken under, resolved once at startup.
/// Stored unresolved so an ambiguous environment fails only the calls that omit
/// an explicit holder, rather than the server's construction.
pub fn mint_holder() -> devkit_locks::ident::Identity {
    devkit_locks::ident::identity(None)
}
```

Change `ServerCtx.default_holder` to `devkit_locks::ident::Identity`.

In `crates/devkit-mcp/src/locks.rs`:

```rust
fn resolve_holder(ctx: &ServerCtx, given: Option<String>) -> Result<String> {
    match (given, &ctx.default_holder) {
        (Some(h), _) => Ok(h),
        (None, Identity::Resolved(s)) => Ok(s.clone()),
        (None, Identity::Ambiguous(c)) => {
            let shown: Vec<String> = c.iter().map(|c| c.to_string()).collect();
            Err(anyhow::anyhow!(
                "ambiguous session identity: {} disagree; pass an explicit `holder` on this call",
                shown.join(" and ")
            ))
        }
    }
}
```

Every call site becomes `let holder = resolve_holder(ctx, a.holder)?;`.

Then update the four remaining `ServerCtx` constructions per the table above. `src/bin/devkit/mcp.rs` itself needs no edit: it already assigns `devkit_mcp::mint_holder()`, whose return type changes with it. Confirm none were missed with `rg -n "default_holder" crates/devkit-mcp src/bin/devkit/mcp.rs`.

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p devkit-mcp && cargo nextest run --test mcp`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/devkit-mcp/src/ src/bin/devkit/mcp.rs
git commit -m "feat(mcp): share the harness session identity"
```

---

### Task 9: A doctor row that reports an unrecognised harness

Nothing in CI can catch a vendor renaming a session variable, because a test exercises devkit's table against itself. The check cannot live in the hook either: a Codex hook process inherits `CODEX_HOME` and nothing else, so a hook flagging "payload id matches no candidate" would stamp a mismatch on every Codex write. `doctor` runs inside the tool command and therefore sees exactly what `lockm` sees.

What is detectable is narrower than comparing against the lock rows. Rows in a checkout belong to whichever sessions have written there, and a session that has not yet edited a file owns none of them — so "no row is mine" is the normal state for a second session in a shared checkout, and for a human at a plain terminal. It cannot be told apart from a genuine desync, and a row that warns in devkit's ordinary case is a row people learn to ignore.

The rename itself has a sharper signature. When a vendor renames its session variable, that variable leaves `HARNESS_SESSION_VARS` but the harness keeps stamping its other variables on the environment. So: a process carrying some `CLAUDE_CODE_`- or `CODEX_`-prefixed variable while none of the exact session variables is set is running under a harness devkit no longer recognises. A plain terminal carries neither and stays silent; a working harness sets the session variable and reads Ok. The lock rows are not consulted at all.

**Files:**
- Modify: `src/bin/devkit/doctor.rs`
- Test: `src/bin/devkit/doctor.rs`

**Interfaces:**
- Consumes: `devkit_locks::ident::{identity, harness_env_present, Identity}` (Task 6), the local `Check` and `Row` types.
- Produces: `fn harness_identity_check(id: Option<Identity>, harness_env: bool) -> Check`, and a `Row` keyed `harness_identity`.

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `src/bin/devkit/doctor.rs`:

```rust
    #[test]
    fn harness_identity_reports_a_detected_session() {
        let id = Some(devkit_locks::ident::Identity::Resolved("S".into()));
        assert!(matches!(harness_identity_check(id, true), Check::Ok(_)));
    }

    #[test]
    fn harness_identity_is_silent_outside_a_harness() {
        assert!(matches!(
            harness_identity_check(None, false),
            Check::Unset(_)
        ));
    }

    #[test]
    fn harness_identity_warns_when_a_harness_names_no_session_variable() {
        match harness_identity_check(None, true) {
            Check::Warn(m) => assert!(m.contains("CLAUDE_CODE_"), "names the prefixes: {m}"),
            other => panic!("expected Warn, got {other:?}"),
        }
    }

    #[test]
    fn harness_identity_warns_on_an_ambiguous_environment() {
        let id = Some(devkit_locks::ident::Identity::Ambiguous(vec![
            devkit_locks::ident::Candidate {
                var: "CLAUDE_CODE_SESSION_ID",
                value: "a".into(),
            },
            devkit_locks::ident::Candidate {
                var: "CODEX_SESSION_ID",
                value: "b".into(),
            },
        ]));
        match harness_identity_check(id, true) {
            Check::Warn(m) => assert!(m.contains("CODEX_SESSION_ID=b"), "names both: {m}"),
            other => panic!("expected Warn, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run --bin devkit harness_identity`

Expected: FAIL to compile: `harness_identity_check` is not defined.

- [ ] **Step 3: Implement the check and the row**

In `src/bin/devkit/doctor.rs`:

```rust
/// Report the session identity `lockm` will hold locks under. `id` is `None` when
/// no `HARNESS_SESSION_VARS` entry is set; `harness_env` says whether any variable
/// carries a known harness prefix. The two together are the rename signature: a
/// harness is present, but the variable naming its session is not one devkit knows.
fn harness_identity_check(id: Option<devkit_locks::ident::Identity>, harness_env: bool) -> Check {
    use devkit_locks::ident::{Identity, HARNESS_ENV_PREFIXES};
    match (id, harness_env) {
        (Some(Identity::Resolved(s)), _) => Check::Ok(s),
        (Some(Identity::Ambiguous(c)), _) => {
            let shown: Vec<String> = c.iter().map(|c| c.to_string()).collect();
            Check::Warn(format!(
                "{} disagree; `lockm acquire` and `lockm release` need an explicit --as",
                shown.join(" and ")
            ))
        }
        (None, true) => Check::Warn(format!(
            "a harness is present ({}) but sets no session variable devkit knows; \
             locks will be held under the parent pid and will not match the write hook",
            HARNESS_ENV_PREFIXES.join("*, ") + "*"
        )),
        (None, false) => Check::Unset("not running under a known coding agent"),
    }
}
```

Add the row to the `rows` vector, next to `devrun_strays`:

```rust
        Row {
            key: "harness_identity",
            source: Source::Unset,
            check: {
                let present = devkit_locks::ident::harness_env_present();
                let id = (!devkit_locks::ident::harness_candidates().is_empty())
                    .then(|| devkit_locks::ident::identity(None));
                harness_identity_check(id, present)
            },
        },
```

The row deliberately reads no lock rows. `identity(None)` is called only when a session variable is set, so a plain terminal's parent-pid fallback never reaches the Ok arm and is never reported as a detected session.

If `devkit-locks` is not already a dependency of the root package, add it to `Cargo.toml` under `[dependencies]` as `devkit-locks = { path = "crates/devkit-locks" }`. Check with `rg -n "devkit-locks" Cargo.toml` first; `src/bin/devkit/locks.rs` already imports it, so it will be there.

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run --bin devkit harness_identity`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add src/bin/devkit/doctor.rs
git commit -m "feat(doctor): report the resolved harness session identity"
```

---

### Task 10: Correct the documentation

Two claims in the skill are false today and true after this work, and one instruction actively causes the bug. The identity list appears in three places and all three must agree.

**Files:**
- Modify: `skills/using-devkit/SKILL.md`
- Modify: `skills/using-devkit/references/locks.md`
- Modify: `docs/commands.md`
- Modify: `src/bin/devkit/locks.rs` (the `--as` help text on `acquire`, `check` and `release`)
- Modify: `docs/agents.md`
- Modify: `AGENTS.md`

**Interfaces:**
- Consumes: nothing. Prose only.
- Produces: nothing.

- [ ] **Step 1: Fix the enforced-checkouts claim**

In `skills/using-devkit/SKILL.md`, under `## Enforced checkouts`, replace "Acquiring manually there is harmless and redundant, so the step above works in both modes." with:

```markdown
Acquiring manually there is redundant for `Edit`/`Write`, which the hook already covers, and it is safe: `lockm` resolves the same session id the hook does, so your own claims are recognised as your own. Claim by hand only for a `Bash` write the hook never sees.
```

- [ ] **Step 2: Remove the instruction that causes the bug**

In `skills/using-devkit/SKILL.md`, under `## Claim before you edit`, delete step 1 ("Set one stable holder id per session") and its `export DEVKIT_SESSION=...` block. Renumber the remaining steps. Replace with:

```markdown
**1. Look at the board, then claim everything in one call.** Your holder id is detected from the coding-agent session you are running in, so acquire and release already agree with each other and with the write hook. `acquire` is all-or-nothing: it claims every path, or if *any* is held it claims none and exits non-zero.
```

- [ ] **Step 3: Update the identity list**

In `skills/using-devkit/references/locks.md`, under `## Holder identity`, replace the numbered list and the sentence after it:

```markdown
1. `--as <id>` on the call
2. the coding-agent session id (`$CLAUDE_CODE_SESSION_ID`, `$CODEX_SESSION_ID`)
3. `$DEVKIT_SESSION`
4. `$TMUX_PANE`
5. the controlling tty
6. the parent pid

Inside a coding-agent session this needs no setup: the detected id is the one the write hook holds locks under, so a manual claim and an automatic one are the same holder. `$DEVKIT_SESSION` still applies outside one. When two harnesses are nested and expose different ids, `acquire` and `release` refuse rather than guess, and name both so you can pass `--as`.

A claim made by hand inside a sub-agent is recorded at session granularity, because no harness exposes a sub-agent id to a subprocess. It blocks every other session and does not block sibling sub-agents of your own; that isolation comes from the write hook, which does have sub-agent ids. For the same reason `lockm release --all` inside a sub-agent frees the whole session's manual claims.
```

- [ ] **Step 4: Update the command reference**

In `docs/commands.md`, replace the sentence beginning "Sessions identify themselves by (in priority order)":

Replace the whole paragraph, not just its first sentence: its closing advice to "pass a stable `--as`/`$DEVKIT_SESSION`" for non-interactive agent sessions is exactly the instruction this work removes.

```markdown
Sessions identify themselves by (in priority order) `--as <id>`, the coding-agent session id (`$CLAUDE_CODE_SESSION_ID`, `$CODEX_SESSION_ID`), `$DEVKIT_SESSION`, `$TMUX_PANE`, the controlling tty, or the parent pid. Inside a coding-agent session the detected id matches the one the write hook uses, so no setup is needed; when two harnesses are nested and disagree, `acquire` and `release` exit `2` naming both candidates. Conflicts fail fast: `acquire`/`check` exit `1` and report who holds the path. Locks expire after their TTL (default 30 min, `--ttl 0` disables) or when a recorded anchor pid dies; `release` frees them explicitly.
```

- [ ] **Step 5: Correct the remaining three places the old precedence is stated**

These are user-facing and now wrong. Find them with `rg -n 'DEVKIT_SESSION' src/bin/devkit/locks.rs docs/ AGENTS.md`.

In `src/bin/devkit/locks.rs`, the `--as` help on `acquire` (line 21), `check` (line 40) and `release` (line 53) each reads "Defaults to $DEVKIT_SESSION, then $TMUX_PANE ...". Replace all three with the same line, which stays ASCII because help text reaches the completion scripts verbatim:

```rust
    /// Holder id. Defaults to the coding-agent session, then $DEVKIT_SESSION,
    /// $TMUX_PANE, the controlling tty, the parent pid.
```

In `docs/agents.md` (line 24), "`holder` is a session identity minted from `$DEVKIT_SESSION` (or a per-process id)" becomes "`holder` is a session identity detected from the coding-agent session, falling back to `$DEVKIT_SESSION` or a per-process id".

In `AGENTS.md` (line 121), the `lockm` bullet's "Always pass a consistent `--as <id>` (or set `$DEVKIT_SESSION`) so acquire and release refer to the same holder" is no longer needed inside a coding agent. Replace it with: "Inside a coding-agent session the holder is detected, so acquire and release already agree; pass `--as <id>` only outside one, or when devkit reports two nested harnesses disagreeing."

- [ ] **Step 6: Run the full gate**

Run: `cargo nextest run --workspace --no-fail-fast && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --doc`

Expected: PASS, all three.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add skills/using-devkit/ docs/commands.md docs/agents.md AGENTS.md src/bin/devkit/locks.rs
git commit -m "docs(locks): describe detected holder identity"
```

---

## Verification after the last task

Run the whole gate one more time from a clean tree, then confirm the original defect is gone by hand in an enforced checkout:

```bash
cargo nextest run --workspace --no-fail-fast
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
lockm status --all
```

Edit a file through the agent's `Edit` tool, then run `lockm acquire <that same path>` with no `--as`. It must succeed rather than report a conflict. Then run `devkit doctor` and confirm the `harness_identity` row reads Ok with your session id, and that the id it prints is byte-identical to the holder `lockm status` shows on the row the hook took.

## Known limitations, by design

- A manual claim inside a sub-agent is session-granular and does not isolate siblings. No harness exposes a sub-agent id to a subprocess.
- Nested harnesses remain broken on the hook write path: an outer session's row and an inner session's writer are not on one ancestry line, and nothing available can relate two harnesses' session ids.
- `doctor` reports which id this session resolves, not whether it matches the rows in the checkout. A session that has not yet written owns no rows, and neither does a human at a terminal, so "no row is mine" carries no information. What the row does catch is a harness present in the environment that names no session variable devkit knows, which is the shape a vendor rename takes.
- Cursor is untouched. Its manifest registers no write or lifecycle hooks, so there is no holder for `lockm` to agree with.
