//! What this checkout pins, per registered library.
//!
//! Filesystem reads only: the merged docs manifest plus whatever lockfile the
//! importer graph consults. No clone, no fetch, no worktree, no cache lock —
//! so a session-start hook can call this on a cold machine.

use crate::importers::{self, Evidence, Undeclared};
use crate::manifest::{self, Ecosystem, LibEntry};
use crate::names;
use anyhow::Result;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The importer graph named a version. `workspace` is the directory whose
    /// manifest selected it, relative to the project root where it is under
    /// one; `lockfile` is the file that carried it.
    Version {
        version: String,
        workspace: PathBuf,
        lockfile: String,
    },
    /// A manual `ref` pin in the manifest. No lockfile is consulted.
    Ref(String),
    /// Nothing this readout can state. One line, already short enough to render.
    Unresolved(String),
    /// This workspace does not depend on the library.
    Undeclared,
}

#[derive(Debug, Clone)]
pub struct Pin {
    pub name: String,
    pub outcome: Outcome,
    /// Declared by a project's own `devkit.toml`, not the machine-wide
    /// catalog — evidence this library belongs to the checkout in hand.
    pub project_scoped: bool,
    /// What the importer graph can say about this workspace depending on the
    /// package. Computed separately from `outcome` because a `ref` pin
    /// short-circuits resolution and would otherwise carry no evidence in
    /// either direction.
    pub declared: Evidence,
}

/// Every registered library's pin for the checkout at `start`, alphabetical.
///
/// `Err` means the registrations could not be enumerated at all — a caller
/// must report that rather than print an empty listing. A single library
/// failing to resolve is data, and lands in that row's `Outcome`.
pub fn pins(start: &Path, global: Option<&Path>) -> Result<Vec<Pin>> {
    let discovered = manifest::discover(start, global)?;
    let global_path = global
        .map(Path::to_path_buf)
        .unwrap_or_else(manifest::global_docs_path);
    let project_root = discovered
        .project_devkit_toml
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf);

    let mut out: Vec<Pin> = discovered
        .manifest
        .libs
        .iter()
        .map(|entry| pin_for(start, entry, &global_path, project_root.as_deref()))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn pin_for(start: &Path, entry: &LibEntry, global_path: &Path, project_root: Option<&Path>) -> Pin {
    let project_scoped = entry.origin_file.as_deref() != Some(global_path);
    let package = entry.package_name();
    let inspection = entry
        .ecosystem
        .map(|ecosystem| importers::inspect(start, ecosystem, &package));
    let declared = inspection
        .as_ref()
        .map(|i| i.evidence)
        .unwrap_or(Evidence::Unknown);

    let outcome = match entry.r#ref.as_deref() {
        // A ref wins over lockfile resolution, exactly as `resolve` orders it.
        // Validate it here: `discover` checks library names but not refs, and
        // resolution later rejects an invalid ref through `names::checkout_dir`
        // — so without this the table could state a pin `docm info` refuses.
        Some(pin) => match names::validate_ref(pin) {
            Ok(()) => Outcome::Ref(pin.to_string()),
            Err(error) => Outcome::Unresolved(format!("{error}")),
        },
        None => match (entry.ecosystem, inspection) {
            (None, _) => {
                Outcome::Unresolved("no ecosystem and no ref; add one with `docm add`".to_string())
            }
            (Some(Ecosystem::Git), _) => {
                Outcome::Unresolved("git entry with no ref pinned".to_string())
            }
            (Some(_), Some(inspection)) => match inspection.result {
                Ok(selection) => Outcome::Version {
                    version: selection.version,
                    workspace: relative_workspace(&selection.workspace, project_root),
                    lockfile: selection.lockfile,
                },
                Err(error) if error.downcast_ref::<Undeclared>().is_some() => Outcome::Undeclared,
                // Top-level message only: the `undeclared` diagnostic is three
                // lines, and that belongs in `docm info`, not injected context.
                Err(error) => Outcome::Unresolved(format!("{error}")),
            },
            (Some(_), None) => Outcome::Unresolved("no ecosystem resolved".to_string()),
        },
    };

    Pin {
        name: entry.name.clone(),
        outcome,
        project_scoped,
        declared,
    }
}

/// A workspace named the way a reader of this project sees it. Absolute paths
/// are noise in a table injected into a session that already knows its root.
fn relative_workspace(workspace: &Path, project_root: Option<&Path>) -> PathBuf {
    match project_root.and_then(|root| workspace.strip_prefix(root).ok()) {
        Some(relative) if relative.as_os_str().is_empty() => PathBuf::from("."),
        Some(relative) => relative.to_path_buf(),
        None => workspace.to_path_buf(),
    }
}
