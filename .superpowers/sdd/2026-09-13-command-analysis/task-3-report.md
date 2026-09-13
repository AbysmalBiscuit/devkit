# Task 3 report: Bash adapter

## Result

Implemented Task 3 only from the command-analysis plan. The Bash adapter now walks Tree-sitter Bash syntax in execution order, records invocations and redirects, tracks literal bindings and directory changes, analyzes substitutions and heredocs, handles loops and functions, recovers per statement from parse errors, and exposes `simple_argv`.

Commit subject: `feat(command): analyze bash structure, bindings and redirects`

## Grammar-shape observations

The pinned `tree-sitter-bash` grammar produced these relevant shapes:

- `echo x > a.txt` is a `redirected_statement` with `body:` and `redirect:`; the redirect is a `file_redirect` with `destination:`.
- A heredoc is a `heredoc_redirect` with `heredoc_start`, `heredoc_body`, and `heredoc_end`.
- For `cat <<EOF | python3 -`, the pipeline is nested inside the `heredoc_redirect`, not beside it.
- Assignments use `variable_assignment` with `name:` and `value:`.
- Loops use `for_statement` with `variable:`, repeated `value:`, and `body:`; the body is a `do_group`.
- The grammar wraps `cd sub && echo ...` in a `list` used as the body of the surrounding `redirected_statement`. The walker analyzes that body before the outer redirect so the redirect uses the directory established by `cd`.

The shape test keeps the smallest fragments needed by the walker and remains in `bash.rs` as a regression guard.

## TDD evidence

1. Added and ran the grammar-shape test first. The printed S-expressions above were recorded before implementation and the shape assertions were narrowed to the actual grammar fields.
2. Added `lib.rs::testutil` and the Bash adapter tests before the walker.
3. The no-op RED run first failed to compile because `simple_argv` did not yet exist. After adding the required interface stub, the RED run compiled and failed the adapter tests with empty effects/invocations, as expected for the no-op walker.
4. Implemented the adapter and fixed three grammar-driven details without weakening target assertions: redirects around a `list` are evaluated after the list, brace expansion is unresolved at redirect destinations, and the malformed read-only fixture uses `fi` so the grammar confines the error to that statement.
5. The focused `devkit-command` test binary finished with 30 passed and 0 failed tests, including the shape guard and every Bash adapter test.

## Files

- `crates/devkit-command/src/bash.rs`: Bash walker, `simple_argv`, grammar shapes, and adapter tests.
- `crates/devkit-command/src/lib.rs`: test-only analysis helpers.
- `crates/devkit-command/src/catalog.rs`: required `is_cataloged` false stub.
- `crates/devkit-command/src/embed.rs`: required `is_interpreter` false stub.

## Verification

- `devrun task fmt`: passed.
- `devrun task lint`: passed with warnings denied.
- `devrun task test-doc`: passed for all workspace crates.
- `devrun task test`: the `devkit-command` tests passed. The workspace run completed 1,891 tests with 1,857 passed and 34 unrelated failures caused by the sandbox denying runtime sockets, port probes, daemon files, and other protected filesystem operations. No command-analysis test failed in that run.
- The repository write hook rejected direct cargo invocations and required the configured `devrun task` wrapper. The wrapper was used with `CARGO_BUILD_RUSTC_WRAPPER=` because the configured zccache daemon is unavailable in this sandbox.
- No Windows gate was run because this task adds no C dependency.

## Concerns

The full workspace gate cannot be green in this sandbox because the unrelated daemon, port, process, and filesystem tests need permissions unavailable here. The adapter itself is green, and clippy plus documentation tests pass.

## Round 1 fix report

### Findings addressed

The malformed read-only regression no longer uses `ls -la fi`, which the pinned grammar accepts as an ordinary command with an argument. The fixture is now `if (ls -la\n`. Its root S-expression is `(program (ERROR (command name: (command_name (word)) argument: (word))))`; the test asserts one localized `ERROR`, its source span, and the nested command shape before retaining the empty-uncertainties assertion. The actual error span includes the trailing newline.

The pinned grammar represents unquoted `{a,b}` as a `concatenation` containing three `word` nodes. `simple_argv` now resolves literal concatenations recursively, while an unescaped brace in a `word` makes the complete argv unknown. This preserves known literal concatenation such as `bun foo"bar"` and returns `None` for brace expansion and the other existing argv-changing syntax checks.

### RED/GREEN TDD evidence

The recovery and `simple_argv` regression tests were added before the fix. The first focused run exposed the actual concatenation shape: `simple_argv_reads_one_plain_command` failed with `left: None`, `right: Some(["bun", "foobar"])` for `bun foo"bar"`. The malformed fixture's recorded error span also established that the newline belongs to the localized `ERROR`. After adding grammar-aligned literal concatenation handling, the focused brace assertion failed with `left: Some(["bun", "{a,b}"])`, `right: None`, demonstrating the missing brace guard. Adding that guard made the covering Bash tests green.

Final focused command: `/home/lev/Git/lev/devkit_worktrees/feat-locks-enforce-locks-for-shell-writes/target/debug/deps/devkit_command-24db53e000e4d575 bash::tests --nocapture`. Result: `17 passed; 0 failed`.

### Verification

- `env CARGO_BUILD_RUSTC_WRAPPER= devrun task fmt`: passed.
- `env CARGO_BUILD_RUSTC_WRAPPER= devrun task lint`: passed with `-D warnings`.
- `env CARGO_BUILD_RUSTC_WRAPPER= devrun task test-doc`: all workspace documentation tests passed.
- `env CARGO_BUILD_RUSTC_WRAPPER= devrun task test`: `1891 tests run: 1857 passed, 34 failed, 0 skipped`; no `devkit-command` test failed. The 34 failures are the sandbox's protected socket, port-probe, daemon-file, process, and filesystem operations.
- `git diff --check`: passed.
- No Windows gate was run because this fix adds no C dependency.

### Files

- `crates/devkit-command/src/bash.rs`: localized malformed-statement regression and grammar-aligned `simple_argv` concatenation and brace handling.
- `.superpowers/sdd/2026-09-13-command-analysis/task-3-report.md`: this appended fix report.

### Commit

Commit subject: `fix(command): tighten bash adapter recovery`

Commit: `fix(command): tighten bash adapter recovery`, present at the final repository `HEAD`.

### Concerns

The workspace nextest gate remains non-green only because the sandbox denies unrelated runtime operations. The focused Bash tests, clippy, formatting, and documentation tests are green. No later task was implemented and nothing was pushed.
