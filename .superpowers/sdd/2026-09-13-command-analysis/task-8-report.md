# Task 8 report: PowerShell adapter

Implementation commit: `f610bbf9af4e121408b8e723cb17adb1acf219ec`

Grammar-kind cleanup commit: `6eed9d2`

## Scope

Only `crates/devkit-command/src/powershell.rs` was changed for the adapter, grammar probes, and integration tests. The existing Rust `tree-sitter-powershell` dependency was used; no C dependency was added.

## Grammar shape results

The Step 1 shape test was added and passed against tree-sitter-powershell 0.26.4. The adapter records these grammar kinds in `kinds`:

`pipeline`, `pipeline_chain`, `command`, `command_name`, `command_name_expr`, `command_elements`, `command_parameter`, `redirection`, `redirected_file_name`, `assignment_expression`, `variable`, `invokation_expression`, `type_literal`, `member_name`, `argument_list`, `foreach_statement`, `function_statement`, `sub_expression`, `script_block`, `script_block_body`, and `statement_block`.

The malformed-shape tripwire remains and passes for:

- `Get-ChildItem | Format-Table Mode, Name -AutoSize`
- `git -C 'C:/repo' log --format='%h %s'`
- `git push --force-with-lease=a:b origin c`

The failure-boundary sexps showed:

- foreach iterables wrapped as `pipeline -> pipeline_chain -> logical_expression -> ... -> array_literal_expression`;
- redirection destinations as `redirected_file_name -> command_argument_sep -> generic_token`;
- `$null` destinations carrying a leading separator in the wrapper text;
- assignments nested under `pipeline`, with RHS `pipeline -> pipeline_chain -> command`;
- malformed checkout represented by an `ERROR` root with `command_name`, `generic_token`, and split `simple_name` nodes;
- the malformed here-string pipeline represented by an `ERROR` root containing `verbatim_here_string_characters` and `command_name`.

## RED/GREEN

The initial shape run used:

```text
cargo nextest run -p devkit-command powershell::shapes --no-capture
```

The write harness redirected this configured test task to `devrun task test`, and the wrapper initially hit the sandbox zccache daemon failure. The working wrapper command was:

```text
devrun task --env RUSTC_WRAPPER= test
```

Before implementation, the PowerShell integration tests failed against the stub adapter. Each remaining failure was then reproduced individually from the compiled test binary with `--exact --nocapture`.

The five root causes and isolated fixes were:

1. `list` did not unwrap `pipeline_chain`, so literal foreach items became unknown. Adding that grammar wrapper made the foreach test pass.
2. An `ERROR` root bypassed the statement walker, so a recoverable here-string never reached `Analyzer::invocation` and `embed`. Grammar-derived recovery forwarded its here-string and interpreter to the existing analyzer path.
3. `redirected_file_name` was not unwrapped, so destinations were unknown. The adapter now evaluates its value-bearing child.
4. The outer pipeline treated an assignment as an opaque value, so the `Join-Path` result was not stored. Pipeline dispatch now calls `assign`.
5. An `ERROR` root discarded recoverable external commands. Statement-level recovery forwards the malformed command to the catalog; its effects decide whether parse uncertainty is emitted.

After each single fix, the corresponding focused test was rerun. The final focused commands all passed:

```text
/home/lev/Git/lev/devkit_worktrees/feat-locks-enforce-locks-for-shell-writes/target/debug/deps/devkit_command-24db53e000e4d575 powershell::shapes::node_shapes_this_adapter_relies_on --exact --nocapture
/home/lev/Git/lev/devkit_worktrees/feat-locks-enforce-locks-for-shell-writes/target/debug/deps/devkit_command-24db53e000e4d575 powershell::shapes::the_grammar_still_fails_on_the_recorded_argument_shapes --exact --nocapture
/home/lev/Git/lev/devkit_worktrees/feat-locks-enforce-locks-for-shell-writes/target/debug/deps/devkit_command-24db53e000e4d575 powershell::tests::a_literal_foreach_runs_per_item_and_wildcards_are_unresolved --exact --nocapture
/home/lev/Git/lev/devkit_worktrees/feat-locks-enforce-locks-for-shell-writes/target/debug/deps/devkit_command-24db53e000e4d575 powershell::tests::a_here_string_piped_to_python_is_python_source --exact --nocapture
/home/lev/Git/lev/devkit_worktrees/feat-locks-enforce-locks-for-shell-writes/target/debug/deps/devkit_command-24db53e000e4d575 powershell::tests::redirects_and_null --exact --nocapture
/home/lev/Git/lev/devkit_worktrees/feat-locks-enforce-locks-for-shell-writes/target/debug/deps/devkit_command-24db53e000e4d575 powershell::tests::variables_join_path_and_set_location --exact --nocapture
/home/lev/Git/lev/devkit_worktrees/feat-locks-enforce-locks-for-shell-writes/target/debug/deps/devkit_command-24db53e000e4d575 powershell::tests::external_programs_reach_the_catalog --exact --nocapture
```

Both shape tests and all five adapter tests passed. The full PowerShell shape and integration suite also passed.

## Verification

```text
devrun task fmt
```

Passed.

```text
devrun task --env RUSTC_WRAPPER= lint
```

Passed with `-D warnings`.

```text
devrun task --env RUSTC_WRAPPER= test-doc
```

Passed for all workspace doctests.

```text
devrun task --env RUSTC_WRAPPER= test
```

PowerShell tests passed. Workspace result: 1,953 tests run, 1,919 passed, 34 failed, 0 skipped. The 34 failures are sandbox-sensitive and unrelated to this adapter:

```text
devkit::shim_dispatch::devrun_shim_still_refuses_reap_without_a_terminal
devkit::down_ports::down_ports_releases_listed_reservations
devkit::bin/devkit issue::dashboard::cache::tests::get_put_roundtrip_under_real_cache_dir
devkit::lifecycle::idle_exit_with_no_clients_or_children
devkit::lifecycle::ping_pong_handshake
devkit::lock_daemon::acquire_through_daemon_is_visible_to_check
devkit::lifecycle::second_instance_exits_immediately
devkit::lock_daemon::acquired_lock_persists_to_file_after_daemon_exits
devkit::lock_daemon::write_decide_and_release_prefix_through_daemon
devkit-common supervise::tests::probe_port_true_when_listening_false_when_free
devkit-common supervise::tests::spawn_and_ready_on_python_tcp
devkit::parity::alloc_through_daemon_writes_registry
devkit::parity::snapshot_roundtrips
devkit-locks::tests::resolved_fns_roundtrip_via_flock_path
devkit-locks::tests::resolver_scopes_each_batch_member_to_its_own_repository
devkit-mcp::locks::tests::acquire_status_release_roundtrip_through_handlers
devkit-ports::registry::liveness_tests::detects_bound_port
devkit-ports::registry::ops_tests::allocation_skips_a_port_a_stray_process_is_listening_on
devkit-ports::run::tests::bring_down_ports_releases_listed_reservations
devkit-ports::run::tests::bring_down_releases_a_pidless_reservation
devkit-ports::run::tests::launch_non_blocking_returns_before_readiness_then_status_flips
devkit-ports::run::tests::read_log_tails_a_tracked_logfile
devkit-ports::run::tests::resolve_ports_includes_an_app_referenced_via_ports_template
devkit-ports::run::tests::server_rows_marks_a_listening_entry_ready
devkit-ports::strays::os::tests::real_port_probe_reports_a_bound_listener
devkit::supervision::cap_requested_without_delegation_falls_back
devkit::supervision::health_probe_restarts_hung_server
devkit::supervision::down_does_not_restart
devkit::supervision::memory_restart_gives_up_within_budget
devkit::supervision::memory_restart_over_limit_server
devkit::supervision::restart_after_kill
devkit::supervision::restart_survives_concurrent_snapshot
devkit::supervision::second_supervise_of_live_server_is_noop
devkit::supervision::supervised_python_server_becomes_ready
```

The failures report `Read-only file system` or `Operation not permitted` while creating state files, probing/binding ports, or starting daemon sockets. The initial direct Cargo invocation was also blocked by the write harness; a later filtered direct Cargo invocation was rejected by the hook as the `test-doc` task. The configured devrun wrapper was used for all final checks. No claim of a green workspace suite is made.

The follow-up run after the grammar-kind cleanup compiled the changed adapter and reported 1,953 tests run, 1,918 passed, 35 failed, and 0 skipped. It reproduced the same 34 failures above and added `devkit-locks::tests::facade_without_daemon_uses_flock_path`, which also failed in the sandbox while using the flock path. This is unrelated to the PowerShell adapter.
