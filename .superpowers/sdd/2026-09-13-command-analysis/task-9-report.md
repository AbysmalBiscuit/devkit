# Task 9 report

Implemented the JavaScript and TypeScript adapter in `crates/devkit-command/src/js.rs`.

## Probes

- Node: `node -e 'console.log(JSON.stringify(process.argv))' alpha beta` returned `["/usr/bin/node","alpha","beta"]`.
- Bun: `bun -e 'console.log(JSON.stringify(process.argv))' alpha beta` returned `["/home/lev/.bun/bin/bun","alpha","beta"]`.
- The compiled grammar probe confirmed `variable_declarator.name/value`, `member_expression.object/property`, `call_expression.function/arguments`, `subscript_expression.object/index`, and TypeScript `as_expression`.

## RED/GREEN

- Added all nine outer `testutil::bash` integration tests first. The first sanctioned run reported seven intended Task 9 failures; two no-effect tests passed against the stub because it emitted no effects.
- GREEN run: all Task 9 tests passed. The template fixture uses a shell-safe single-quoted Node source so Bash passes JavaScript template backticks through unchanged.

## Commands and verification

- Direct `cargo nextest run -p devkit-command js` was denied by the command hook; used `devrun -C /home/lev/Git/lev/devkit_worktrees/feat-locks-enforce-locks-for-shell-writes task --env RUSTC_WRAPPER= test`.
- `devrun ... task --env RUSTC_WRAPPER= fmt`: passed.
- `devrun ... task --env RUSTC_WRAPPER= lint`: passed.
- `devrun ... task --env RUSTC_WRAPPER= test-doc`: passed.
- Final workspace nextest: 1936 passed, 34 failed; all Task 9 tests passed. Failures are sandbox restrictions on Unix socket binding, port probes, and state-store writes.

## Sandbox failures

- `lockm acquire ... --as 01a097ae-75d9-79a0-a2db-a9bde7231f68` failed with `Read-only file system (os error 30)`.
- The initial `devrun task test` could not start `zccache`; clearing `RUSTC_WRAPPER` through the sanctioned task override allowed compilation.

Commit: 99f0f23.

## Review fix report

The review regressions were added through the real outer `testutil::bash` form before implementation:

- `reassigned_process_argv_does_not_use_outer_arguments`
- `reassigned_fs_member_is_not_a_known_write`
- `oversized_js_values_are_unresolved`
- `directly_called_local_function_is_analyzed`

RED evidence: `devrun -C /home/lev/Git/lev/devkit_worktrees/feat-locks-enforce-locks-for-shell-writes task --env RUSTC_WRAPPER= test` compiled 1974 tests; all four new `devkit-command` tests failed for the reported missing behaviors. The first value-limit fixture also exposed that an 8-byte outer limit rejects the whole embedded shell token before JS analysis, so it was tightened to 128 bytes and changed to construct an oversized path from short JS values. This preserves the outer integration while testing the JS value boundary.

GREEN evidence: the focused adapter binary ran `js::tests::` with 13 passed and 0 failed. The final workspace run completed 1940 passed and 34 failed; none were `devkit-command` tests.

Fixes: member assignments now invalidate qualified bindings; `process.argv` assignments and mutations no longer reuse outer argv; strings, templates, concatenations, path constructions, and argv-derived values enforce `Limit::ValueSize`; directly called local function declarations retain their body range and are analyzed lazily at the call site.

Final checks:

- `devrun ... task --env RUSTC_WRAPPER= fmt`: passed.
- `devrun ... task --env RUSTC_WRAPPER= lint`: passed.
- `devrun ... task --env RUSTC_WRAPPER= test-doc`: passed.
- `devrun ... task --env RUSTC_WRAPPER= test`: 1940 passed, 34 failed, 0 skipped.

Known sandbox failures remain exactly in socket binding, port liveness probes, registry/state-store writes, and related daemon/supervision tests, with `Operation not permitted` or `Read-only file system`. Direct cargo nextest remains blocked by the project command hook, so the devrun wrapper was used. No C dependency was added.

Fix commit: recorded in the final handoff.
