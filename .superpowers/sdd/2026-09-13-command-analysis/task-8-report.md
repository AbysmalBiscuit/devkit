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

## Review fix round

This round stayed within Task 8. It changed only `crates/devkit-command/src/powershell.rs` and this report; no Task 9 behavior was added. No C dependency was added.

### Grammar and focused coverage

The grammar-derived `kinds` module remains the source of truth for tree-sitter-powershell 0.26.4. The recorded kinds remain:

`pipeline`, `pipeline_chain`, `command`, `command_name`, `command_name_expr`, `command_elements`, `command_parameter`, `redirection`, `redirected_file_name`, `assignment_expression`, `variable`, `invokation_expression`, `type_literal`, `member_name`, `argument_list`, `foreach_statement`, `function_statement`, `sub_expression`, `script_block`, `script_block_body`, and `statement_block`.

The malformed-shape tripwire remains unchanged and passed for the three recorded argument shapes. The final focused commands were:

```text
/home/lev/Git/lev/devkit_worktrees/feat-locks-enforce-locks-for-shell-writes/target/debug/deps/devkit_command-b94aadffacfba209 powershell::shapes --nocapture
/home/lev/Git/lev/devkit_worktrees/feat-locks-enforce-locks-for-shell-writes/target/debug/deps/devkit_command-b94aadffacfba209 powershell::tests --nocapture
```

Results: 2 shape/tripwire tests passed and 19 PowerShell adapter tests passed. The relevant observed recovery shapes were an `ERROR` root containing the malformed here-string plus `command_name`, an `ERROR` root containing a malformed `command` plus sibling `simple_name` nodes, and an invocation argument list whose recursive flag is a `variable` node for `$true`.

### RED, evidence, root cause, and GREEN

Each regression was added before its implementation fix. The configured test wrapper was used because direct Cargo execution is hook-blocked:

```text
devrun task --env RUSTC_WRAPPER= test
```

1. `a_malformed_here_string_does_not_skip_following_writes` was RED. The malformed root sexp contained the here-string pipeline and no separately walked sibling; the result contained only `C:/repo/h.txt`, not `C:/repo/after.txt`. Root cause: `walk` returned immediately after `recover_here_string`, so recovery was root-scoped rather than statement-scoped. Fix: recover the here-string, then analyze the source tail in the current scope. GREEN: the test passed and both file targets were present.

2. `a_malformed_known_cmdlet_preserves_parse_error` was RED. The grammar sexp was:

```text
(ERROR (command command_name: (command_name) command_elements: (command_elements (command_argument_sep) (command_parameter) (command_argument_sep) (generic_token) (command_argument_sep) (command_parameter) (command_argument_sep) (generic_token) (command_argument_sep))))
```

The result was a `Set-Content` invocation with no effect and no uncertainty. Root cause: recovery classified effectfulness only through the external catalog, which does not model native PowerShell cmdlets, so a malformed known write cmdlet was silently accepted. Fix: known effectful cmdlets retain `ParseError`; read-only known cmdlets remain silent. GREEN: `ParseError` is present.

3. `dotnet_directory_delete_is_tree_only_when_recursive` was RED. The actual invocation sexp contains `invokation_expression`, `type_literal`, `member_name`, `argument_list`, two `argument_expression` children, and a `variable` node for `$true`; before the fix both `[IO.Directory]::Delete('build')` and the two-argument form emitted a file effect. Root cause: the combined `io.file.delete`/`io.directory.delete` arm always called `file_effect`, and `$true` was unresolved because the PowerShell automatic boolean variables were not modeled. Fix: recognize `$true`/`$false` and emit a tree effect only for the recursive directory form. GREEN: nonrecursive deletion is a file effect; recursive deletion is tree-only.

4. `malformed_recovery_uses_current_location` was RED. After `Set-Location sub`, the recovered `git checkout -- a.rs |` invocation had `cwd: Some("C:/repo")` and effects at `C:/repo/a.rs` and `C:/repo/|`, while the live scope had already changed. Root cause: `recover_command` used the immutable outer `frame.cwd` instead of the current statement scope. Fix: pass the current `Scope` through `broken` and `recover_command`. GREEN: the recovered target is under `C:/repo/sub`, with no `C:/repo/a.rs` target.

5. The node and value limit regressions were isolated separately.

   - `malformed_here_string_recovery_respects_node_limit` first used an embedded Python body and falsely passed because the embedded adapter consumed the shared budget. The diagnostic was replaced with `@'\n'@ | python - |`, `nodes = 1`; the genuine RED result was empty with no uncertainty. Root cause: `find_kind` recursively traversed the malformed tree without charging `Analyzer::budget`. Fix: the shared PowerShell `visit` guard charges the traversal and emits `LimitExhausted(Nodes)`. GREEN: the test passes with the existing limit uncertainty.
   - `malformed_error_recovery_respects_node_limit` used `Get-ChildItem | Format-Table Mode, Name -AutoSize`, `nodes = 20`. Its actual sexp was `(ERROR (pipeline (pipeline_chain (command command_name: (command_name) command_elements: (command_elements (command_argument_sep))) (command command_name: (command_name) command_elements: (command_elements (command_argument_sep) (generic_token))))) (simple_name) (simple_name))`. RED produced an invocation and no uncertainty. Root cause: `broken`'s explicit stack walk did not charge nodes. Fix: every popped error-tree node uses the same `visit` guard. GREEN: `LimitExhausted(Nodes)` is emitted without treating the partial recovery as known.
   - `oversized_powershell_values_are_not_emitted_as_targets` generated a 65 KiB literal path. RED recorded an oversized `FileEffect::Overwrite` path and no uncertainty. Root cause: literal, expanded, composed, recovered, and resolved PowerShell values reached `Analyzer::file_effect` without the shared 64 KiB check. Fix: bounded raw and cwd-resolved values now produce `LimitExhausted(ValueSize)` and become `Value::Unknown` at cmdlet, redirection, .NET, tree-effect, recovery, location, and path-composition boundaries. GREEN: the result has one unresolved target and the existing value-limit uncertainty.

### Verification

```text
devrun task fmt
```

Passed (`cargo +nightly fmt --all`).

```text
devrun task --env RUSTC_WRAPPER= test
```

The wrapper ran `cargo nextest run --workspace --all-features --locked --no-fail-fast`. Result: 1,960 tests run, 1,926 passed, 34 failed, 0 skipped. All PowerShell shape and adapter tests passed. The 34 failures are sandbox-sensitive and were unchanged in kind: daemon/socket binding and process probes report `Operation not permitted`, while lock/cache paths report `Read-only file system`.

```text
devrun task --env RUSTC_WRAPPER= lint
```

Passed: workspace clippy with `--all-targets --all-features --locked -- -D warnings`.

```text
devrun task --env RUSTC_WRAPPER= test-doc
```

Passed: all workspace doctest binaries completed with zero failures.

The exact failing test names from the final nextest run follow in the next subsection. The source diff contains no Cargo or grammar dependency change, so no C dependency was introduced. The review-fix commit hash is recorded in the post-commit identity section below.

### Final sandbox-sensitive failures

```text
devkit::shim_dispatch devrun_shim_still_refuses_reap_without_a_terminal
devkit::down_ports down_ports_releases_listed_reservations
devkit::lifecycle ping_pong_handshake
devkit::lifecycle idle_exit_with_no_clients_or_children
devkit::lifecycle second_instance_exits_immediately
devkit::lock_daemon acquire_through_daemon_is_visible_to_check
devkit::lock_daemon acquired_lock_persists_to_file_after_daemon_exits
devkit::lock_daemon write_decide_and_release_prefix_through_daemon
devkit::parity alloc_through_daemon_writes_registry
devkit::parity snapshot_roundtrips
devkit::bin/devkit issue::dashboard::cache::tests::get_put_roundtrip_under_real_cache_dir
devkit-common supervise::tests::probe_port_true_when_listening_false_when_free
devkit-common supervise::tests::spawn_and_ready_on_python_tcp
devkit::supervision cap_requested_without_delegation_falls_back
devkit::supervision down_does_not_restart
devkit::supervision health_probe_restarts_hung_server
devkit::supervision memory_restart_gives_up_within_budget
devkit::supervision memory_restart_over_limit_server
devkit::supervision supervised_python_server_becomes_ready
devkit::supervision restart_after_kill
devkit::supervision second_supervise_of_live_server_is_noop
devkit::supervision restart_survives_concurrent_snapshot
devkit-locks tests::resolved_fns_roundtrip_via_flock_path
devkit-locks tests::resolver_scopes_each_batch_member_to_its_own_repository
devkit-mcp locks::tests::acquire_status_release_roundtrip_through_handlers
devkit-ports registry::liveness_tests::detects_bound_port
devkit-ports registry::ops_tests::allocation_skips_a_port_a_stray_process_is_listening_on
devkit-ports run::tests::bring_down_ports_releases_listed_reservations
devkit-ports run::tests::bring_down_releases_a_pidless_reservation
devkit-ports run::tests::read_log_tails_a_tracked_logfile
devkit-ports run::tests::server_rows_marks_a_listening_entry_ready
devkit-ports run::tests::launch_non_blocking_returns_before_readiness_then_status_flips
devkit-ports strays::os::tests::real_port_probe_reports_a_bound_listener
devkit-ports run::tests::resolve_ports_includes_an_app_referenced_via_ports_template
```

The failures report `Read-only file system` or `Operation not permitted` while creating state files, probing/binding ports, or starting daemon sockets. The initial direct Cargo invocation was also blocked by the write harness; a later filtered direct Cargo invocation was rejected by the hook as the `test-doc` task. The configured devrun wrapper was used for all final checks. No claim of a green workspace suite is made.

The follow-up run after the grammar-kind cleanup compiled the changed adapter and reported 1,953 tests run, 1,918 passed, 35 failed, and 0 skipped. It reproduced the same 34 failures above and added `devkit-locks::tests::facade_without_daemon_uses_flock_path`, which also failed in the sandbox while using the flock path. This is unrelated to the PowerShell adapter.

## Cleanup verification

Cleanup commit: `6eed9d243ac5281827fff5ecbead0075ee086ca1` (`fix(command): centralize PowerShell grammar kinds`). No production code changed while appending this section.

Focused verification used the two grammar shape/tripwire tests and the five PowerShell adapter tests listed above, each with `--exact --nocapture`; all seven passed.

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

Final workspace result: 1,953 tests run, 1,918 passed, 35 failed, 0 skipped. The 35 failures were sandbox-sensitive:

```text
devkit::shim_dispatch::devrun_shim_still_refuses_reap_without_a_terminal
devkit::down_ports::down_ports_releases_listed_reservations
devkit::bin/devkit issue::dashboard::cache::tests::get_put_roundtrip_under_real_cache_dir
devkit::lifecycle::idle_exit_with_no_clients_or_children
devkit::lifecycle::ping_pong_handshake
devkit::lifecycle::second_instance_exits_immediately
devkit::lock_daemon::acquired_lock_persists_to_file_after_daemon_exits
devkit::lock_daemon::acquire_through_daemon_is_visible_to_check
devkit::lock_daemon::write_decide_and_release_prefix_through_daemon
devkit-common supervise::tests::probe_port_true_when_listening_false_when_free
devkit-common supervise::tests::spawn_and_ready_on_python_tcp
devkit::parity::alloc_through_daemon_writes_registry
devkit::parity::snapshot_roundtrips
devkit-locks tests::facade_without_daemon_uses_flock_path
devkit-locks tests::resolved_fns_roundtrip_via_flock_path
devkit-locks tests::resolver_scopes_each_batch_member_to_its_own_repository
devkit-mcp locks::tests::acquire_status_release_roundtrip_through_handlers
devkit-ports registry::liveness_tests::detects_bound_port
devkit-ports registry::ops_tests::allocation_skips_a_port_a_stray_process_is_listening_on
devkit-ports run::tests::bring_down_ports_releases_listed_reservations
devkit-ports run::tests::bring_down_releases_a_pidless_reservation
devkit-ports run::tests::launch_non_blocking_returns_before_readiness_then_status_flips
devkit-ports run::tests::server_rows_marks_a_listening_entry_ready
devkit-ports strays::os::tests::real_port_probe_reports_a_bound_listener
devkit-ports run::tests::read_log_tails_a_tracked_logfile
devkit-ports run::tests::resolve_ports_includes_an_app_referenced_via_ports_template
devkit::supervision::health_probe_restarts_hung_server
devkit::supervision::down_does_not_restart
devkit::supervision::memory_restart_over_limit_server
devkit::supervision::cap_requested_without_delegation_falls_back
devkit::supervision::memory_restart_gives_up_within_budget
devkit::supervision::restart_after_kill
devkit::supervision::restart_survives_concurrent_snapshot
devkit::supervision::second_supervise_of_live_server_is_noop
devkit::supervision::supervised_python_server_becomes_ready
```

### Post-commit identity

Review-fix commit: `e5126fd` (`fix(command): harden PowerShell recovery`). The report update is intentionally a separate documentation-only commit so this section can record the exact fix commit hash.
