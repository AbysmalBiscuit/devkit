//! The JSON Schema for `devkit.toml`, derived from the same types serde reads
//! so the two cannot drift. Editors that speak the TOML language server
//! (taplo) use it for completion, hover docs and validation; see
//! `docs/configuration.md`.
//!
//! This lives in the `devkit` binary rather than a library crate because it is
//! the only unit that sees every crate owning a table: `[defaults]`/`[apps]`/…
//! come from `devkit-ports`, `[docs]` from `devkit-docs`, `[harness]` from
//! `devkit-locks`. Those three are read straight off the raw TOML by their own
//! crates rather than through `Config`, so they are spliced on here — a
//! document type with a flattened `Config` keeps `Config`'s field list in one
//! place while still describing the whole file.

use anyhow::Result;
use schemars::JsonSchema;

/// The published id. The release workflow attaches this file to every GitHub
/// Release, so `releases/latest/download` always resolves to the newest
/// released schema — and, unlike a URL naming one tag, never needs editing when
/// a release ships. A user wanting to validate against the version they run
/// substitutes their tag: `releases/download/v1.2.3/devkit-config.json`. Both
/// beat pointing at `main`, which would validate every config against
/// unreleased keys.
const ID: &str =
    "https://github.com/AbysmalBiscuit/devkit/releases/latest/download/devkit-config.json";

/// Every table a `devkit.toml` (or `~/.config/devkit/config.toml`) may carry.
/// Never constructed — it exists to be reflected over.
#[derive(JsonSchema)]
#[schemars(rename = "DevkitConfig")]
#[allow(dead_code)]
struct Document {
    /// `[defaults]`, `[apps]`, `[people]`, `[daemon]`, `[linear]`,
    /// `[templates]`, `[tasks]`, `[brief]`.
    #[serde(flatten)]
    core: devkit_ports::config::Config,
    // `#[serde(default)]` rather than `Option`, so each table is described by
    // its own schema directly. An `Option` would render as `anyOf [T, null]`,
    // and TOML has no null for the second branch to ever match.
    /// Layer-stack control: `root = true` stops the upward walk here.
    #[serde(default)]
    config: devkit_ports::config::LayerMarker,
    /// Libraries `docm` resolves version-correct checkouts for.
    #[serde(default)]
    docs: devkit_docs::manifest::DocsManifest,
    /// Write-enforcement opt-in for this checkout.
    #[serde(default)]
    harness: devkit_locks::hook::HarnessSection,
}

/// The schema document, pretty-printed with a trailing newline.
pub fn document() -> Result<String> {
    let mut schema = serde_json::to_value(schemars::schema_for!(Document))?;
    let obj = schema
        .as_object_mut()
        .expect("a struct schema is a JSON object");
    obj.insert("$id".into(), ID.into());
    obj.insert("title".into(), "devkit.toml".into());
    obj.insert(
        "description".into(),
        "Configuration for the devkit CLIs (devrun, portm, issue, lockm, docm, devkit).".into(),
    );

    // `Config` requires `[defaults]`, but that holds for the *merged* config,
    // not for any one file. A layer legitimately carries a subset — this
    // repository's own devkit.toml is `[harness]` and nothing else, and a
    // `[docs]`-only overlay is the documented way to register a library for
    // one project. An editor validates the file in front of it, so a top-level
    // requirement here would mark those correct files as errors.
    obj.remove("required");

    Ok(format!("{}\n", serde_json::to_string_pretty(&schema)?))
}

pub fn run() -> Result<()> {
    print!("{}", document()?);
    Ok(())
}
