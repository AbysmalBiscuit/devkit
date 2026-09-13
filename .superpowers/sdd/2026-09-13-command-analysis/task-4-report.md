# Task 4 report: wrappers, runners and program options

## Result

Implemented Task 4 only in `crates/devkit-command/src/normalize.rs`.

The normalizer now removes and records process wrappers and runner prefixes,
tracks wrapper-driven directory changes, unwraps both supported Doppler forms,
models xargs input as unknown values, analyzes `find -exec` children before the
outer `find` invocation, and removes Git global options from `semantic_args`
while preserving the original argument vector. `doppler_flags` extracts config
and project values for downstream guards. Existing `basename`, `CwdChange`,
`Unwrapped`, and `ProgramOptions` interfaces remain intact.

## TDD evidence

The five normalization tests were added before the implementation:

- process wrappers and wrapper records
- runner prefixes and runner cwd changes
- Doppler `--` and `--command=` forms
- Git global options and semantic arguments
- xargs unknown arguments and `find -exec` ordering

The first focused test command was rejected by the repository write/test hook,
which requires `devrun task test`. The first task-wrapper run then failed before
compilation because the configured zccache daemon was unavailable. Therefore the
expected stub RED assertion was not observable through the enforced runner. The
tests were still written before production implementation, and the final
focused run passed all five tests.

## Verification

- `devrun task fmt`: passed.
- Focused `devkit-command` normalization tests: 5 passed.
- `devrun task lint --env RUSTC_WRAPPER= --env RUSTC_WORKSPACE_WRAPPER=`: passed.
- `devrun task test-doc --env RUSTC_WRAPPER= --env RUSTC_WORKSPACE_WRAPPER=`: passed.
- Required workspace nextest task, with compiler wrappers disabled: 1862 passed, 34 failed due sandbox restrictions unrelated to Task 4. Failures were Unix socket bind, TCP bind, filesystem permission, and cache-environment failures. All `devkit-command` normalization tests passed in that run.
- `git diff --check`: passed.

## Files

- `crates/devkit-command/src/normalize.rs`
- `.superpowers/sdd/2026-09-13-command-analysis/task-4-report.md`

## Commit

Commit `185389df73b7963ef1a28bd241251fe5f5d23748` with the required subject: `feat(command): unwrap wrappers, runners and git options`.

## Concerns

The plan's Git test expects `inv.args.len() == 7`, but the parsed command has
eight arguments after `git`: `-C`, its value, `-c`, its value, `--no-pager`, and
the three semantic arguments. The implementation preserves that full vector,
so the test assertion uses 8 while retaining the planned semantic assertion.

The workspace gate cannot be fully green in this sandbox because tests requiring
Unix sockets, network binds, or unrestricted filesystem operations are denied.

## Fix round 1

The reviewed implementation head was verified as `185389df73b7963ef1a28bd241251fe5f5d23748` before the evidence run. A standalone temporary clone was created at that head with an isolated Cargo target directory and a temporary focused devkit task; the shared worktree and Git stash were not used.

RED was observed through the approved `devrun` wrapper using `devrun task -C /tmp/devkit-task4-red.ot5B06/checkout --config /tmp/devkit-task4-red.ot5B06/focused-devkit.toml test --env RUSTC_WRAPPER= --env RUSTC_WORKSPACE_WRAPPER=` after replacing only the temporary clone's normalizer with the stub and supplying a temporary `doppler_flags` signature. The result was `0 passed, 5 failed, 30 skipped`; failures were the expected missing wrapper, runner, Doppler, xargs/find, and Git normalization behaviors.

GREEN was then observed through the same approved task after restoring `normalize.rs` from `185389df73b7963ef1a28bd241251fe5f5d23748`: `5 passed, 30 skipped`.

The implementation checkout remained at the reviewed head throughout. The required follow-up verification was run there with the project compiler-wrapper overrides: `devrun task fmt` passed; `devrun task lint --env RUSTC_WRAPPER= --env RUSTC_WORKSPACE_WRAPPER=` passed; `devrun task test-doc --env RUSTC_WRAPPER= --env RUSTC_WORKSPACE_WRAPPER=` passed; and `devrun task test --env RUSTC_WRAPPER= --env RUSTC_WORKSPACE_WRAPPER=` produced the same sandbox-only failures while all Task 4 normalization tests passed. No production behavior or design was changed in this fix round.

The artifact correction and RED/GREEN evidence are committed separately as `fix(command): verify normalization red path`.
