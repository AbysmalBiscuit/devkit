# Issue status events

Issue: [#65](https://github.com/AbysmalBiscuit/devkit/issues/65).

## Problem

An issue's tracker status says nothing about whether anyone is working on it. When an agent starts on an issue, or opens its PR, the issue stays wherever it was until a person moves it by hand. GitHub issues have no "in progress" state at all: devkit maps every open one to `Started` (`tracker/github.rs`, `map_state`), and the status a person reads lives in a Projects v2 field.

## Goal

devkit moves an issue's tracker status when devkit itself does something to that issue, as the project configures:

- `issue setup` creates the issue's worktree.
- The first agent session starts in that worktree.
- `issue pr create` opens the issue's PR.

Each event moves the issue only from the states the config allows, to the state it names. With no config, events do nothing and devkit writes nothing to any tracker.

## Out of scope

- Merges and closes. The forge sees them, devkit does not, and both trackers already move issues on them.
- Events that run tasks or commands. `[hooks]` runs commands on lifecycle events; `[issue.events]` only changes status.
- Linear's own GitHub integration. devkit leaves it alone; the `from` lists keep the two from undoing each other.
- An MCP action that changes status.

## Design

### Config

```toml
[issue.events.setup]
to = "Todo"

[issue.events.start]
from = ["", "Todo", "Backlog"]
to = "In progress"

[issue.events.pr_open]
to = "In review"

[github]
project = 3
status_field = "Status"
```

- `[issue.events]` holds one optional table per event. The event names are fixed: `setup`, `start` and `pr_open`. An unknown name fails to parse, so a misspelled event is a config error rather than an event that never fires. An absent table means the event does nothing, and every table is absent by default.
- `to` is required: the status the event moves the issue to.
- `from` lists the statuses the event may move the issue from, `["*"]` by default, where `*` matches any status. The empty string matches an issue with no status: a GitHub issue that is not in the project, or whose field is unset. Names match case-insensitively, since GitHub boards and Linear teams spell the same state differently.
- An issue already at `to` is left alone whatever `from` says.
- `[github] project` is the number of the Projects v2 project that holds the status, owned by the owner of `issues_repo`. `[github] status_field` is its single-select field, `Status` by default. Linear needs no keys: its states belong to the issue's team, which devkit already resolves.

`IssueConfig`, `IssueEventsConfig` and `EventTransition` live in `devkit-config` with doc comments that become the schema, and a doctest example on `IssueConfig`. The schema description of `from` recommends listing the start states for `start`, since `["*"]` lets a second worktree pull an issue back from review.

### Writing status

The `Tracker` trait stays read-only: its contract says `devkit-issue` never mutates a tracker, and the MCP server and the triage commands hold trackers. Status writes get their own trait in `devkit-common::tracker`:

```rust
pub trait StatusWriter {
    /// The issue's current status name, `None` when it has none.
    fn status(&self, id: &str) -> Result<Option<String>>;
    /// Move the issue to the status named `to`. An unknown name is an error
    /// that lists the names the tracker has.
    fn set_status(&self, id: &str, to: &str) -> Result<()>;
}
```

- **GitHub:** finds the issue's item in the project. When the issue has none, `set_status` adds it to the project first. It then sets the field to the option named `to`. Each call goes through `cmd::gh_json_in`, the path every repository-scoped `gh` call takes.
- **Linear:** reads the issue's team's workflow states and sets the issue's state to the one named `to`.
- **No tracker:** there is no writer. A configured event warns once that it cannot fire.

The binary holds the writers in an enum dispatched with ambassador, like `vcs::Vcs`.

### Firing an event

`devkit issue status --event <event> [ISSUE]` fires one event. It resolves the issue from the current worktree's record when no issue is given. It reads the current status, applies `from` and the already-at-target rule, and writes. Each of the three triggers ends in this code path, and a person can run it by hand to retry or to see an error.

| Event | Trigger | How it runs |
|---|---|---|
| `setup` | `issue setup`, after the worktree is created and reported | inline; a failure warns and setup succeeds |
| `start` | the SessionStart hook, in an issue worktree, the first time | a detached `devkit issue status --event start`; the hook returns at once |
| `pr_open` | `issue pr create`, after the PR is open | inline; a failure warns and the PR stays open |

`issue pr checkout` fires nothing: a reviewer's checkout is not the start of the work.

### Firing once per worktree

`IssueRecord` gains `events`, the events already fired for that worktree. An event that is in it is skipped; one that fires is added before the write. The marker is added before the write, and a failed write does not remove it, so a tracker that is down costs one missed transition, never a retry on every session. A person reruns the command to recover. `issue end` removes the record, and the marker with it. The record stores no events on worktrees set up before the field existed, so their next session fires `start` once.

The SessionStart hook reads the record, which is a local file, and spawns the detached process only when `start` is configured and not yet fired. It starts the run through `devkit_common::sys::spawn_background`, as the todo backend's background sync does. A worktree with no issue, or whose issue `pr checkout` could not resolve, fires nothing.

### Failure

- **Hooks:** no verdict, no stdout, never exit 2, the rule for every hook. The hook does no network IO. A failed background run is written to devkit's log.
- **CLI:** a status name the tracker lacks, a missing project, a field that is not single-select, or a token without access is an error that names the config key. `issue setup` and `issue pr create` print it as a warning and succeed.
- **Token scope:** GitHub Projects needs the `project` scope, which `gh auth login` does not grant by default (`gh project --help`: "The minimum required scope for the token is: `project`"). The error says to run `gh auth refresh -s project`.

### `devkit doctor`

When any event is configured, an `issue events` row lists each event's transition and checks that the writer can reach the tracker. On GitHub it checks that the project exists, that `status_field` is a single-select field holding every `to` and `from` name, and that the token has the `project` scope.

### Documentation

`plugin/skills/using-devkit/references/issues.md` gains a section on status events: the three events and when each fires, the config, the empty-string status, firing once per worktree and rerunning by hand, and the GitHub token scope.

## Testing

- **GitHub writer** against `tests/common/ghfake.rs`: sets the option on an existing item; adds a missing item first; an unknown name is an error listing the options; a field that is not single-select is an error naming `status_field`.
- **Linear writer** against faked responses: sets the state by name within the team; an unknown name lists the team's states.
- **Transition rule:** `from` match, `*`, the empty string for no status, case-insensitive names, and no write when the issue is already at `to`.
- **Hook:** the first SessionStart in an issue worktree with `start` configured spawns one run and records the event; a second session spawns nothing; a worktree with no issue spawns nothing; with no event configured nothing is spawned; the hook's output is unchanged when the run fails.
- **CLI:** `issue setup` and `issue pr create` fire their events and stay successful when the write fails.
- **Config:** an unknown event name fails to parse; the schema is regenerated.
- **Doctor:** the row on a configured project, and the scope check.

## Unresolved

- Whether one GraphQL query can read the item, the field and its options together, or whether finding the item takes a second round trip. The plan checks the GitHub schema.
