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

Commit: cb38acf (amended after this report update).
