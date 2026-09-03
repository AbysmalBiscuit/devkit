//! The JSON Schema for `devkit.toml`, derived from the same types serde reads
//! so the two cannot drift. Editors that speak the TOML language server
//! (taplo) use it for completion, hover docs and validation; see
//! `docs/configuration.md`.
//!
//! This lives in the `devkit` binary rather than a library crate because it is
//! the only unit that sees every crate owning a table: `[defaults]`/`[apps]`/…
//! come from `devkit-ports`, `[docs]` from `devkit-docs`, `[harness]` from
//! `devkit-config`. Those three are read straight off the raw TOML by their own
//! crates rather than through `Config`, so they are spliced on here — a
//! document type with a flattened `Config` keeps `Config`'s field list in one
//! place while still describing the whole file.

use anyhow::{Context, Result};
use schemars::JsonSchema;
use std::path::Path;

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
    /// `[templates]`, `[tasks]`, `[brief]`, `[hooks]`.
    #[serde(flatten)]
    core: devkit_config::Config,
    // `#[serde(default)]` rather than `Option`, so each table is described by
    // its own schema directly. An `Option` would render as `anyOf [T, null]`,
    // and TOML has no null for the second branch to ever match.
    /// Layer-stack control: `root = true` stops the upward walk here.
    #[serde(default)]
    config: devkit_config::LayerMarker,
    /// Libraries `docm` resolves version-correct checkouts for.
    #[serde(default)]
    docs: devkit_docs::manifest::DocsManifest,
    /// Write-enforcement opt-in for this checkout.
    #[serde(default)]
    harness: devkit_config::HarnessSection,
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

/// taplo reads the schema association from a `#:schema` directive, which it
/// honors only as a header — first line, preceded at most by other directives
/// and comments.
fn directive() -> String {
    format!("#:schema {ID}")
}

/// A starter with every setting commented out, so nothing is active until its
/// owner has read it: an uncommented `worktree_root` would have devkit creating
/// worktrees somewhere nobody chose. Commented, the file resolves to nothing
/// and `devkit brief` says exactly that.
///
/// The app carries `base_port`, `path` and `launch` together: an `[apps.x]`
/// missing `base_port` stops the whole merged config from parsing, and one
/// missing `path` is skipped at catalog build with only a stderr note.
const STARTER: &str = r#"
# Uncomment what this project uses and replace the <placeholders>. Until
# [defaults] is uncommented the config does not resolve, and `devkit brief`
# reports that rather than staying quiet.

# [defaults]
# worktree_root = "~/git/<project>-worktrees"
# branch_prefix = "<you>/"
# baseline_ref = "origin/main"
# baseline_dir = "~/git/<project>-worktrees/_baselines"

# An app needs base_port and launch to parse at all, and path to resolve
# without a doppler.yaml to infer it from.
# [apps.web]
# base_port = 9100
# path = "apps/web"
# launch = ["pnpm", "dev", "--port", "{{ port }}"]

# Commands run on a lifecycle event, as argv arrays (no shell). Failures warn
# and are skipped. after_worktree_create runs in a worktree `issue setup` or
# `issue pr checkout` has just created.
# [hooks]
# after_worktree_create = [["zoxide", "add", "{{ worktree }}"]]
"#;

/// Point `path` at the published schema, creating it from `STARTER` when it
/// does not exist. Idempotent: a file that already carries a directive is left
/// exactly as it is, so this is safe to run against a config under review.
pub fn init(path: &Path) -> Result<()> {
    let directive = directive();
    let existing = match std::fs::read_to_string(path) {
        Ok(body) => Some(body),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };

    let body = match existing {
        Some(body) if body.contains("#:schema") => {
            println!("{} already points at a schema", path.display());
            return Ok(());
        }
        Some(body) => format!("{directive}\n{body}"),
        None => format!("{directive}\n{STARTER}"),
    };

    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(path, &body).with_context(|| format!("writing {}", path.display()))?;
    println!("{} now points at {ID}", path.display());
    Ok(())
}
