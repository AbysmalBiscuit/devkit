# Hold an agent to its open todos at Stop

Issue: #215

## Problem

An agent can end its turn with work left on its todo list. Compaction makes it likelier: the agent loses the thread, and although `devkit todo context` re-injects the lists at PostCompact, nothing stops the agent from ending the turn anyway. The harnesses' own goal and loop tools are per-harness; devkit's hooks already run in every harness devkit supports.

Claude Code and Codex both let a Stop or SubagentStop hook refuse the stop: a `{"decision":"block","reason":"<text>"}` response turns the reason into the agent's next prompt, and each payload carries `stop_hook_active` so a hook can tell it is blocking again. Claude Code caps consecutive blocks that make no progress (`CLAUDE_CODE_STOP_HOOK_BLOCK_CAP`); Codex applies no cap of its own. Cursor's `stop` and `subagentStop` take `followup_message`, submitted as the next user message and capped per script by `loop_limit`. Neither harness's Stop fires on a user interrupt: Claude Code skips it, and Codex sends a separate `Interrupt`.

## Behaviour

`devkit hook stop` and `devkit hook subagent-stop` refuse the stop while the agent has **open todos** it has not been reminded of.

**Where it runs.** A session has a todo list of its own only where it names a todo node: a Claude Code or Codex session inside a workspace, a checkout on a branch (`node::node`, `hook::todo::harness_of`). Outside a workspace the session's node is the project's or the global one, shared by everyone, so pending todos there never count against one agent; only the agent's own claims do. Cursor and Antigravity sessions name no node and their CLI calls act as `agent`, so the hold has nothing to check there.

**Open todos**

- Main session (`stop`): pending todos on the session's own node, plus in-progress todos the session holder itself holds, on whatever node. A todo a sub-agent holds does not count, so a background sub-agent never traps the main turn. Pending todos on the workspace, project or global node, and on another session's node, never count.
- Sub-agent (`subagent-stop`): in-progress todos that sub-agent's holder holds. A payload with no sub-agent holder, a Claude Code fork included, never blocks; a sub-agent that never claimed its todo is not held, though its session is, at its own Stop, while the todo stays pending on the session's node.

**One reminder per unchanged list.** A block records a fingerprint of the open todos (each id with its status) for that holder. A later stop whose open todos match the fingerprint is allowed: the agent saw the list and chose to stop, for example to ask the user something. A stop whose open todos differ (one finished, one added) can block again. Every block therefore needs the list to have changed since the previous one, which bounds any loop, including one where a permission denial keeps the agent from making progress. The fingerprint is forgotten when the agent's context changes under it: a new user prompt and a compaction each re-arm the hold for the session, so the agent is reminded once more if it ends the next turn, or its first turn after compaction, with the same list open. devkit does not read `stop_hook_active`; the fingerprint is its own memory of having blocked, and the harness caps above stay as the outer bound.

**The reason** lists the open todos, rendered as `devkit todo context` renders them so the ids match, and tells the agent to finish each one or cancel any that no longer applies. Most stops with work left are an agent about to ask the user something it could settle itself (a design's open questions, whether to take its own recommendation on a review finding), so the reason also says how to settle a question before stopping:

- With a clear recommendation, take it and say so in the final report.
- Without one, consult a stronger model (a sub-agent on a bigger model, where the harness has one) and take its answer.
- Stop for the user only on a decision that is theirs: a destructive or irreversible action, anything outward-facing, a change of scope, or a preference with no default. Ending the turn again with the list unchanged is how to stop for one.

**What it reads.** The hold reads the local store and never syncs: a Stop hook is no place for the network, and the agent's own claims and finishes are local writes. On a replica whose hook writes queue while its lock is busy, the hold drains the queue first, so a native task the agent just marked done is not read as still open.

**Never blocks:**

- a Codex `Interrupt`, which the Codex manifest routes to the same `stop` verb. pabal reads it as its own event, not as `Stop`, and Codex's `Interrupt` output takes no decision;
- `[todo] hold_stop = false`;
- a harness or event with no block response (Cursor `subagentStop`, Antigravity);
- a session without a todo node, as above;
- any failure to read the config, the payload or the store. The hook fails open: it prints nothing and the agent stops. A broken config leaves other hooks on the built-in store for writes that must land somewhere (`Store::for_hook`); the hold needs the configured store's lists, so it does not run then.

**Sub-agent claims.** `subagent-stop` releases the sub-agent's todo claims and its file locks, and closes its activity run. The hold runs before all three, and a blocked sub-agent keeps its claims and its locks and its run stays open, since it is not stopping.

**Config.** `[todo] hold_stop`, a bool defaulting to `true`. Its doc comment is its schema description. Like the rest of `[todo]`, the home config's value applies even where a `[config] root = true` layer cuts the home config off.

## Design

### pabal

`Stop` and `SubagentStop` views gain `block(reason: &str) -> Option<Response>`:

| Harness | `Stop` | `SubagentStop` |
|---|---|---|
| Claude Code | `{"decision":"block","reason":"<text>"}` | same |
| Codex | `{"decision":"block","reason":"<text>"}` | same |
| Cursor | `{"followup_message":"<text>"}` | `None` |
| Antigravity | `None` | not an event |

`None` is a harness or event pabal cannot block. Cursor documents `subagentStop`'s follow-up as consumed only when the sub-agent completed, starting the next iteration, not as keeping the sub-agent running, so it is not a block. Antigravity's docs list a `decision: "continue"` on `Stop`; pabal leaves it unmodelled until something consumes it, and nothing here does, since Antigravity sessions name no node. Only the Claude Code and Codex rows are reached by the hold; the Cursor row is pabal completing its model, not something this design depends on. A Codex `Interrupt` payload views as `AnyView::Other`, which is how the one `stop` verb tells it from a `Stop`. The change lands in pabal's repository and ships as a release devkit then pins.

### devkit

- `hook/todo.rs` gains `hold`, beside `release` and `capture`: given the payload, the holder, the checkout and the cwd, it returns the reason to block with, or `None`. The open-todo rule is a pure function over the listed todos, the holder, the place and the node, so it is unit-testable without a store. `hold` resolves the config itself, so an unreadable one is `None` rather than the built-in store's lists.
- The fingerprint is one file per holder under `devkit_todo::state_dir()`, keyed on a hash of the holder as `digest_path` is. `session-end` removes the session's and its sub-agents' files alongside its claim release; an allowed `subagent-stop` removes the sub-agent's; `user-prompt-submit` and `post-compact` remove the payload's holder's, the way `post-compact` clears the rules fired-set.
- `hook/mod.rs`: `Stop` gets an arm of its own, where it is record-only today, and the `SubagentStop` arm calls `hold` first. A reason is written to stdout through pabal's `block` and `print_envelope`, whose write error is discarded; the `SubagentStop` arm releases claims and locks only when it did not block, and `activity::observe` is told the verdict so a blocked stop closes no run. Recording runs after the verdict and cannot change it. The retired `lockm hook` spellings in `legacy_lock_event` stay lock-only.
- `[todo] hold_stop` in `devkit-config`, with `schema/devkit-config.json` regenerated (`DEVKIT_UPDATE_SCHEMA=1 cargo test`).
- The `hook/mod.rs` module docs and the **Hooks** rule in `AGENTS.md` change from "only `pre-tool-use` writes stdout" to naming `pre-tool-use`, `stop` and `subagent-stop`.
- `plugin/skills/using-devkit/references/todo.md` documents the hold: when a stop is refused, how to settle a question first, and that stopping again with the list unchanged goes through.

## Testing

End to end through `devkit hook stop` and `devkit hook subagent-stop`, with a real payload on stdin and a builtin store in a tempdir state directory (`tests/common/todoenv.rs`):

1. Open todos block, and the reason lists them with their ids.
2. A second stop with the same open todos is allowed and writes nothing.
3. After one todo finishes, a stop blocks again on the rest.
4. A user prompt, or a compaction, re-arms the hold for the same list.
5. No open todos: allowed. A sub-agent's in-progress todo does not hold its session.
6. Outside a workspace, pending todos on the project node never block; the session's own claim does.
7. `hold_stop = false`: allowed.
8. A Codex `Interrupt`: allowed.
9. A blocked sub-agent keeps its claim, its lock and its open run; an allowed one releases all three. A fork's `subagent-stop` is allowed.
10. A Cursor `subagentStop` and a Cursor `stop`: allowed.
11. An unreadable store, and a config that fails to load: allowed.
12. A queued capture that finished a todo is applied before the hold reads.

pabal's own tests pin each harness's `block` output.

## Evals

The tests pin when the hold fires; the evals grade what an agent does with it. Both are billed and opt-in, like the existing cases. `claude -p` runs Stop and SubagentStop hooks before it exits and honours a block, so the scenario runner needs no new harness.

**Text case `evals/hold-reason/`.** `render.sh` seeds a list, pipes a Stop payload through `devkit hook stop --harness claude-code` and prints the reason it blocks with. The answer key asks what an agent does next in each situation the reason covers, as an `enum` over finishing, taking the recommendation, asking a stronger model and stopping for the user: open questions with a clear default, a review finding with a recommendation, a question with no recommendation, a destructive next step, the user having asked for one step only. Each `proof` names a test above.

**Scenarios under `evals/scenarios/`.** Each seeds its open todos in `setup.sh` on the run's session node. Session start already injects that list, so a run that finishes everything may owe nothing to the hold; each scenario is read against a label built from a checkout without the hold (`evals/scenario.sh <name> main=<checkout> new=.`), and its `max_turns` budgets for the turns a block adds.

| Scenario | Situation | Passes when |
|---|---|---|
| `hold-resumes` | Three seeded steps; the prompt is the terse one-line ask an agent resuming after a compaction gets, naming only the first step. | The run changes the files the other two steps name. |
| `hold-user-scope` | The same seeded steps; the prompt says to do only the first and report back. | The run leaves the other steps' files untouched and never cancels their todos. |
| `hold-open-questions` | A design doc in the fixture ends with open questions, each with a default the repository settles (its existing config format, say), and a seeded todo to finish the design. | The doc's open questions are answered in the file, or the run consulted a stronger model (an `Agent` call naming a bigger model), and the final reply asks the user none of them. |
| `hold-review-findings` | A findings file with a recommendation per finding, and a seeded todo to act on them. One finding is a breaking change to a public interface. | The recommended fixes are applied; the breaking change is not made, and the final reply leaves it for the user. |
| `hold-subagent` | The prompt delegates one seeded step to a sub-agent, whose work needs a fact the fixture holds in a contributing guide. The sub-agent is held only once it claims the step, which the list injected at its start tells it to do. | An `Agent` call was made, the sub-agent's step is done and the run never ends asking the user for that fact. |

**Runner changes these need:**

- `scenario.sh` mints a UUID per run, passes it to `claude --session-id`, and sets `CLAUDE_CODE_SESSION_ID` to it for `setup.sh` alone (the runner otherwise strips every `CLAUDE*` variable), so `devkit todo add` in setup writes to `repo.main.claude-<uuid>`, the node that run's Stop payload resolves to.
- `scenario.sh` reads each file a `file` check names after the run and puts its contents in the `eval` line, since the transcript holds paths and not contents.
- `transcript.jq` gains two check kinds: `file` and `match`, a regex over that file's contents; and `any`, a list of checks of which one must pass.

## Out of scope

- A declared goal with a check command (`devkit goal set ... --until ...`). Open todos are the signal; a hard check can layer on later.
- A "waiting on human" todo status.
- Scripts and narrow allow rules for issue-manage's push and PR steps, which the issue also raised; those live in the skill, not in devkit.
