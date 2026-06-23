# Layered config resolution + `devrun config` command

**Date:** 2026-06-23
**Status:** Design — approved, pending implementation plan.

Two coupled features over the `devkit-ports` config loader:

- **#8 — Hierarchical `devkit.toml` resolution.** Resolve config by layering every
  `devkit.toml` from cwd up to the filesystem root, plus the home config, and
  deep-merging them (deepest wins per value) — the way Claude Code resolves
  `CLAUDE.md` files. Today only the single deepest file is used.
- **#7 — `devrun config` subcommand.** Show the effective merged config (with an
  optional per-value source annotation) and list the configured app catalog.

They share one design because *how* config is resolved dictates *what* `config show`
displays — provenance is the safety mechanism that makes layered merge auditable.

---

## 1. Layered resolution — `devkit-ports::config`

Today `config::locate(explicit, start)` returns a single path (first match walking
up, else the home config as a fallback) and only that file is parsed. This becomes a
two-phase **resolver**.

### 1a. Discovery — ordered layer list

Build a list of config files ordered lowest→highest precedence:

1. `~/.config/devkit/config.toml` (home base layer), if it exists, then
2. every `devkit.toml` found from the filesystem root *down to* `start` (cwd) —
   deeper directories rank higher.

While collecting `devkit.toml` files (walking cwd → root), if a file declares
`[config] root = true`, **stop**: that file is the shallowest layer kept, and all
shallower layers — including the home config — are dropped. This is the opt-in
isolation escape hatch for a repo that wants to ignore inherited config.

**Explicit override bypasses layering.** `--config <path>` (CLI) or `$DEVKIT_CONFIG`
selects exactly that one file, parsed as-is with no walking or merging — preserving
today's hard-override behavior.

### 1b. Merge — deep field-level

Parse each layer to a `toml::Table` and deep-merge in precedence order (lowest first,
each higher layer overlaid on the accumulator) by **one uniform rule**:

> **Tables merge recursively, key by key; every non-table value (scalar or array) is
> replaced wholesale by the deeper layer.**

This single rule yields the per-field behavior we want at every level:

- `defaults` and `daemon` are tables → merge field by field; a field a deeper layer
  omits inherits the shallower value.
- `apps` and `people` are tables → union by key; a shared `apps.<name>` is itself a
  table → its fields merge one level deeper (override one app's `base_port`, inherit
  the rest).
- `apps.<name>.launch` is an array → replaced wholesale (argv is never partially
  merged); `apps.<name>.static_env` / `prep_env` are tables → merge key by key (a
  deeper layer can add or override a single env var).

Merge granularity is therefore **per leaf value**, not per section — defining
`[defaults]` in a deep file overrides only the fields it names.

**`[harness]` is out of scope.** Harness write-enforcement is read by a *separate*
path (`devkit-locks::hook::{harness_enabled, global_harness_enabled}`) with its own
checkout-or-global OR semantics and the `DEVKIT_ENFORCE_WRITES` master switch; it does
not go through this resolver and is unchanged. `[harness]` is not part of `Config`, so
`config show` does not render it.

The merged `toml::Table` is then deserialized into the **existing `Config` structs,
unchanged**. Serde defaults and required-field validation run on the *merged* result,
so required fields may be satisfied across layers (e.g. home supplies `worktree_root`,
a repo supplies `apps`). The resolver returns `(Config, Provenance)`.

### 1c. Provenance

Built during the merge: for each leaf value, record the deepest layer (file path) that
set it. Shape:

```
struct Provenance {
    layers: Vec<PathBuf>,                 // ordered, lowest→highest precedence
    origin: HashMap<String, PathBuf>,     // dotted path -> file that supplied the value
}
```

`origin` keys are dotted paths matching the serialized config tree
(`defaults.worktree_root`, `apps.api.base_port`, …). This is the sole input to
`config show --origin`; two values in the same section can carry different origins.

### 1d. Errors

`anyhow` `.context()` names the offending file when a layer fails to parse. Missing
files are skipped (the home layer is optional). A merged config that fails
required-field validation reports the merged view, noting which layers contributed.

---

## 2. `devrun config` subcommand — `src/bin/devrun/config.rs`

A new `config` subcommand on `devrun` (the most config-heavy consumer). The resolver
lives in `devkit-ports`; `devrun` calls it and formats output.

### 2a. `devrun config show [--origin] [--json]`

Render the effective *resolved* config — the merged result deserialized into `Config`,
so serde defaults (`apps_dir`, `pr_base`, daemon fields, …) appear as their effective
values even when no file set them.

- Default output: **TOML** (round-trippable; `toml::to_string_pretty(&config)` — needs
  `Serialize` on the config structs).
- `--json`: `serde_json::to_string_pretty(&config)` instead.
- `--origin`: a **flattened provenance view** rather than the nested block. Serialize
  the resolved `Config` to a `toml::Value`, flatten it to dotted-path leaves (scalars
  and arrays), and emit one line per leaf in path order:
  - TOML form (default): `defaults.worktree_root = "~/x"  # from /abs/path/devkit.toml`
    — dotted-key assignments are valid TOML, so the output still parses. A value with
    no source layer (pure serde default) gets `# (default)`.
  - JSON form (`--json --origin`): a sidecar object
    `{ "config": { … }, "origins": { "defaults.worktree_root": "<path>", … } }`; defaulted
    values are simply absent from `origins`.

Flattening to dotted keys avoids fragile post-serialization comment injection and keeps
the origin lookup a direct `origin.get(&dotted_path)`.

### 2b. `devrun config apps [--json]`

A pure config readout of the merged app catalog: a table with columns `name`,
`base_port`, resolved `path`, `provides_url`, `url_env`, and `launch`. `--json` emits
the structured list. No filesystem or registry probing — live readiness stays with
`devrun status`.

---

## 3. Testing

- **Layering & precedence:** nested temp dirs with `devkit.toml` at multiple levels;
  assert deep field-level merge and that the deepest value wins per field.
- **`root = true`:** a marker mid-tree drops all shallower layers including home.
- **Explicit override:** `--config` / `$DEVKIT_CONFIG` bypasses layering (single file).
- **Provenance:** a value set only in home vs. only in the repo resolves to the correct
  origin path; an overridden value points at the deeper layer.
- **`config show`:** TOML and JSON rendering; `--origin` annotations in both formats.
- **`config apps`:** catalog listing matches the merged config; `--json` shape.
- **Validation across layers:** required fields satisfied by a combination of layers
  pass; a genuinely missing required field still errors with a useful message.

---

## 4. Behavior change to call out

Today a repo's own `devkit.toml` fully shadows any parent or home config. After this
change, parents and the home config **merge in** beneath the repo's values. A repo that
needs the old total-isolation behavior sets `[config] root = true`.

---

## 5. Out of scope (tracked separately)

- **Configurable per-app prep step** — generalizing the hardcoded `.env.local`
  filename/format/write-if-absent in `issue setup`. Recorded in
  `docs/next-features.md` ("Configurable per-app prep step"); its own brainstorm/spec.
- **Linear/Slack setup UX (#6)** — credential/OAuth setup from the CLI. Separate
  subsystem, separate spec.
