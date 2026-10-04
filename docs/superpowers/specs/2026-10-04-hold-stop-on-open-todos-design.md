# Hold an agent to its open todos at Stop

Issue: #215

## Problem

An agent can end its turn with work left on its todo list. Compaction makes it likelier: the agent loses the thread, and although `devkit todo context` re-injects the lists at PostCompact, nothing stops the agent from ending the turn anyway. The harnesses' own goal and loop tools are per-harness; devkit's hooks already run in every harness devkit supports.

Claude Code and Codex both let a Stop or SubagentStop hook refuse the stop: a `{"decision":"block","reason":"<text>"}` response turns the reason into the agent's next prompt, and each payload carries `stop_hook_active` so a hook can tell it is blocking again. Cursor's `stop` takes `followup_message`, submitted as the next user message.

## Behaviour

`devkit hook stop` and `devkit hook subagent-stop` refuse the stop while the agent has **open todos** it has not been reminded of.

**Open todos**

- Main session (`stop`): pending todos on the session's own node, plus in-progress todos the session itself holds. A todo a sub-agent holds does not count, so a background sub-agent never traps the main turn.
- Sub-agent (`subagent-stop`): in-progress todos that sub-agent holds.

**One reminder per unchanged list.** A block records a fingerprint of the open todos (each id with its status) for that holder. A later stop whose open todos match the fingerprint is allowed: the agent saw the list and chose to stop, for example to ask the user something. A stop whose open todos differ (one finished, one added) can block again. Every block therefore needs the list to have changed since the previous one, which bounds any loop, including one where a permission denial keeps the agent from making progress.

**The reason** lists the open todos, rendered as `devkit todo context` renders them so the ids match, and tells the agent to finish each one or cancel any that no longer applies, and that ending the turn again is how to stop while waiting on the user.

**Never blocks:**

- a Codex `Interrupt`, which the Codex manifest routes to the same `stop` verb;
- `[todo] hold_stop = false`;
- a harness or event with no block response (Cursor `subagentStop`, Antigravity);
- any failure to read the config, the payload or the store. The hook fails open: it prints nothing and the agent stops.

**Sub-agent claims.** `subagent-stop` releases the sub-agent's claims. The hold runs before the release, and a blocked sub-agent keeps its claims, since it is not stopping.

**Config.** `[todo] hold_stop`, a bool defaulting to `true`. Its doc comment is its schema description.

## Design

### pabal

`Stop` and `SubagentStop` views gain `block(reason: &str) -> Option<Response>`:

| Harness | `Stop` | `SubagentStop` |
|---|---|---|
| Claude Code | `{"decision":"block","reason":"<text>"}` | same |
| Codex | `{"decision":"block","reason":"<text>"}` | same |
| Cursor | `{"followup_message":"<text>"}` | `None` |
| Antigravity | `None` | not an event |

`None` is a harness or event pabal cannot block. Cursor documents `subagentStop`'s follow-up as starting the next iteration after the sub-agent completes, not as keeping the sub-agent running, so it is not a block. This ships as a pabal release that devkit then depends on.

### devkit

- `hook/todo.rs` gains `hold`, beside `release` and `capture`: given the payload, the holder, the checkout and the cwd, it returns the reason to block with, or `None`. The open-todo rule is a pure function over the listed todos, the holder and the node, so it is unit-testable without a store.
- The fingerprint is one file per holder under `devkit_todo::state_dir()`. `session-end` removes the ending session's files alongside its claim release.
- `hook/mod.rs`: the `Stop` arm and the `SubagentStop` arm call `hold` first. A reason is written to stdout through pabal's `block`; the `SubagentStop` arm releases claims only when it did not block. Recording runs after the verdict and cannot change it.
- The `hook/mod.rs` module docs and the **Hooks** rule in `AGENTS.md` change from "only `pre-tool-use` writes stdout" to naming `pre-tool-use`, `stop` and `subagent-stop`.
- `plugin/skills/using-devkit/references/todo.md` documents the hold: when a stop is refused, and that stopping again with the list unchanged goes through.

## Testing

End to end through `devkit hook stop` and `devkit hook subagent-stop`, with a real payload on stdin and a builtin store in a tempdir state directory:

1. Open todos block, and the reason lists them.
2. A second stop with the same open todos is allowed.
3. After one todo finishes, a stop blocks again on the rest.
4. No open todos: allowed.
5. `hold_stop = false`: allowed.
6. A Codex `Interrupt`: allowed.
7. A blocked sub-agent keeps its claim; an allowed one releases it.
8. A Cursor `subagentStop`: allowed.
9. An unreadable store: allowed.

pabal's own tests pin each harness's `block` output.

## Out of scope

- A declared goal with a check command (`devkit goal set ... --until ...`). Open todos are the signal; a hard check can layer on later.
- A "waiting on human" todo status.
- Scripts and narrow allow rules for issue-manage's push and PR steps, which the issue also raised; those live in the skill, not in devkit.
