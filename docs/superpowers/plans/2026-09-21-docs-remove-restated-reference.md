# Remove the reference prose that restates generated output: plan

Issue: #59. Spec: the issue body, plus two rulings made with the maintainer before this plan:

- Per-key config behavior moves into the Rust doc comments on the config types, so `devkit schema` and editor hover carry it. Command behavior and cross-cutting gotchas move into `skills/using-devkit/references/` (docm into `skills/docs/`). `docs/configuration.md` shrinks to location, layering, secrets, editor setup, environment variables and the whole-config example. `docs/commands.md` is deleted; `README.md` keeps a short subcommand index.
- The issue's premise that most of the prose is already generated is only partly true: `-h` carries flags only, and most per-key behavior is absent from the schema. So most lines relocate rather than disappear.

## Global constraints

- No fact exists in only one place before the change and nowhere after it. Every cut line is either already in a doc comment, `-h`, the skill, or `docs/agents.md`, or is moved there in the same task.
- Doc comments are the schema's descriptions. Keep them plain prose; a field's comment says what the key does and what bites, not how the code is structured.
- `schema/devkit-config.json` is regenerated with `DEVKIT_UPDATE_SCHEMA=1 cargo test --test config_schema` after any doc-comment edit.
- Plans and specs under `docs/superpowers/` are snapshots; their links to the removed docs stay as they are.
- Living docs carry no counts.

## Task 1: Config reference into doc comments

Files: `crates/devkit-config/src/lib.rs`, `crates/devkit-config/src/harness.rs`, `crates/devkit-docs/src/manifest.rs`, `schema/devkit-config.json`.

For every section of `docs/configuration.md` under `## Sections`, move the per-key and per-table prose that the type's doc comments lack onto the type or field it describes: `[defaults]` (including path-value resolution), baselines (onto `baseline_dir`), `[apps.<name>]` (launch templating, prd guard, memory cap advice), `[tasks]` (args, `required_args`, `require_live`, sequences, `split`), `[daemon]` (each key's env override and the cgroup behavior), `[parallelism]`, `[docs]`, `[harness]` (opt-in precedence, what enforcement and the guard gate, command rules, `app_match`, fail-open/closed), `[harness.log]` (global-only keys, clamping, redaction, retention), `[brief]`, `[mcp]`, `[tracker]`, `[github]`, `[linear]`, `[hooks]`, `[preserve]`, `[rules]`, `[[context.files]]`, `[people]`, `[templates]`.

Steps:

1. Edit the doc comments.
2. Run `DEVKIT_UPDATE_SCHEMA=1 cargo test --test config_schema`. Expected: pass, schema rewritten.
3. Run `cargo test -p devkit-config -p devkit-docs --doc`. Expected: pass (doc comments still parse as doctests).
4. Commit `docs(config): carry the key reference in the doc comments`.

Test command: `cargo nextest run --test config_schema -p devkit-config -p devkit-docs`.

## Task 2: Link check, then slim `docs/configuration.md`

Files: `tests/docs_links.rs` (new), `docs/configuration.md`, `skills/using-devkit/references/config.md`.

1. Write `tests/docs_links.rs`: every relative markdown link in `README.md`, `AGENTS.md`, `docs/*.md` (not `docs/superpowers/`) and `skills/**/*.md` names a file that exists, and a `#fragment` into a markdown file names a heading there (GitHub slug rules). Run it. Expected: pass on the current tree (the guard is for the cuts that follow).
2. Cut `docs/configuration.md` to: intro pointing at `devkit schema`, `devkit schema init` and hover; Location; Layering; Secrets; Editor support; Example; Environment; TLS trust. Cross-cutting gotchas the doc comments cannot hold go into `references/config.md`.
3. Run the link test. Expected: FAIL on the anchors the cut removed (`configuration.md#harness`, `#harnesslog`, `#mcp`, `#tasks`, `#templates`, `#brief`, the split section). Retarget them.
4. Run `cargo nextest run --test docs_examples --test docs_links`. Expected: pass.
5. Commit `docs: cut the per-key reference from configuration.md`.

## Task 3: Relocate `docs/commands.md`, then delete it

Files: `docs/commands.md` (deleted), `skills/using-devkit/SKILL.md`, `skills/using-devkit/references/{issues,servers,diagnostics,locks,config}.md`, `skills/docs/references/docm.md` (new), `skills/docs/SKILL.md`, `docs/agents.md`, `README.md`.

Each behavior paragraph goes where an agent loads it:

- help views, `--timing`: `SKILL.md` global-flags paragraph and `README.md`.
- `devrun` status and strays, `baseline list`/`prune`, down scope: `servers.md`.
- `issue setup` slug, short_slug, upstream and hooks; `pr create` reuse and head check; `pr ready`; `pr checkout` naming; `end` ordering and baseline release; `sync-includes` symlinks and output grouping; `prs` paging; `dashboard` cache; `review request` ordering; live rendering: `issues.md`.
- `devkit auth`, `config`, `doctor`, `brief`: `diagnostics.md`.
- `devrules`: `config.md`.
- `hook`, `hook-log`, `devkit-mcp` gates: `docs/agents.md`.
- `docm` resolution, references, sizes, reserved names, 0.12 migration: `skills/docs/references/docm.md`, linked from the docs skill.
- `README.md` gains a one-line-per-binary index replacing the `docs/commands.md` link.

Run the link test. Expected: FAIL naming every link to `commands.md` that remains; retarget them. Expected then: pass. Commit `docs: move the command reference into the skills`.

## Task 4: Retarget the remaining pointers

Files: `AGENTS.md`, `.gitignore`, `src/bin/devkit/schema.rs`, `src/bin/devkit/main.rs`, `crates/devkit-config/src/lib.rs`, `crates/devkit-config/src/harness.rs`, `skills/using-devkit/references/{diagnostics,issues}.md`, `tests/docs_examples.rs`.

Every prose or comment pointer at `docs/commands.md`, or at a `docs/configuration.md` section that no longer exists, names the new home. AGENTS.md's "User-facing reference" line names `devkit schema`, `-h` and the skill, and gains a Conventions rule saying where each kind of documentation lives, so the restated copy does not grow back. Regenerate the schema. `rg -n 'commands\.md' -g '!docs/superpowers/**' -g '!CHANGELOG.md'` returns nothing.

Commit `docs: point at the generated reference`.

## Task 5: Full gate

`cargo nextest run --workspace --no-fail-fast`, `cargo test --workspace --doc`, `cargo clippy --workspace --all-targets -- -D warnings`, `devrun task fmt-check`. All pass.

## Review focus

- A fact that was in `docs/configuration.md` or `docs/commands.md` and is now nowhere.
- A doc comment that became a schema description too long or too internal to read as hover text.
- A skill reference that now contradicts `-h` or the schema.
