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

First review fix commit: 558408d.

## Second review fix report

The second-round regressions were added through the real outer `testutil::bash`
form before the implementation changes:

- `oversized_process_cwd_is_unresolved` constructs a resolved `process.chdir` cwd beyond `Limit::ValueSize`, then checks that a relative write does not emit the oversized target.
- `local_function_calls_are_hoisted` calls a declared function before its declaration and expects its filesystem write to be analyzed.
- `called_function_expression_is_uncertain` invokes an arrow function whose body writes and expects unresolved uncertainty rather than a silently ignored call.

The existing strict runtime probes remain part of this task's evidence:

- `node -e 'console.log(JSON.stringify(process.argv))' alpha beta` returned `["/usr/bin/node","alpha","beta"]`.
- `bun -e 'console.log(JSON.stringify(process.argv))' alpha beta` returned `["/home/lev/.bun/bin/bun","alpha","beta"]`.
- The adapter uses the pinned Tree-sitter JavaScript shapes `function_declaration`, `arrow_function`, `function_expression`, and `call_expression`; the outer integration tests exercise those shapes through the analyzer rather than executing JavaScript.

RED/GREEN evidence was recorded one root cause at a time. The first run after
adding all three tests was:

```text
devrun -C /home/lev/Git/lev/devkit_worktrees/feat-locks-enforce-locks-for-shell-writes task --env RUSTC_WRAPPER= test
1977 tests run: 1940 passed, 37 failed, 0 skipped
FAIL devkit-command js::tests::called_function_expression_is_uncertain
FAIL devkit-command js::tests::local_function_calls_are_hoisted
FAIL devkit-command js::tests::oversized_process_cwd_is_unresolved
```

After the cwd boundary fix, the same command reported 1941 passed and 36 failed, with the cwd regression absent from the failure list. After the function-declaration prepass, it reported 1942 passed and 35 failed, with the hoisting regression absent. After function-expression uncertainty, it reported 1943 passed and 34 failed, with all three new adapter regressions absent. The focused compiled adapter command was:

```text
/home/lev/Git/lev/devkit_worktrees/feat-locks-enforce-locks-for-shell-writes/target/debug/deps/devkit_command-b94aadffacfba209 js::tests::oversized_process_cwd_is_unresolved js::tests::local_function_calls_are_hoisted js::tests::called_function_expression_is_uncertain --exact --nocapture
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 113 filtered out
```

The implementation checks resolved `process.chdir` paths and `process.cwd` values with the shared value budget, registers function declarations before executing each statement list, and reports an unresolved write for invoked function or arrow expressions that the current model cannot safely execute.

Exact final verification commands and results:

- `devrun -C /home/lev/Git/lev/devkit_worktrees/feat-locks-enforce-locks-for-shell-writes task --env RUSTC_WRAPPER= fmt`: passed.
- `devrun -C /home/lev/Git/lev/devkit_worktrees/feat-locks-enforce-locks-for-shell-writes task --env RUSTC_WRAPPER= test`: 1977 tests run, 1942 passed, 35 failed, 0 skipped. All three second-round adapter tests passed; the changed adapter introduced no failing test.
- `devrun -C /home/lev/Git/lev/devkit_worktrees/feat-locks-enforce-locks-for-shell-writes task --env RUSTC_WRAPPER= lint`: passed with `-D warnings`.
- `devrun -C /home/lev/Git/lev/devkit_worktrees/feat-locks-enforce-locks-for-shell-writes task --env RUSTC_WRAPPER= test-doc`: passed; every workspace doctest binary reported zero failures.
- Direct `RUSTC_WRAPPER= cargo nextest run -p devkit-command --lib` was blocked by the project hook, which required `devrun task test`.

The final nextest sandbox failures were these 35 tests:

- `devkit::shim_dispatch devrun_shim_still_refuses_reap_without_a_terminal`
- `devkit::down_ports down_ports_releases_listed_reservations`
- `devkit::lifecycle idle_exit_with_no_clients_or_children`
- `devkit::lifecycle ping_pong_handshake`
- `devkit::lifecycle second_instance_exits_immediately`
- `devkit::lock_daemon acquire_through_daemon_is_visible_to_check`
- `devkit::lock_daemon acquired_lock_persists_to_file_after_daemon_exits`
- `devkit::lock_daemon write_decide_and_release_prefix_through_daemon`
- `devkit::bin/devkit issue::dashboard::cache::tests::get_put_roundtrip_under_real_cache_dir`
- `devkit::parity alloc_through_daemon_writes_registry`
- `devkit::parity snapshot_roundtrips`
- `devkit-common supervise::tests::probe_port_true_when_listening_false_when_free`
- `devkit-common supervise::tests::spawn_and_ready_on_python_tcp`
- `devkit::supervision down_does_not_restart`
- `devkit::supervision cap_requested_without_delegation_falls_back`
- `devkit::supervision memory_restart_gives_up_within_budget`
- `devkit::supervision health_probe_restarts_hung_server`
- `devkit::supervision memory_restart_over_limit_server`
- `devkit::supervision restart_after_kill`
- `devkit::supervision restart_survives_concurrent_snapshot`
- `devkit::supervision second_supervise_of_live_server_is_noop`
- `devkit::supervision supervised_python_server_becomes_ready`
- `devkit-locks tests::facade_without_daemon_uses_flock_path`
- `devkit-locks tests::resolved_fns_roundtrip_via_flock_path`
- `devkit-locks tests::resolver_scopes_each_batch_member_to_its_own_repository`
- `devkit-mcp locks::tests::acquire_status_release_roundtrip_through_handlers`
- `devkit-ports registry::liveness_tests::detects_bound_port`
- `devkit-ports registry::ops_tests::allocation_skips_a_port_a_stray_process_is_listening_on`
- `devkit-ports run::tests::bring_down_ports_releases_listed_reservations`
- `devkit-ports run::tests::bring_down_releases_a_pidless_reservation`
- `devkit-ports run::tests::launch_non_blocking_returns_before_readiness_then_status_flips`
- `devkit-ports run::tests::read_log_tails_a_tracked_logfile`
- `devkit-ports run::tests::server_rows_marks_a_listening_entry_ready`
- `devkit-ports strays::os::tests::real_port_probe_reports_a_bound_listener`
- `devkit-ports run::tests::resolve_ports_includes_an_app_referenced_via_ports_template`

The reported causes were `Operation not permitted (os error 1)` when the
sandbox tried to bind Unix sockets or probe a listener, and
`Read-only file system (os error 30)` when tests wrote state, lock, registry,
or cache files. The test-local temporary paths varied per run. The lock claim
also remained unavailable: `lockm acquire ... --as
01a097ae-75d9-79a0-a2db-a9bde7231f68` failed with `Read-only file system (os
error 30)`. No C dependency was added.

Commit identity for the handoff:

- Initial Task 9 commit: `99f0f23`.
- First review fix commit: `558408d`.
- Second review fix commit: `3b1a664`.
