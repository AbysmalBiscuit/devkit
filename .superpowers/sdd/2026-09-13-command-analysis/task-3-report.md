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
