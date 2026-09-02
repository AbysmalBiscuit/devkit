# The `issue pr` command group

## Goal

Give the PR lifecycle its own command group, and make draft state real.

Today `issue review request` pushes the branch, creates or reuses the PR,
verifies it carries this worktree's commits, writes the record, adds GitHub
reviewers, and Slacks them. Six jobs behind a name that describes one. Its
`--no-notify` flag exists to switch off the verb in the command name, which is
what a command doing too much looks like from the flag side.

Separately, devkit cannot tell a draft PR from a ready one anywhere except
`issue prs`. A draft reads as `OPEN` through the triage table, through the
verdict, and through what MCP serializes to agents.

The two problems share a fix. PR creation moves under `issue pr`, draft becomes
the default state a PR is created in, and `issue review request` is left with
one job: tell humans the PR is ready, which includes making that true.

## Scope

**In:** the `issue pr` group (`status`, `create`, `ready`, `checkout`);
`is_draft` carried through `PrBrief`, `PrStatus`, the triage render, and the
reviewer queue; `defaults.require_pr_reviewer` moved to the ready transition and
re-pointed at GitHub reviewers; `review request` requiring an existing PR and
flipping draft to ready; hidden top-level aliases for `info` and `checkout-pr`;
the `defaults.pr_create_state` key; the docs, completions, and JSON schema that
follow.

### Non-goals

- **`issue pr merge`.** The group is the obvious home for it and the CLI has
  never merged anything. Merging has its own gates (checks, approvals, merge
  method) and belongs in its own change.
- **Renaming `issue prs`.** It collides with bare `issue pr` by one letter, but
  both are read-only, both print immediately, and a wrong guess is obvious on
  sight. Revisit if it bites.
- **Tracker mutation.** `issue end` gates on a Completed issue and nothing in
  devkit can complete one. Unchanged here.
- **Verification in `review finish`.** It still checks nothing about whether a
  review was submitted.
- **MCP mutations.** `issue` actions on MCP stay read-only; they gain draft
  visibility, not commands.
- **Enforcing `issue setup` before `issue pr create`.** A record-less worktree
  keeps working, as it does today.

## Background

`review request` (`src/bin/devkit/issue/review/request.rs`) branches on
`action_for` (`review/mod.rs:41`), which maps a PR state to Create, AddReviewer,
or Stop. Both acting arms run `assert_belongs` (`review/finish.rs:154`), the gate
that refuses to touch a PR whose head oid differs from local HEAD. The push at
`request.rs:265` is what keeps that gate satisfied in practice: by the time the
fetch happens, the remote already has this worktree's commits.

`defaults.require_pr_reviewer` (`crates/devkit-config/src/lib.rs:321`) is checked
only on the Create arm (`request.rs:384`) and reads `targets.is_empty()` over
Slack recipients (`review/mod.rs:62`), while `reviewer_logins` drops any target
with no `github` handle (`request.rs:60`). So `--to '#eng'` satisfies the gate
with zero reviewers on the PR, and the AddReviewer arm never checks it at all.

Draft state exists in exactly one place. `crates/devkit-issue/src/prs.rs:205`
deserializes `isDraft`, and `mine_action` returns `"draft"` for the author's own
row (`prs.rs:489`). Nothing else fetches it: `PrBrief`
(`crates/devkit-common/src/github.rs:548`) has `state: String` with
`MERGED | OPEN | CLOSED` and no draft field, and no query that builds one selects
it. `parse_brief` (`github.rs:626`) ignores the REST `draft` key it already
receives.

`issue info` and `issue checkout-pr` predate any PR namespace. `info` renders one
worktree's row from the same table `status` renders for all of them
(`triage.rs`), defaulting to the current worktree, and its `--json` emits
`IssueWorktree` through a hand-written `Serialize`
(`crates/devkit-issue/src/status.rs:78`).

## Design

### The surface

| Command | Does |
|---|---|
| `issue pr` | `issue pr status` for the current worktree |
| `issue pr status [selector] [--json] [--cache-only]` | today's `issue info`, unchanged behavior |
| `issue pr create [--draft\|--ready] [--to] [--base] [--pr-title] [--pr-body] [--no-push] [--pr]` | push, create or reuse, gate, write the record |
| `issue pr ready [--no-push] [--pr]` | push, gate, mark ready for review |
| `issue pr checkout <target> [path] [--setup] [--apps]` | today's `issue checkout-pr` |
| `issue review request …` | resolve the PR, flip draft to ready, add reviewers, Slack |
| `issue review finish …` | unchanged |

`status` earns its word rather than being a bare positional on `pr`: clap
resolves subcommands before positionals, so `issue pr <selector>` would make a
branch or worktree named `create` unreachable. With `status` holding the
selector, `Pr { #[command(subcommand)] cmd: Option<PrCmd> }` mirrors the
`IssueCli.cmd: Option<Cmd>` the top level already uses (`mod.rs:57`), with `None`
meaning status.

`issue info` and `issue checkout-pr` remain as hidden top-level variants
dispatching to the same functions, so muscle memory and existing scripts keep
working. Hidden means absent from `--help` and from the docs, present in the
parser.

### Draft in the PR model

`PrBrief` gains `is_draft: bool`. Four queries select it and three parsers read
it:

| Site | Add |
|---|---|
| `github.rs:712` `prs_by_number_query` | `isDraft` |
| `github.rs:824` `head_query` | `isDraft` |
| `github.rs:835` `parse_pr_node` | read it |
| `github.rs:626` `parse_brief` (REST) | read the `draft` key already in the payload |
| `crates/devkit-issue/src/status.rs:318` `heads_query` | `isDraft` |
| `review/request.rs:34` `PrFlat` and `:122` `existing_pr` | `isDraft` in the `--json` list and the struct |

`PrFlat` is the `gh pr list` fallback, taken whenever `github::token()` is
`None`. Defaulting it to `false` would make every draft read as ready on exactly
the path that has no token.

A missing `isDraft` parses as `false` rather than failing, matching how
`parse_pr_node` already treats `headRefOid`. Strict parsing would mean rewriting
every JSON fixture in `github.rs` and `status.rs` to buy nothing.

`PrStatus::Unique` gains `is_draft: bool` alongside `number`, `state`, `url`.
The compiler finds the sites that construct it, chiefly `pr_status_of`
(`crates/devkit-issue/src/status.rs:204`, the single place a `HeadLookup`
becomes a `PrStatus`) and `apply_cached_pr` (`info.rs:313`).

It does not find the sites that read it. `pr_label` (`triage.rs:8`),
`status.rs:37,44,53,589`, and `info.rs:194` all match `Unique { .. }` with a rest
pattern and keep compiling while silently ignoring the new field. Those are found
by grep, not by rustc, and `pr_label` is the one the render depends on.

`state_label()` keeps returning `OPEN` for a draft. Its doc comment records that
it is the value the serialized `pr_state` field carries for consumers written
against it, and quietly changing that string is how an MCP consumer's state
machine breaks. Draft reaches consumers through the tagged `pr` field, which
serializes the new bool for free.

Rendering does change. `pr_label` (`triage.rs:5`) renders `DRAFT #123` where the
PR is a draft.

`info_cache::CachedPr` gains the field so `--cache-only` reports draft state
without a network call. It is `#[serde(default)]`: `info_cache::read`
(`info_cache.rs:22`) returns `None` on a parse failure, so a required field would
give every existing `pr.json` one silent cache miss.

`pr_cell` (`triage.rs:50`) dims a draft rather than painting it yellow, since a
draft is not waiting on anyone. It checks the flag before its match on
`state_label()`, which would otherwise take the `OPEN` arm.

`reviewer_state` (`prs.rs:557`) returns `draft` instead of `REVIEW NEEDED` when
the PR is a draft. Today a pending review request wins unconditionally, so a
draft that carries a reviewer sits in that reviewer's queue as work to do. The
legend at `src/bin/devkit/issue/prs.rs:182` already calls draft passive; this
makes the reviewer's section agree with the author's.

### `issue pr create`

Push (unless `--no-push`), resolve the PR the way `review request` does today
(explicit `--pr`, then the record, then branch discovery), then:

- **No PR:** create it. Draft unless `--ready`. `verify_created` afterwards.
- **PR exists and is OPEN:** reuse it. Run `assert_belongs` before writing
  anything. Report the existing PR and its draft state.
- **MERGED or CLOSED:** stop, as `action_for` already does.

The reuse arm never changes draft state. `create` means "make sure a PR exists",
and a flag that also flips an existing PR would be doing two unrelated jobs
depending on whether one happened to be there already. A state flag that
contradicts the PR found is reported rather than ignored:

```
PR #123 already exists and is ready for review.
--draft was ignored. To move it back: gh pr ready --undo
```

The reuse arm runs `assert_belongs` because it writes the record, and the record
is what makes a PR authoritative downstream. `finish.rs:356` has a test for the
failure this prevents: a mistyped `--pr` names a real PR, the record makes it
authoritative, and that PR's merge lets `issue end` run `git branch -D` on a
worktree whose work never landed.

`--draft` and `--ready` are a mutually exclusive clap group. Neither given, the
state comes from config.

`--to` sets GitHub reviewers and sends no Slack, on both arms: `gh pr create
--reviewer` when creating, `gh pr edit --add-reviewer` (`request.rs:364`) when
reusing. It is allowed alongside `--draft`: once `reviewer_state` reads the flag,
a draft with a reviewer shows as passive in that reviewer's queue, and "start
this and put it on someone's radar" is a thing people want.

`action_for` keeps its single mapping, `None => Create`. `pr create` acts on it;
`review request` bails on it. Two mappers for one PR-state question would be two
places to keep in sync.

### `issue pr ready`

Pushes (unless `--no-push`), fetches the PR, runs `assert_belongs`, then marks it
ready. Idempotent: already-ready is reported and exits zero.

The push is not optional-by-default. Without it, `pr create --draft`, two more
commits, `pr ready` fails `assert_belongs` with "PR #N is at X but this worktree
is at Y", an error whose stated diagnosis is wrong. The PR does carry this work;
the remote just hasn't heard about it. Without the gate instead, `ready` marks
whatever same-named PR resolves, which is the fork case the reuse arm closes.

The mark-ready call is `gh pr ready <n>` through `cmd::gh_capture`, which appends
`--repo` so an ambient `GH_REPO` cannot redirect it. `github.rs` has no GraphQL
mutation helper and the token-less path needs `gh` anyway.

Like `pr create`, it writes the resolved locator into `.devkit/issue.toml`. A
command that fetched the PR and gated on it has the binding in hand; leaving the
record empty here and healing it in the next command would be arbitrary.

There is no devkit command for the reverse. See the rejected alternatives.

### `issue review request`

Loses `--base`, `--pr-title`, `--pr-body`, and the ability to create. Keeps
`--to`, `--pr`, `--arg`, `--no-push`, `--no-notify`, and the body.

With no PR for the branch it fails, naming `issue pr create`. With a PR it
pushes, gates, heals the record if the PR was found by branch discovery, flips
draft to ready, adds `--to` reviewers, and Slacks.

The record heal stays here. A `setup` worktree is created with `pr: None`
(`setup.rs:534`), and branch discovery inside `review request` is what binds it
today. Dropping that would leave the record empty forever and push `status` and
`end` onto branch matching, the path `status.rs:489` exists to avoid.

`--no-notify` does not flip the draft. It means "update GitHub, tell nobody", and
promoting a PR to ready is telling everybody.

The Slack `{{ pr_title }}` now comes from the PR rather than the locally rendered
`pr_title` template, so a re-request stops depending on a template for a PR it is
not creating.

The title comes from a `github::pr_full` call, the one `review finish` already
makes (`finish.rs:110`). `PrBrief` has no title field (`github.rs:548`) and
adding one would mean touching all six construction sites for a field one caller
wants. One extra round trip on a command that already makes several is the
cheaper trade.

### Where `require_pr_reviewer` lives

It moves to the ready transition, and it counts human GitHub reviewers instead of
Slack targets.

The gate fires on a run that *makes* a PR ready, not on every run that touches a
ready one. Three paths reach it: `pr create --ready`, `pr ready`, and `review
request` flipping a draft. A run that transitions nothing is not refused for a
violation it did not create. `pr create --draft` with no reviewer is no longer a
violation, because it isn't one.

The invariant still holds under `--no-notify`, which never flips a draft: the
only ways to reach ready are the three gated paths.

A human reviewer means a login that is either currently requested or has already
submitted a review, unioned with whatever `--to` adds in this run. Requested
alone is not enough: GitHub drops a login from `reviewRequests` the moment they
review (`crates/devkit-issue/src/prs.rs:485`), so a draft that collected an early
approval would show zero reviewers and be refused. `requested_reviewers`
(`github.rs:881`) returns raw logins including bots, so the gate helper applies
`is_human_login` itself rather than relying on the fetcher.

One behavior changes for a project with the key set: `--to '#eng'` no longer
satisfies it, since a channel is not a reviewer. That is the gate doing what its
name says.

### Which commands push and gate

| Command | Pushes | `assert_belongs` |
|---|---|---|
| `pr create` (create arm) | yes | `verify_created` after |
| `pr create` (reuse arm) | yes | yes |
| `pr ready` | yes | yes |
| `pr checkout` | no | unchanged |
| `review request` | yes | yes |
| `review finish` | no | no, by design |

## Config

One new key on `[defaults]`:

```toml
[defaults]
pr_create_state = "draft"   # or "ready"
```

Default `draft`. A two-value enum rather than a `pr_draft` bool, because
`pr_draft = false` is a double negative for what is really "open it ready".
Matched exhaustively, with `Deserialize`, `Serialize`, `JsonSchema`, and
`Display`. No `ValueEnum`: `devkit-config` has no clap dependency, and the CLI
surface is `--draft` / `--ready` rather than a value.

`require_pr_reviewer` keeps its name and gains a new doc comment describing the
transition it now guards. Config doc comments are copied verbatim into
`schema/devkit-config.json`, so this one and `after_worktree_create`'s reference
to `issue checkout-pr` (`crates/devkit-config/src/lib.rs:163`) both reach the
committed schema, which is regenerated with `DEVKIT_UPDATE_SCHEMA=1 cargo test`.

## Documentation

`docs/commands.md` gains an `issue pr` section and loses the create half of
`review request`. `docs/configuration.md` documents `pr_create_state` and the
moved gate. `skills/using-devkit/references/issues.md` teaches the new flow, and
its `issue info --json` recipe becomes `issue pr status --json`.

`docs/agents.md:23` lists the CLI-only `issue` verbs; `issue pr` joins `review`
and `end` there, and the `reviews` rows gain a `draft` action value MCP consumers
have not seen before.

Prose across the repo referring to `checkout-pr` by name is updated. The config
keys `templates.checkout_worktree_dir` and `checkout_worktree_dir_max` keep their
names: renaming them would break every existing config to fix a cosmetic
mismatch, and config keys are not command names.

## Testing

- `is_draft` survives each transport: the GraphQL node parse, the REST parse, the
  `heads_query` split, and the `gh pr list` fallback.
- A payload with no `isDraft` parses as not-draft rather than failing.
- `pr_label` renders `DRAFT #n`, and `state_label()` still returns `OPEN` for the
  same row.
- `reviewer_state` returns `draft` for a draft carrying a review request.
- The reviewer gate: refused with no human reviewer on each of the three ready
  paths; satisfied by an existing requested reviewer with no `--to`; satisfied by
  a reviewer who already submitted and is no longer requested; not satisfied by a
  `#channel`; not fired by a run that transitions nothing.
- `pr ready` on an already-ready PR exits zero and mutates nothing.
- `pr create --draft` against an existing ready PR leaves it ready and says the
  flag was ignored.
- `review request` with no PR fails naming `issue pr create`.
- `review request` heals a record whose `pr` is `None`.
- The hidden aliases dispatch: `issue info` reaches `pr status`, `issue
  checkout-pr` reaches `pr checkout`.
- `tests/shim_dispatch.rs:228` asserts `issue --help` lists `checkout-pr`. Hiding
  the variant removes it from help, so the assertion moves to `pr`.

## Commit sequence

Ordered so no intermediate commit leaves the tree broken. Only the last is
breaking.

1. `feat(github): carry is_draft on PrBrief` — the queries, both parsers, `PrFlat`, lenient parse, fixtures.
2. `feat(issue): tag PrStatus::Unique with is_draft` — `pr_status_of`, `apply_cached_pr`, `CachedPr`; MCP serializes it for free.
3. `feat(issue): render drafts in triage and the reviewer queue` — `pr_label`, `pr_cell`, `reviewer_state`.
4. `feat(config): add defaults.pr_create_state` — the enum, schema regen, `configuration.md`.
5. `feat(issue): add the pr command group` — `pr status` and `pr checkout` as pure moves, hidden aliases, the shim test, prose.
6. `feat(issue): add pr create` — extract the create arm out of `request.rs` into a shared module that `review request` still calls.
7. `feat(issue): add pr ready` — the mark-ready call, idempotence, the reviewer-gate helper.
8. `feat(issue)!: require an existing PR in review request` — drop the creation flags, move the gate, the draft flip, the title source, the `commands.md` rewrite.

## Rejected alternatives

**Keep `issue info` where it is.** Argued for on the grounds that it reports
tracker data as well as PR data, so filing it under `pr` misnames it. Rejected:
`info` is a name that describes nothing, it predates the namespace, and the
columns you run it for are the PR and the verdict. The hidden alias covers
anything already written against it.

**An `issue pr draft` demote command.** Rejected. `gh pr ready --undo` already
does it, and a devkit wrapper would add only PR resolution from the record.
Every reason `pr ready` exists as a command is absent going the other way: no
push is needed, no reviewer gate applies, and only one caller would ever want
it. `pr create --draft` does not cover the case either, and deliberately so.

**`issue pr open` for the ready transition.** Rejected. "I opened a PR" means
"created" to every developer alive, `pr create` would sit beside it in `--help`,
and the wrong guess is silent. GitHub's own verb is `ready`.

**`--draft` conflicts with `--to`.** Proposed to stop a draft appearing as
`REVIEW NEEDED` in a reviewer's queue. Rejected in favor of fixing
`reviewer_state`, which is needed anyway for drafts created outside devkit, and
which leaves the useful combination available.

**`require_pr_reviewer` degraded to a warning.** Rejected. A project that set it
believes PRs cannot go out unreviewed; a warning quietly makes that false, which
is the same fail-open-while-believed-protected failure the `deny_unknown_fields`
convention exists to prevent.

**Folding `issue prs` under `issue status`.** Rejected. `status` reads worktrees
on this machine; `prs` reads GitHub and includes PRs with no worktree here plus
PRs awaiting your review. And `status` takes positional ids, so `prs` would
become a selector you can never use.

## Open questions

1. Does anything outside this repo read `issue info --json`? If so the alias is
   permanent rather than transitional.
