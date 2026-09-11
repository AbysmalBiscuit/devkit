# Shared command analysis

## Purpose

Issue [#33](https://github.com/AbysmalBiscuit/devkit/issues/33) extends file-lock enforcement to edits made through shell tools. A shared `devkit-command` crate analyzes commands and embedded scripts so write enforcement, task guards, and configurable command rules use the same interpretation of executable code.

The analyzer uses Tree-sitter for Bash, PowerShell, Python, JavaScript, and TypeScript. It identifies supported file operations and their targets without running commands. When it cannot determine a relevant effect or target, it reports uncertainty explicitly. The consuming policy decides whether to block, warn, or allow.

This replaces the shell guard's handwritten lexer as the source of command structure. It preserves the existing structured-edit hook and lock ownership model.

## Agreed scope

- A separate reusable analyzer crate, with Tree-sitter as the v1 backend.
- Shell command structure, redirects, executable substitutions, interpreter wrappers, and ad-hoc scripts passed as arguments or through stdin.
- Embedded Python, JavaScript, and TypeScript, including common import aliases and simple path construction.
- Native PowerShell syntax and file operations, including here-strings and .NET file APIs.
- Automatic claims for statically resolved write targets under `enforce_writes`.
- Configurable treatment of unresolved writes and existing script-file invocations.
- Shared parsed-command matching for configured rules, task redirects, and dev-server guards.
- Conditional parallel analysis through devkit's existing bounded Rayon pool.
- Correctness tests through the real hook entry point, including missed-write and false-block cases.

Benchmarking is deferred at the user's request. Performance shapes the implementation, but v1 must not claim a measured latency or detection-accuracy result that has not been established.

## Boundaries

`devkit-command` owns parsing, command normalization, bounded value tracking, and effect analysis. It has no dependency on devkit's configuration loader, hook envelopes, file-lock registry, task catalog, or daemon. It does not read script files, inspect transcripts, spawn interpreters, execute snippets, or create a worker pool.

The crate accepts either source text with an explicit analysis context or an argument vector from a caller that already has one. Configured task and launch arguments enter normalization directly; they are not joined into a shell string and reparsed.

`devkit-common` owns harness payload adaptation, configuration discovery, and the shared pool. `devkit-ports::guard` owns task, application, and command-rule decisions. `devkit-locks` owns path-to-registry resolution, session ownership, and claims. The `devkit harness shell` entry point composes these consumers and emits the harness response.

The command-decision component remains free of mutations. The shell hook gains a separate write-enforcement stage when `enforce_writes` is enabled. The existing invariant that the entire shell entry point never acquires locks must be narrowed accordingly; command-only guarding continues to leave registries untouched.

## Analysis contract

The result carries distinct collections of:

| Result | Meaning |
|---|---|
| Invocations | Program and argument values, preserving unknown values, wrappers, nested invocations, source locations, and established execution context |
| File effects | Supported create, overwrite, append, delete, rename, and copy operations, with resolved target paths or an explicit unresolved target |
| Script-file invocations | Calls to an existing script file whose contents were not supplied to the analyzer |
| Uncertainties | Unsupported executable constructs, dynamic effects, unresolved values or context, parse failures, and exhausted analysis limits |

Uncertainty belongs to the affected operation or executable region. Discovering one unknown does not discard known effects elsewhere in the command. Consumers receive the complete result before deciding whether execution is allowed.

Source locations identify the original command or the embedded source that produced a finding. Decoded strings retain their enclosing invocation location so diagnostics do not claim an incorrect position in the outer command.

Wrappers remain visible alongside their inner invocations. A rule targeting `uv` can match the wrapper while another consumer analyzes the Python script it launches. Normalization must retain options that change execution context rather than blindly deleting every flag.

## Runtime input and execution context

The detector uses the incoming hook payload and explicit devkit configuration. Collector-added environment hints, transcript contents, result events, and metadata scraped from another source do not become runtime inputs.

Preserve the supplied session, agent, call identity, command, shell, hook directory, and requested working directory as separate fields. Missing fields remain missing. The adapter must not invent a session identity for automatic claims.

A hook directory is suitable for finding configuration, but it is not automatically proof of the command's effective working directory. Relative write targets resolve only against an established execution directory, such as an explicit requested workdir or a statically determined directory change. An absolute target can be resolved without that directory. Structured edit tools retain their existing documented path-resolution behavior.

A tool named `Bash` does not establish the shell dialect. Explicit supported shell metadata selects the parser. A new `[harness] shell` setting accepts `auto`, `bash`, or `powershell`; its default is `auto`. Explicit payload shell metadata takes precedence over this fallback. An explicitly unsupported shell is not silently reinterpreted as Bash.

In `auto`, analyze plausible Bash and PowerShell interpretations and retain only conclusions established across them. A parser returning a tree, or one parser succeeding while another fails, does not by itself prove which shell runs. Disagreement or an unsupported interpretation produces uncertainty where it affects a command decision or write target. An explicit interpreter invocation establishes the dialect of its embedded source even when the outer command requires separate analysis.

Path analysis respects the selected shell and path form. It does not use the analyzer host's path rules to reinterpret Windows source as Unix source. Filesystem canonicalization and lock-key resolution remain in the existing lock facade.

## Parsing and executable source

Use the Tree-sitter runtime and language grammars through their Rust bindings, compiled into the binary. Python and Node installations are not runtime requirements for analysis.

The initial grammar choices are `tree-sitter-bash`, `tree-sitter-pwsh`, `tree-sitter-python`, `tree-sitter-javascript`, and `tree-sitter-typescript`, including its TSX entry point where the source context calls for it. Pin compatible releases in `Cargo.lock`; builds on the supported platforms establish compatibility. Source research identified wharflab's PowerShell grammar as the initial candidate, with known recovery gaps that require explicit uncertainty handling.

Language adapters use syntax nodes and fields to identify executable structure. They do not scan arbitrary strings for command names or file APIs. Tree-sitter `ERROR` and `MISSING` nodes, parser cancellation, and unsupported executable constructs are not successful analysis.

Extract embedded source only from a recognized execution form. V1 covers shell command arguments, interpreter evaluation arguments, stdin/heredoc scripts, PowerShell here-string execution forms, and recognized runner wrappers. Model the wrapper's argument grammar before identifying its script argument.

Ordinary strings and heredoc data remain data. Shell substitutions inside expandable strings or heredocs are executable and must be inspected separately. If shell expansion can change the embedded source and v1 cannot resolve it, report uncertainty rather than parsing the unexpanded text as the program that will run.

Nested interpreters recurse through the same bounded analysis interface. A constant subprocess argument vector can feed command analysis directly. A constant shell command passed to a process API feeds the appropriate shell parser. Dynamic command construction remains unknown.

## Supported effect analysis

### Shell and PowerShell

Preserve command chains, pipelines, redirects, substitutions, and nested script execution. Track simple literal assignments and statically established directory changes in execution order. A branch or scope that makes a binding ambiguous invalidates that binding rather than choosing one value.

Recognize direct file redirects and the supported forms of common in-place editors, file-management commands, and formatters. A copy locks its destination; a rename or move locks both the removed source and the destination. Unsupported options that can change the target set make the operation unresolved.

PowerShell analysis recognizes supported content/file cmdlets and `System.IO.File`/`System.IO.Directory` operations. Track simple variables, literal paths, and supported path joins. Dynamic providers, splatting, wildcard expansion, computed invocation, and aliases without an established binding remain unresolved when they affect a write or command decision.

Redirections are evaluated independently of command-rule exemptions. For example, allowing `devrun task check` does not exempt the target in `devrun task check > shared.txt`.

### Python

Recognize supported `pathlib`, built-in file opening, `os`, and `shutil` operations. Handle literal write modes, simple assignments, imported aliases, path composition, and interpolation whose inputs are known constants.

Receiver names alone do not establish types. Rebinding an imported module, constructor, file handle, or path value invalidates the associated knowledge. Unknown modes, computed callees, dynamic imports, and unmodeled calls that may write produce uncertainty.

Function bodies are not treated as executed merely because they are defined. Calls through unmodeled functions remain unknown. Analyze definition-time executable expressions where applicable; do not overlook effects in defaults or decorators.

### JavaScript and TypeScript

Recognize supported filesystem operations from Node, Bun, and Deno, including synchronous and asynchronous forms. Track supported imports, `require` bindings, destructured aliases, simple constants, path joins, and constant template strings. TypeScript syntax is parsed as syntax, not removed with text substitutions.

Shadowing or reassignment invalidates an API binding. A method named `writeFile` is not sufficient evidence of a filesystem write. Recognized read-only operations, package-resolution checks, and file-read assertions must remain useful negative fixtures.

Computed properties, dynamic imports, unknown functions, and dynamically constructed process commands preserve uncertainty. Syntax support does not imply whole-program type inference or evaluation of arbitrary functions.

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
script_files = "allow"

[harness.commands.worktree-add]
enabled = true
programs = ["git"]
args = ["worktree", "add"]
action = "block"
severity = "error"
reason = "Use devkit's issue command to create worktrees."
```

`unresolved_writes` and `script_files` use the same action vocabulary as command rules:

| Action | Behavior |
|---|---|
| `block` | Deny execution with an actionable diagnostic |
| `warn` | Allow execution and return an agent-facing diagnostic |
| `allow` | Allow execution silently for this policy finding |

`unresolved_writes` defaults to `block`. In this mode an agent must rewrite an unresolved ad-hoc edit until the relevant effects and targets can be determined. Holding an unrelated lock or declaring intended targets does not bypass the result. There is no declaration-based retry mechanism in v1.

`script_files` defaults to `allow`. It governs recognized invocations of stored script files without reading those files or automatically claiming them. Choosing `warn` reports that the script's effects were not analyzed. Choosing `block` requires expressing the work through a supported invocation. An outer redirect or another independently recognized write still goes through normal write enforcement under every setting.

Known write targets always use ordinary lock enforcement when `enforce_writes` is enabled, including when unresolved effects are allowed or warned about. A known ownership conflict cannot be overridden by `allow` on an unrelated finding.

Custom rules gain `enabled`, `action`, and `severity`. Their defaults preserve existing behavior: enabled, blocking, with error severity. Severity accepts `info`, `warning`, and `error` and classifies the diagnostic; action controls execution. `reason` remains the canonical message field.

`programs` matches a parsed invocation's executable basename, and `args` matches its known leading arguments after supported semantic normalization. Program-specific normalization handles forms such as `git -C /repo worktree add` while retaining execution context. Rules see wrappers and nested executable commands, but not quoted data that only mentions them.

Raw regex matching is deferred from v1. The existing program/argument matcher covers the concrete requested examples. This is an implementation-scope choice; a later matcher must specify the parsed fields it matches and preserve argument boundaries.

## Inheritance and precedence

Use existing devkit table merging. Global rules are inherited by project layers, and a child can override a named rule's individual fields. `enabled = false` explicitly disables an inherited rule; legacy `programs = []` continues to match nothing.

`allow` suppresses denial from its own matching rule. It is not a global exemption from other rules, task guards, or write enforcement. Any applicable blocking result prevents the complete tool call from executing. Warnings are emitted in stable source/rule order, and parallel completion order must not choose the diagnostic.

Task and application guard semantics remain in their existing consumer: task signatures, catalog precedence, app selection, and the replacement command are retained. All consume the shared invocation representation. A task guard override affects its own routing decision rather than surrounding file effects.

Parse configuration keys independently. A malformed command rule cannot disable `enforce_writes`. Invalid write-policy values produce a diagnostic and retain the conservative blocking default; malformed command rules preserve their existing fail-open treatment. Update the schema and configuration documentation alongside the types and resolver.

## Hook execution and locks

The shell hook performs these stages:

1. Read and adapt the payload, retaining identities and context without filling missing runtime facts from collector metadata.
2. Resolve activation and applicable policy. An inactive path skips command analysis and pool creation.
3. Analyze the command once and share the result with command guarding and write enforcement.
4. Evaluate blocking and warning policies in deterministic order.
5. If execution remains allowed, resolve every known write target through the existing lock facade and obtain the necessary claims before returning permission to run.
6. Emit the harness's deny or warning envelope, or allow silently.

Claims use the same session/subagent identity, ownership ancestry, TTL, daemon routing, and release lifecycle as structured edits. Missing identity when claims are needed, registry failures, or ownership conflicts deny the write. No shell-hook code writes the registry behind a live daemon.

Use the existing multi-target operations where targets share a registry scope. Cross-project targets do not introduce a new distributed transaction protocol: execution is denied if any claim fails, and any claims already acquired remain subject to the normal release lifecycle. All required successful claims precede execution.

The shell hook does not execute the captured command. It does not require result events to release locks or establish success. Post-tool outcomes and transcript audit results remain offline evidence only.

Update registrations for supported shell tool names on Claude Code, Codex, and Cursor, including native Windows command variants. Keep structured-edit and session-release registrations intact.

## Failure handling

Expected uncertainty about writes, including unsupported syntax and exhausted bounds, follows `unresolved_writes`; stored script invocations follow `script_files`. A recognized write with an unresolved path is never reported as protected.

When uncertainty prevents establishing whether a command rule, task signature, or catalog restriction matches, warn and allow for that possible match. Definite matches still apply. For example, `git "$verb" add /tmp/wt` does not establish a match against a `worktree add` rule. This preserves command guarding's fail-open behavior; independently identified write uncertainty still follows the write policy when enabled.

Internal failures are distinct from expected uncertainty. When write enforcement is active, an unusable write payload, internal analysis failure, or failed registry operation denies with a clear reason. The command-only guard retains its fail-open behavior and emits diagnostics without mutating registries.

Diagnostics name the affected operation or source region and explain a usable correction. A blocking unresolved-write diagnostic asks for simpler explicit targets or a supported structured edit. It does not suggest an unrelated lock claim as proof of coverage.

## Performance and parallel execution

Parse each source once and reuse the result across consumers. Keep literal argument vectors structured, avoid repeated config resolution within the hook, and avoid copying entire source strings per finding. Instantiate language parsers only for executable source that needs them.

Bound source size, cumulative decoded source, parser nesting, visited syntax nodes, and constant-value growth. The initial implementation uses a 256 KiB outer-source limit, a 1 MiB cumulative source limit, nesting depth 8, 100,000 visited nodes, and a 64 KiB limit on a resolved value. These are implementation limits, not performance measurements. Exhaustion produces uncertainty without discarding findings already established.

Small calls remain serial. The initial parallel condition requires at least two independent embedded-source jobs whose combined source size is at least 8 KiB. A large single script is not split across dependent statements merely to use more threads. The threshold is provisional and unbenchmarked.

Only the caller uses `devkit_common::pool`; the analyzer does not create a local or global Rayon pool. Pool width must be settled from the supported environment/configuration precedence before initialization. If the caller has not needed to settle that configuration, it can retain the serial path. V1 does not eagerly start a pool on every CLI launch.

Independent jobs can parse and analyze concurrently with separate parser state. Variable tracking and working-directory changes remain ordered within each executable scope. Merge results in source order. Thread-creation failure must take an explicitly serial path without accidentally invoking Rayon's global pool.

The discussed target is no more than 2 ms p95 of added analysis for commands up to 8 KiB on WSL and native Windows. It remains an unvalidated target. Benchmarking and timing-based acceptance tests are deferred; correctness checks and deterministic serial/parallel equivalence tests remain required.

## Validation

Use the real shell-hook entry point with isolated temporary projects and registries. Required cases include:

- A free target is claimed; another session's claim denies; same-session and permitted ancestor ownership allow.
- Multiple targets, rename endpoints, outer redirects around permitted devkit commands, and registry failures.
- `block`, `warn`, and `allow` for unresolved writes and script-file invocations; a warning or allow finding cannot override a known lock conflict.
- Existing rule compatibility, inheritance, explicit disabling, per-rule actions and severity, uncertain command matching, and isolation of malformed rules from write enforcement.
- Shell wrappers, argument quoting, pipelines, substitutions, ordinary versus executable heredocs, and explicit versus missing execution context.
- Python path construction, aliases, rebinding, unknown open modes, and constant versus dynamic subprocess input.
- PowerShell here-strings, cmdlets, file APIs, path expressions, parser recovery, and ambiguous shell interpretation.
- JavaScript/TypeScript imports, aliases, shadowing, reads, writes, asynchronous calls, and embedded process commands.
- Quoted command text, unused function bodies, and readonly examples that must not acquire locks or trigger unrelated command rules.
- Input/nesting/work bounds and equivalent results from serial and parallel execution.

Turn representative collected hook calls into minimized fixtures that preserve syntax and payload shape while removing private paths and unrelated contents. Do not execute captured commands. Use the existing `run_analysis.py` entry point under `data_analysis.local` in the primary checkout to regenerate corpus measurements; its successful subset is not a precision or recall result.

Run the configured `fmt-check`, `lint`, `test`, and `test-doc` tasks, or their `verify` sequence, in the implementation worktree. Formatting uses the configured nightly formatter. Regenerate the committed JSON schema and verify its consistency test. Cross-platform CI remains the build and behavior gate for supported operating systems.

## Documentation and implementation boundaries

Update `docs/configuration.md`, `docs/commands.md`, the agent skill guidance, hook manifests, schema, and the command-guard invariant in `AGENTS.md` to reflect the new write-enforcement stage. Keep existing public command names and structured-edit behavior compatible.

The implementation plan must separate the pure analysis contract, language adapters, shared scheduling, guard/config integration, and lock-hook integration into reviewable tasks with focused failing tests. Each task must consume the same result model rather than introducing another parser or policy vocabulary.

## Review decisions

The user approved the separate crate, Tree-sitter, the language scope, configurable unresolved-write handling, and conditional use of the existing Rayon pool, and explicitly deferred benchmarking.

The `shell` fallback setting, `script_files` key, structured-only v1 rule matcher, concrete analysis limits, and initial parallel threshold are implementation choices made to complete this spec. Their behavior is specified above so they can be reviewed before code is written. No additional user answer is required to interpret the document.
