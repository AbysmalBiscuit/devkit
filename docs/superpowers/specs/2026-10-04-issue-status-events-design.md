# Issue status events

Issue: [#65](https://github.com/AbysmalBiscuit/devkit/issues/65).

## Problem

An issue's tracker status says nothing about whether anyone is working on it. When an agent starts on an issue, or opens its PR, the issue stays wherever it was until a person moves it by hand. GitHub issues have no "in progress" state at all: devkit maps every open one to `Started` (`tracker/github.rs`, `map_state`), and the status a person reads lives in a Projects v2 field.

## Goal

devkit moves an issue's tracker status when devkit itself does something to that issue, as the project configures:

- `issue setup` creates the issue's worktree.
- The first agent session starts in that worktree.
- `issue pr create` opens or reuses the issue's PR.

Each event moves the issue only from the states the config allows, to the state it names. With no config, events do nothing and devkit writes nothing to any tracker.

## Out of scope

- Merges and closes. The forge sees them, devkit does not, and both trackers already move issues on them.
- `issue pr ready`. A draft-first flow reaches the `pr_open` status when the draft is created; a separate ready event can follow if that proves too early.
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
project = 3            # or "some-org/7"
status_field = "Status"
```

- `[issue]` is a new top-level table. `IssueConfig` and `IssueEventsConfig` are `deny_unknown_fields`, so the event names are fixed (`setup`, `start`, `pr_open`) and a misspelled one is a parse error rather than an event that never fires. An absent event table means the event does nothing, and every table is absent by default.
- `EventTransition` holds `to`, required, and `from`, `["*"]` by default. `*` matches any status, the empty string matches an issue with no status (a GitHub issue outside the project, or with the field unset), and names match case-insensitively. An issue already at `to` is left alone whatever `from` says.
- `[github] project` names the Projects v2 project that holds the status: `"owner/N"`, or a bare `N` owned by `issues_repo`'s owner, since an organization's project over a personal repository is common. `[github] status_field` is its single-select field, `Status` by default. `GithubConfig`'s doc comment widens from "the repository holding the issues" to cover both keys, and the test helpers that build `GithubConfig` literally gain them. Linear needs no keys: its states belong to the issue's team.
- `from` and `to` name statuses on one project's board, so they belong in the repository's `devkit.toml`. The doc comments say so: tables merge key by key across layers, so splitting an event between the home config and the project is legal but names one board's statuses against another's.

Each key's meaning lives in its doc comment, which becomes the schema; `IssueConfig` carries the doctest example. The doc comment on `start`'s `from` recommends listing the start states, since `["*"]` lets a second worktree pull an issue back from review.

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

The binary holds the writers in an enum dispatched with ambassador, like `vcs::Vcs`.

**GitHub.** The writer holds a `github::Api` for `issues_repo`'s host and sends its query and mutations through `Api::graphql`, taking the same `gh api graphql --hostname` fallback `GithubTracker` takes when no token resolves. Nothing here is repository-scoped, so `cmd::gh_json_in` is not involved. One query reads everything:

```graphql
query {
  repository(owner: $o, name: $n) {
    issue(number: $i) {
      id
      projectItems(first: 20, includeArchived: true) {
        nodes {
          id
          project { id number }
          fieldValueByName(name: $f) { ... on ProjectV2ItemFieldSingleSelectValue { name optionId } }
        }
      }
    }
    owner {
      ... on ProjectV2Owner {
        projectV2(number: $p) {
          id
          field(name: $f) { ... on ProjectV2SingleSelectField { id options { id name } } }
        }
      }
    }
  }
}
```

`User` and `Organization` both implement `ProjectV2Owner`, so the fragment resolves either kind of owner. A project named `"owner/N"` reads `repositoryOwner(login:)` in place of `repository.owner`. The write is `updateProjectV2ItemFieldValue(input: {projectId, itemId, fieldId, value: {singleSelectOptionId}})`, preceded by `addProjectV2ItemById(input: {projectId, contentId})` when the issue has no item in the project: one round trip to read, one or two to write. A missing field, or one that is not single-select, comes back null or as another union member, which is the error naming `status_field`.

**Linear.** One query, `issues(filter: {team, number}) { nodes { id state { name } team { states { nodes { id name } } } } }`, yields the issue's UUID, its current state and its team's states; `issueUpdate(id: <uuid>, input: {stateId})` writes. Two round trips. An id that `parse_id` rejects is an error naming it.

**No writer.** With tracker kind `none`, or GitHub with no `[github] project`, there is no writer. The CLI run is an error naming the missing key, the inline callers print it as a warning, the background run logs it, `devkit doctor`'s row reports it, and the hook spawns nothing when the resolved tracker kind is `none`.

### Firing an event

`devkit issue event <setup|start|pr_open> [ISSUE]` fires one event. `issue status` stays the read-only report. With no issue given, the command resolves it from the current worktree's record. It reads the current status, applies `from` and the already-at-target rule, and writes. Each trigger ends in this code path, and a person can run it by hand to retry or to see an error.

| Event | Trigger | How it runs |
|---|---|---|
| `setup` | `issue setup`, as its last step, after `after_worktree_create` hooks | inline; a failure warns and setup succeeds |
| `start` | the SessionStart hook, in a qualifying worktree, the first time | a detached `issue event start`; the hook returns at once |
| `pr_open` | `issue pr create`, once `ensure` returns, whether it created the PR or reused one | inline; a failure warns and the PR stays as it is |

### Firing once per worktree

`IssueRecord` gains two fields:

- `events`: the events already fired for this worktree.
- `origin`: `setup` or `checkout`, written by `issue setup` and `issue pr checkout`. Absent on records written before it existed.

`record` gains `update(worktree, |rec| ...)`: a read-modify-write under an `fd_lock` on `.devkit/issue.lock`, the pattern `store.rs` already uses. Every existing updater moves to it: `pr create`'s `record_with_pr`, `pr ready`, `review request`, and `devrun up`'s `write_pin`. Without it, a background run writing `events` while an agent's first `devrun up` writes the baseline pin erases one or the other.

An event is claimed by adding it to `events` through `update`, before any network write. A claim that finds the event already present stops there. A failed write, or a failed spawn, does not remove the claim: a tracker that is down costs one missed transition, never a retry on every session, and a person reruns `issue event` to recover. `issue end` removes the record, and the claims with it.

- `setup`: `issue setup` writes its record with `setup` already in `events` when that event is configured, so the claim costs no second write.
- `pr_open`: the claim is added in the same `update` that records the PR.
- `start`: the hook claims it itself, so of several sessions starting together in one checkout, exactly one spawns a run.

`start` fires only when all of these hold:

- `origin` is `setup`. A record with no `origin` does not qualify: `devrun up` synthesizes one for a hand-made worktree, named after its branch, and a branch like `ENG-123` parses as a tracker id. Worktrees set up before `origin` existed therefore never fire `start`.
- `issue` parses as a tracker id through `worktree::IssueId::tracker`, which rejects `UNKNOWN` and an empty id. This excludes `issue setup --slug` worktrees.

A reviewer's `pr checkout` worktree therefore never fires `start`, so a reviewer's first session cannot pull an issue back from review.

### The SessionStart hook

Today every hook verb but `pre-tool-use` only records, with one global config read. With `start` in play, the SessionStart verb also:

1. resolves the payload's checkout from `record::payload_cwd`;
2. reads the record, a local file;
3. walks the project's config layers, as `devkit brief` already does at the same event;
4. claims `start` through `record::update` when the conditions above hold;
5. spawns `std::env::current_exe() issue event start --dir <checkout root>` through `devkit_common::sys::spawn_background`, as `todo/sync.rs` does, only when its own claim added the marker.

It does no network IO, writes no stdout, never exits 2, and never changes a verdict. The same verb runs from the Claude Code, Codex and Cursor manifests. A failed background run is written to devkit's log.

### Failure

- **CLI:** a status name the tracker lacks, a missing project, a field that is not single-select, or a token without access is an error that names the config key. `issue setup` and `issue pr create` print it as a warning and succeed.
- **Token scope:** GitHub reports a token without Projects access as a GraphQL error of type `INSUFFICIENT_SCOPES`. `Api::graphql` surfaces only the first error's message today, so the writer reads the error's type and reports "token lacks the `project` scope". The remedy depends on `Api::token_source`: `gh auth refresh -s project` for a token from `gh`, and reissuing `GH_TOKEN` or `GITHUB_TOKEN` with `project` for one from the environment.

### `devkit doctor`

When any event is configured, an `issue events` row lists each event's transition and checks it against the tracker. On GitHub it runs the read query and confirms that the project exists, that `status_field` is a single-select field holding every `to` and `from` name, and that the token has Projects access (no `INSUFFICIENT_SCOPES`). A GitHub Enterprise Server without Projects v2 fails schema validation on `projectV2`, and the row reports it. With no writer, the row says which key is missing.

### Documentation

`plugin/skills/using-devkit/references/issues.md` gains a section on status events: when each event fires and in which worktrees, firing once per worktree, rerunning with `issue event`, and the token scope. It points at `devkit schema` for the keys rather than restating them.

## Testing

- **GitHub writer:** query and mutation builders and parsers over recorded fixtures, as `tracker/github.rs` tests its reads. Cases: the item found; no item, so it is added first; the option named `to` resolved case-insensitively; an unknown name listing the options; a field that is not single-select; `INSUFFICIENT_SCOPES` reported with the remedy for each token source. The network wrapper is not unit-tested, like the rest of the tracker.
- **Linear writer:** builders and parsers over recorded fixtures. Cases: the state set by name within the team; an unknown name listing the team's states; an id `parse_id` rejects.
- **Transition rule:** a `from` match, `*`, the empty string for no status, case-insensitive names, and no write when the issue is already at `to`.
- **Record:** `update` serializes concurrent updaters, so a claim and a baseline pin written at once both survive.
- **Hook:**
  - The first SessionStart in a qualifying worktree with `start` configured claims the event and spawns one run.
  - A second session spawns nothing, and two sessions starting together spawn one run.
  - Nothing spawns for a `checkout` worktree, a worktree whose issue is not a tracker id, a record with no `origin` (a legacy one, or one `devrun up` synthesized), a tracker of kind `none`, or no `start` configured.
  - The hook's output is unchanged when the run fails.
- **CLI:** `issue setup` and `issue pr create` fire their events, including when `pr create` reuses an existing PR, and stay successful when the write fails. `issue status` is unchanged.
- **Config:** an unknown event name fails to parse; `project` takes `N` and `"owner/N"`; the schema is regenerated.
- **Doctor:** the row on a configured project, the scope check, and a missing writer.
