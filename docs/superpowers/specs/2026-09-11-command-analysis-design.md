# Shared command analysis

## Purpose

Issue [#33](https://github.com/AbysmalBiscuit/devkit/issues/33) extends file-lock enforcement to edits made through shell tools. A shared `devkit-command` crate analyzes commands and embedded scripts so write enforcement, task guards, and configurable command rules use the same interpretation of executable code.

The analyzer uses Tree-sitter for Bash, PowerShell, Python, JavaScript, TypeScript, and fish. It identifies supported file operations and their targets without running commands. When it cannot determine a relevant effect or target, it reports uncertainty explicitly. The consuming policy decides whether to block, warn, or allow.

This replaces the shell guard's handwritten lexer as the source of command structure. It preserves the existing structured-edit hook and lock ownership model.

## Measurements this spec relies on

Every count below comes from the shell-call corpus in the primary checkout, snapshot `outputs-20260912T094805354846Z`: 37,641 recorded pre-execution attempts across 98 sessions and two machines. Regenerate it with `uv run data_analysis.local/run_analysis.py` and compare against a later snapshot before treating any number here as current. A count of recorded attempts is not a precision or recall result for write detection.

| Cohort | Attempts | Dialect | Signal that establishes it |
|---|---|---|---|
| claude-code/wsl `Bash` | 16,834 | bash | default |
| codex/wsl `Bash` | 15,434 | bash | default |
| claude-code/windows `Bash` | 3,680 | bash, through Git Bash | default |
| codex/windows `Bash` | 1,546 | PowerShell 7 | harness and platform |
| claude-code/windows `PowerShell` | 147 | PowerShell | tool name |

Three facts from that snapshot shape the design:

- No cohort supplies shell or working-directory metadata in its payload. Every record carries `cwd`.
- Every Codex execution on Windows runs `pwsh.exe`, from the Codex session transcripts; no `powershell.exe` execution appears.
- On all 147 Claude Code `PowerShell` records the collected environment reports `SHELL=C:\Program Files\Git\bin\bash.exe` and `MSYSTEM=MINGW64`, because the hook process itself runs under Git Bash.

## Agreed scope

- A separate reusable analyzer crate, with Tree-sitter as the v1 backend.
- Shell command structure, redirects, executable substitutions, interpreter wrappers, and ad-hoc scripts passed as arguments or through stdin.
- Embedded Python, JavaScript, and TypeScript, including common import aliases and simple path construction.
- Native PowerShell syntax and file operations, including here-strings and .NET file APIs.
- Automatic claims for statically resolved write targets under `enforce_writes`.
- Configurable treatment of unresolved writes, unsupported languages, and existing script-file invocations.
- Shared parsed-command matching for configured rules, task redirects, and dev-server guards.
- Correctness tests through the real hook entry point, including missed-write and false-block cases.

Benchmarking is deferred at the user's request. Parallel analysis is deferred with it; see "Performance". Performance shapes the implementation, but v1 must not claim a measured latency or detection-accuracy result that has not been established.

## Boundaries

`devkit-command` owns parsing, command normalization, bounded value tracking, and effect analysis. It has no dependency on devkit's configuration loader, hook envelopes, file-lock registry, task catalog, or daemon. It does not read script files, inspect transcripts, spawn interpreters, execute snippets, or create a worker pool.

The crate accepts either source text with an explicit analysis context or an argument vector from a caller that already has one. Configured task and launch arguments enter normalization directly; they are not joined into a shell string and reparsed.

`devkit-common` owns harness payload adaptation and configuration discovery. `devkit-ports::guard` owns task, application, and command-rule decisions. `devkit-locks` owns path-to-registry resolution, session ownership, and claims. The `devkit harness shell` entry point composes these consumers and emits the harness response.

The write stage covers Claude Code and Codex. Cursor keeps the command guard alone: its payload carries `conversation_id` rather than a session identity, it has no write or session-lifecycle hook registration, and the holder-identity design excludes it for that reason. Enabling claims there would deny every Cursor shell write. Restoring Cursor is a matter of giving it the same identity and lifecycle registrations the other two have.

The command-decision component remains free of mutations. The shell hook gains a separate write-enforcement stage when `enforce_writes` is enabled. The existing invariant that the entire shell entry point never acquires locks must be rewritten accordingly; command-only guarding continues to leave registries untouched.

## Analysis contract

The result carries distinct collections of:

| Result | Meaning |
|---|---|
| Invocations | Program and argument values, preserving unknown values, wrappers, nested invocations, source locations, and established execution context |
| File effects | Supported create, overwrite, append, delete, rename, and copy operations, with resolved target paths or an explicit unresolved target |
| Tree effects | Operations that rewrite an unenumerated set of files under a directory, carrying that directory as their scope |
| Script-file invocations | Calls to an existing script file whose contents were not supplied to the analyzer |
| Uncertainties | Unsupported executable constructs, source in a language with no analyzer, dynamic effects, unresolved values or context, parse failures, and exhausted analysis limits |

Uncertainty belongs to the affected operation or executable region. Discovering one unknown does not discard known effects elsewhere in the command. Consumers receive the complete result before deciding whether execution is allowed.

Source locations identify the original command or the embedded source that produced a finding. Decoded strings retain their enclosing invocation location so diagnostics do not claim an incorrect position in the outer command.

Wrappers remain visible alongside their inner invocations. A rule targeting `uv` can match the wrapper while another consumer analyzes the Python script it launches. Normalization must retain options that change execution context rather than blindly deleting every flag.

## Runtime input and execution context

The detector uses the incoming hook payload and explicit devkit configuration. Collector-added environment hints, transcript contents, result events, and metadata scraped from another source do not become runtime inputs.

Preserve the supplied session, agent, call identity, command, shell, hook directory, and requested working directory as separate fields. Missing fields remain missing. The adapter must not invent a session identity for automatic claims.

### Shell dialect

A tool named `Bash` does not establish the shell dialect, and neither does the hook process's own environment. `SHELL` and `MSYSTEM` describe the process devkit runs in, which on Claude Code's Windows `PowerShell` tool is Git Bash while the command itself runs in PowerShell.

`[harness] shell` accepts `auto`, `bash`, or `powershell`, and defaults to `auto`. An explicit setting wins over detection. In `auto`, resolve in this order:

1. Explicit supported shell metadata in the payload, if a harness ever supplies it. None does today.
2. A tool name that names its interpreter. Claude Code's `PowerShell` tool means PowerShell.
3. The harness and platform. Codex on Windows means PowerShell, because every Codex Windows execution in the corpus runs `pwsh.exe`.
4. Bash.

An explicitly unsupported shell is not silently reinterpreted as Bash; it is source in an unsupported language and follows `unsupported_language`.

Step 3 requires telling Codex from Claude Code, which the current adapter does not do: both resolve to `Harness::ClaudeCode`. Codex payloads carry `turn_id` and `model`; Claude Code payloads carry `prompt_id` and `transcript_path`. Adapt the harness from those fields rather than from the presence of a payload alone.

An explicit interpreter invocation establishes the dialect of its embedded source whatever the outer dialect is. A bash command that calls `powershell -Command '...'` supplies PowerShell source, and a PowerShell command that calls `python3 -c '...'` supplies Python source.

Path analysis respects the selected shell and path form. It does not use the analyzer host's path rules to reinterpret Windows source as Unix source. Filesystem canonicalization and lock-key resolution remain in the existing lock facade, which resolves a path lexically when it already lies under the checkout root. A target reached through a symlink inside the root therefore keys differently from its real path. That behavior is unchanged by this design and out of scope here.

### Execution directory

The payload's `cwd` establishes the execution directory. A statically determined directory change inside the command (`cd`, `Set-Location`) moves it in execution order. A relative target with no established directory is unresolved. An absolute target needs no directory.

Claude Code's Bash tool keeps a shell whose directory can drift from the `cwd` the payload reports. That risk is accepted: a `cd` in the command being analyzed is tracked, `cwd` is present on every recorded call, and the alternative blocks ordinary relative writes. Structured edit tools retain their existing documented path-resolution behavior.

## Parsing and executable source

Use the Tree-sitter runtime and language grammars through their Rust bindings, compiled into the binary. Python and Node installations are not runtime requirements for analysis.

The grammars are `tree-sitter-bash`, `tree-sitter-powershell` (airbus-cert), `tree-sitter-python`, `tree-sitter-javascript`, `tree-sitter-typescript` including its TSX entry point where the source context calls for it, and `tree-sitter-fish`. Pin compatible releases in `Cargo.lock`; the grammars couple to the runtime through `tree-sitter-language` and the parser ABI rather than a shared `tree-sitter` semver, so a build on each supported platform is what establishes compatibility.

Both published PowerShell grammars were measured against the corpus. `tree-sitter-pwsh` is a fork of airbus-cert's grammar and fails on exactly the same commands; airbus-cert's is the upstream and the more used crate, so v1 takes it.

PowerShell parse failures are a design constraint rather than an edge case. Measured on the corpus: 61 of 147 Claude Code `PowerShell` commands and 216 of 1,546 Codex Windows commands produce an `ERROR` or `MISSING` node, against 26 of 35,948 for `tree-sitter-bash` on the bash cohorts. The failures come from argument shapes, not from exotic syntax. These three minimal cases each fail in both grammars:

```powershell
Get-ChildItem | Format-Table Mode, Name -AutoSize
git -C 'C:/repo' log --format='%h %s'
git push --force-with-lease=a:b origin c
```

Of the failing commands, 28 and 21 respectively contain a write marker, so a rule that discards a whole command on any parse error would block roughly a fifth of Claude Code's PowerShell calls.

Uncertainty from a parse failure therefore attaches to the smallest enclosing statement, not to the command. Effects and invocations established in sibling statements stand. A redirect, assignment, or invocation inside an `ERROR` region is unresolved, and so is anything whose value depends on it. `ERROR` and `MISSING` nodes, parser cancellation, and unsupported executable constructs are never successful analysis of the region that contains them.

Language adapters use syntax nodes and fields to identify executable structure. They do not scan arbitrary strings for command names or file APIs.

Extract embedded source only from a recognized execution form. V1 covers shell command arguments, interpreter evaluation arguments, stdin and heredoc scripts, PowerShell here-string execution forms, and recognized runner wrappers. Model the wrapper's argument grammar before identifying its script argument.

Ordinary strings and heredoc data remain data. Shell substitutions inside expandable strings or heredocs are executable and must be inspected separately. If shell expansion can change the embedded source and v1 cannot resolve it, report uncertainty rather than parsing the unexpanded text as the program that will run.

Nested interpreters recurse through the same bounded analysis interface. A constant subprocess argument vector can feed command analysis directly. A constant shell command passed to a process API feeds the appropriate shell parser. Dynamic command construction remains unknown.

Inline source in a language with no analyzer is its own finding, not a write and not a harmless call. `nu -c`, `awk` programs, `perl -e`, and `ruby -e` all reach the analyzer as an interpreter wrapper whose source it cannot read. These follow `unsupported_language`. Adding a grammar moves a language out of this class; fish is in v1 for that reason, since its recorded calls are read-only completion probes that would otherwise be blocked.

## Supported effect analysis

### The effect catalog

Supported effects come from a catalog built into `devkit-command`, extended by pull request and never by config, the way the dev-server catalog in `devkit-ports::guard` already is. A project that wants to refuse a command of its own writes a `[harness.commands.<name>]` rule; it does not declare write verbs.

Each entry maps a program and an argument form to an effect and a target rule. Target rules are: the operands, the value of a named option, a redirect target, or a directory scope. Entries cover at least the redirect forms, the in-place editors (`sed -i`, `perl -pi`), the file-management commands (`cp`, `mv`, `rm`, `install`, `tee`, `dd of=`, `touch`, `mkdir`), the git verbs that write the working tree, and the whole-tree formatters. The corpus's own frequencies rank the work: Python is the dominant ad-hoc editor, `sed -i` and `perl -pi` are the common in-place forms, and `git checkout`, `merge`, `rebase`, `cherry-pick`, `reset`, and `stash` are the common tree rewrites.

**A program outside the catalog produces no file effect.** That is the boundary the whole design rests on, and it is the reason unknown executable source is a separate finding rather than a silent pass: an unmodeled program is a program devkit has no opinion about, while an unreadable script is a hole an agent could write its edit through.

Two consequences follow, both accepted:

- Incidental writes by build tools, package managers, and caches are not discovered. `cargo build` and `npm install` write files and produce no finding.
- `devrun task <name>` produces no finding even when the task formats the tree. Resolving a task allocates ports and renders commands, and the shell guard must not mutate a registry, so the hook cannot expand a task invocation. This is a known gap; closing it needs an allocation-free task resolution path.

### Tree effects

A whole-tree writer, such as `cargo fmt`, `ruff format` with no path operand, `taplo fmt`, `prettier --write .`, or a git verb that rewrites the working tree, names a directory rather than a file set. Enumerating what it will touch is not statically possible, and claiming a directory is not something the lock registry models.

A tree effect is therefore a conflict check, not a claim: execution is denied when another session holds a lock on any path under the scope, and nothing is claimed when it does not. This uses the existing read paths on the lock facade rather than a new lock kind. A tree effect is not an unresolved write and does not follow `unresolved_writes`; its scope is known even though its file set is not.

### Shell and PowerShell

Preserve command chains, pipelines, redirects, substitutions, and nested script execution. Track simple literal assignments and statically established directory changes in execution order. A branch or scope that makes a binding ambiguous invalidates that binding rather than choosing one value.

A copy locks its destination; a rename or move locks both the removed source and the destination. Unsupported options that can change the target set make the operation unresolved.

PowerShell analysis recognizes supported content and file cmdlets and `System.IO.File`/`System.IO.Directory` operations. Track simple variables, literal paths, and supported path joins. Dynamic providers, splatting, wildcard expansion, computed invocation, and aliases without an established binding remain unresolved when they affect a write or command decision.

Redirections are evaluated independently of command-rule exemptions. For example, allowing `devrun task check` does not exempt the target in `devrun task check > shared.txt`.

### Python

Recognize supported `pathlib`, built-in file opening, `os`, and `shutil` operations. Handle literal write modes, simple assignments, imported aliases, path composition, and interpolation whose inputs are known constants.

Receiver names alone do not establish types. Rebinding an imported module, constructor, file handle, or path value invalidates the associated knowledge. Unknown modes, computed callees, dynamic imports, and unmodeled calls that may write produce uncertainty.

Function bodies are not treated as executed merely because they are defined. Calls through unmodeled functions remain unknown. Analyze definition-time executable expressions where applicable; do not overlook effects in defaults or decorators.

### JavaScript and TypeScript

Recognize supported filesystem operations from Node, Bun, and Deno, including synchronous and asynchronous forms. Track supported imports, `require` bindings, destructured aliases, simple constants, path joins, and constant template strings. TypeScript syntax is parsed as syntax, not removed with text substitutions.

Shadowing or reassignment invalidates an API binding. A method named `writeFile` is not sufficient evidence of a filesystem write. Recognized read-only operations, package-resolution checks, and file-read assertions must remain useful negative fixtures; the recorded inline JavaScript is dominated by `require.resolve` probes, which must not block.

### fish

Recognize redirects, the catalog's file-management commands, and directory changes, the same way bash source is handled. fish is in v1 to keep read-only `fish -c` probes out of the unsupported-language class, not because it carries a distinct write vocabulary.

### Limits of the model

The command catalog describes supported effects; it does not infer every filesystem action performed by every external program. Incidental caches and build outputs are not discovered by inspecting installed program files. Existing script files are governed by their own invocation policy.

Unknown executable code inside an ad-hoc script must not be classified as read-only merely because none of its method names matched the supported write APIs. Known harmless operations and unmodeled executable calls are distinct outcomes.

## Configuration and policy

Keep the existing activation flags and named command tables:

```toml
[harness]
enforce_writes = true
enforce_commands = true
shell = "auto"
unresolved_writes = "block"
unsupported_language = "block"
script_files = "allow"

[harness.commands.worktree-add]
enabled = true
programs = ["git"]
args = ["worktree", "add"]
action = "block"
severity = "error"
reason = "Use devkit's issue command to create worktrees."
```

The three policy keys use one action vocabulary:

| Action | Behavior |
|---|---|
| `block` | Deny execution with an actionable diagnostic |
| `warn` | Allow execution and return an agent-facing diagnostic |
| `allow` | Allow execution silently for this policy finding |

`unresolved_writes` defaults to `block` and governs a recognized or possible write whose target could not be determined. In this mode an agent must rewrite an unresolved ad-hoc edit until the relevant effects and targets can be determined. Holding an unrelated lock or declaring intended targets does not bypass the result. There is no declaration-based retry mechanism in v1.

`unsupported_language` defaults to `block` and governs executable source in a language with no analyzer. It is separate from `unresolved_writes` so that a project can accept the languages devkit cannot yet read without also accepting an unreadable target in a language it can. Each new grammar shrinks what this key covers.

`script_files` defaults to `allow`. It governs recognized invocations of stored script files without reading those files or automatically claiming them. The classification is syntactic: the analyzer does not stat the path, so an invocation naming a script file is a script-file invocation whether or not the file exists. Choosing `warn` reports that the script's effects were not analyzed. Choosing `block` requires expressing the work through a supported invocation. An outer redirect or another independently recognized write still goes through normal write enforcement under every setting.

Known write targets always use ordinary lock enforcement when `enforce_writes` is enabled, including when unresolved effects, unsupported languages, or script files are allowed or warned about. A known ownership conflict cannot be overridden by `allow` on an unrelated finding.

Custom rules gain `enabled`, `action`, and `severity`. Their defaults preserve existing behavior: enabled, blocking, with error severity. `action` accepts `block` and `warn`; a rule that should stop matching is disabled with `enabled = false` rather than given an `allow` action, which would mean the same thing twice. `severity` accepts `info`, `warning`, and `error` and classifies the diagnostic that reaches the agent. `reason` remains the canonical message field.

`programs` matches a parsed invocation's executable basename, and `args` matches its known leading arguments after supported semantic normalization. Program-specific normalization is new behavior, not preserved behavior: `git -C /repo worktree add` does not match a `worktree add` rule today, because matching tests a prefix of the argument vector and wrapper stripping does not reach git's own options. Normalization must retain options that change execution context. Rules see wrappers and nested executable commands, but not quoted data that only mentions them.

A rule names one verb form. The shipped worktree example covers mutations, so `git worktree add` matches and `git worktree list` does not; a project that wants to refuse listing writes its own rule.

Raw regex matching is deferred from v1. The existing program and argument matcher covers the concrete requested examples. This is an implementation-scope choice; a later matcher must specify the parsed fields it matches and preserve argument boundaries.

## Inheritance and precedence

Use existing devkit table merging. Global rules are inherited by project layers, and a child can override a named rule's individual fields. `enabled = false` explicitly disables an inherited rule; legacy `programs = []` continues to match nothing.

`enforce_writes` and `enforce_commands` keep their current behavior, where any layer that enables them enables them. The new scalar keys do not ratchet: the closest layer that sets `shell`, `unresolved_writes`, `unsupported_language`, or `script_files` wins, in the same precedence order the config tables already use.

A `warn` or `allow` finding suppresses denial from itself alone. It is not a global exemption from other rules, task guards, or write enforcement. Any applicable blocking result prevents the complete tool call from executing. Warnings are emitted in stable source and rule order.

Task and application guard semantics remain in their existing consumer: task signatures, catalog precedence, app selection, and the replacement command are retained. All consume the shared invocation representation. A task guard override affects its own routing decision rather than surrounding file effects.

Parse configuration keys independently. A malformed command rule cannot disable `enforce_writes`. An invalid value for a policy key produces a diagnostic and retains that key's conservative default without affecting the others; malformed command rules preserve their existing fail-open treatment. Update the schema and configuration documentation alongside the types and resolver.

## Hook execution and locks

The shell hook performs these stages:

1. Read and adapt the payload, retaining identities and context without filling missing runtime facts from collector metadata.
2. Resolve activation, dialect, and applicable policy. An inactive path skips command analysis.
3. Analyze the command once and share the result with command guarding and write enforcement.
4. Evaluate blocking and warning policies in deterministic order.
5. If execution remains allowed, resolve every known write target through the existing lock facade and obtain the necessary claims, and check every tree-effect scope for another session's locks, before returning permission to run.
6. Emit the harness's deny or warning envelope, or allow silently.

Claims go through `WriteResolver::decide_write`, the same path structured edits use, so ownership ancestry, renewal, and per-directory root resolution behave identically. Claims use the same session and subagent identity, TTL, daemon routing, and release lifecycle as structured edits. Missing identity when claims are needed, registry failures, or ownership conflicts deny the write. No shell-hook code writes the registry behind a live daemon.

Cross-project targets do not introduce a new distributed transaction protocol: execution is denied if any claim fails, and any claims already acquired remain subject to the normal release lifecycle. All required successful claims precede execution.

The shell hook must not carry a registration timeout once it acquires claims. All three harnesses let the tool call proceed when a hook times out, so a slow or contended registry would silently produce an unlocked write. The current 10-second timeout in the hook manifests is removed for this entry point, and a stalled registry must surface as a denial.

A `warn` action needs a delivery channel, because today's non-deny output reaches stderr, which the harnesses discard on a zero exit. Claude Code and Codex both accept an allow decision carrying agent-facing context; that is the warning envelope. Where a harness has no such channel, `warn` degrades to a silent allow and the spec says so rather than pretending the agent was told.

The shell hook does not execute the captured command. It does not require result events to release locks or establish success. Post-tool outcomes and transcript audit results remain offline evidence only.

Update registrations for supported shell tool names on Claude Code, Codex, and Cursor. Claude Code's `PowerShell` tool is a registration and adapter change both: the manifests match `Bash` alone, and the payload adapter returns nothing for any other tool name, so those calls reach no guard at all today. Keep structured-edit and session-release registrations intact.

## Failure handling

Expected uncertainty about writes, including unsupported syntax, parse failures, and exhausted bounds, follows `unresolved_writes`. Source in a language with no analyzer follows `unsupported_language`. Stored script invocations follow `script_files`. A recognized write with an unresolved path is never reported as protected.

When uncertainty prevents establishing whether a command rule, task signature, or catalog restriction matches, warn and allow for that possible match. Definite matches still apply. For example, `git "$verb" add /tmp/wt` does not establish a match against a `worktree add` rule. This preserves command guarding's fail-open behavior; independently identified write uncertainty still follows the write policy when enabled.

Internal failures are distinct from expected uncertainty. When write enforcement is active, an unusable write payload, internal analysis failure, panic, or failed registry operation denies with a clear reason. The command-only guard retains its fail-open behavior and emits diagnostics without mutating registries. The existing panic boundary covers both, choosing its outcome from whether the write stage is active.

Diagnostics name the affected operation or source region and explain a usable correction. A blocking unresolved-write diagnostic asks for simpler explicit targets or a supported structured edit. It does not suggest an unrelated lock claim as proof of coverage.

## Performance

Parse each source once and reuse the result across consumers. Keep literal argument vectors structured, avoid repeated config resolution within the hook, and avoid copying entire source strings per finding. Instantiate language parsers only for executable source that needs them.

Bound source size, cumulative decoded source, parser nesting, visited syntax nodes, and constant-value growth. The initial implementation uses a 256 KiB outer-source limit, a 1 MiB cumulative source limit, nesting depth 8, 100,000 visited nodes, and a 64 KiB limit on a resolved value. These are implementation limits, not performance measurements. Exhaustion produces uncertainty without discarding findings already established.

V1 analysis is serial. Two measurements drive that: recorded commands are small, and the shared pool cannot honor a no-global-pool guarantee. `devkit_common::pool::install` runs its closure on the calling thread when the pool cannot be built, and a rayon parallel iterator inside that closure then reaches rayon's global pool, so a requirement of "serial on thread-creation failure" would need a new pool API. A later parallel path belongs with the deferred benchmarking, and would use `devkit_common::pool` from the caller rather than any pool the analyzer creates.

The discussed target is no more than 2 ms p95 of added analysis for commands up to 8 KiB on WSL and native Windows. It remains an unvalidated target. Benchmarking and timing-based acceptance tests are deferred; correctness checks remain required.

## Validation

Use the real shell-hook entry point with isolated temporary projects and registries. Required cases include:

- A free target is claimed; another session's claim denies; same-session and permitted ancestor ownership allow.
- Multiple targets, rename endpoints, outer redirects around permitted devkit commands, and registry failures. A stalled or failing registry denies rather than timing out into an unguarded run.
- `block`, `warn`, and `allow` for unresolved writes, unsupported languages, and script-file invocations; a warning or allow finding cannot override a known lock conflict.
- Tree effects: another session's lock under the scope denies, a clean scope allows, and nothing is claimed either way.
- Existing rule compatibility, inheritance, explicit disabling, per-rule actions and severity, uncertain command matching, and isolation of malformed rules from write enforcement.
- Dialect resolution per harness, platform, and tool name, including Claude Code's `PowerShell` tool, and the case where the hook process's own `SHELL` and `MSYSTEM` contradict the dialect.
- Shell wrappers, argument quoting, pipelines, substitutions, ordinary versus executable heredocs, and explicit versus missing execution context.
- Python path construction, aliases, rebinding, unknown open modes, and constant versus dynamic subprocess input.
- PowerShell here-strings, cmdlets, file APIs, path expressions, and statement-scoped recovery: a statement that fails to parse leaves a sibling statement's write enforced and its own effects unresolved.
- JavaScript and TypeScript imports, aliases, shadowing, reads, writes, asynchronous calls, and embedded process commands, with `require.resolve` probes as negative cases.
- Unsupported-language wrappers (`nu -c`, `awk`, `perl -e`) and supported fish commands.
- Quoted command text, unused function bodies, and readonly examples that must not acquire locks or trigger unrelated command rules.
- Input and nesting bounds.

Fixtures are synthetic. Write each one from the pattern a recorded call demonstrates, not from the recorded call itself: no captured command text, paths, repository names, or file contents enter the test suite. The corpus selects which patterns deserve a fixture and ranks them; it does not supply the fixture. No captured command is ever executed.

Use the existing `run_analysis.py` entry point under `data_analysis.local` in the primary checkout to regenerate corpus measurements; its successful subset is not a precision or recall result.

The PowerShell grammar's failing constructs deserve their own fixture group, written synthetically from the three minimal forms above. Each is a candidate upstream grammar issue, and each must demonstrate statement-scoped recovery rather than a whole-command denial.

Run the configured `fmt-check`, `lint`, `test`, and `test-doc` tasks, or their `verify` sequence, in the implementation worktree. Formatting uses the configured nightly formatter. Regenerate the committed JSON schema and verify its consistency test. Cross-platform CI remains the build and behavior gate for supported operating systems.

## Documentation and implementation boundaries

Update `docs/configuration.md`, `docs/commands.md`, the agent skill guidance, hook manifests, schema, and the command-guard invariant in `AGENTS.md`. That invariant is rewritten rather than narrowed: the shell entry point acquires locks when `enforce_writes` is enabled, and the existing test asserting that it writes no registry row is replaced by tests that assert which stage may write one.

The implementation plan must separate the pure analysis contract, language adapters, the effect catalog, guard and config integration, and lock-hook integration into reviewable tasks with focused failing tests. Each task must consume the same result model rather than introducing another parser or policy vocabulary.

## Review decisions

The user approved the separate crate, Tree-sitter, the language scope including JavaScript and TypeScript, configurable unresolved-write handling, a separate key for languages devkit cannot parse, tree effects as a conflict check rather than a claim, synthetic-only fixtures, and explicitly deferred benchmarking.

The issue asks how an explicit-lock retry is verified after an unresolved write is blocked. It is not: v1 has no declaration-based retry. The issue itself grants that holding a lock is not evidence that unknown targets are covered, and nothing else could verify such a declaration without running the command. The diagnostic rules and `unresolved_writes = "warn"` are the paths forward instead.

The `shell` resolution order, `unsupported_language` and `script_files` keys, dropping `allow` from rule actions, the structured-only v1 rule matcher, statement-scoped parse recovery, serial v1 analysis, the concrete analysis limits, and the Cursor and `devrun task` gaps are implementation choices made to complete this spec. Their behavior is specified above so they can be reviewed before code is written. No additional user answer is required to interpret the document.
