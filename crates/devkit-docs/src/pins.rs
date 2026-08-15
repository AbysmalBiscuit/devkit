//! What this checkout pins, per registered library.
//!
//! Filesystem reads only: the merged docs manifest plus whatever lockfile the
//! importer graph consults. No clone, no fetch, no worktree, no cache lock —
//! so a session-start hook can call this on a cold machine.

use crate::importers::{Evidence, Inspection, Selector, Undeclared};
use crate::manifest::{self, Ecosystem, LibEntry};
use crate::names;
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The importer graph named a version. `workspace` is the directory whose
    /// manifest selected it, relative to the project root when there is one,
    /// else to the lockfile's own directory; `lockfile` is the file that
    /// carried it.
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

    // One selector per ecosystem present, so each lockfile is read and parsed
    // once for the whole listing rather than once per library. A construction
    // failure is per-ecosystem data, not fatal: every library of that
    // ecosystem gets the construction error as its unresolved reason.
    let mut selectors: HashMap<Ecosystem, Result<Selector, String>> = HashMap::new();
    for entry in &discovered.manifest.libs {
        if let Some(ecosystem) = entry.ecosystem {
            selectors
                .entry(ecosystem)
                .or_insert_with(|| Selector::new(start, ecosystem).map_err(|e| format!("{e}")));
        }
    }

    let mut out: Vec<Pin> = discovered
        .manifest
        .libs
        .iter()
        .map(|entry| pin_for(entry, &selectors, &global_path, project_root.as_deref()))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn pin_for(
    entry: &LibEntry,
    selectors: &HashMap<Ecosystem, Result<Selector, String>>,
    global_path: &Path,
    project_root: Option<&Path>,
) -> Pin {
    let project_scoped = entry.origin_file.as_deref() != Some(global_path);
    let package = entry.package_name();
    let inspection = entry
        .ecosystem
        .and_then(|ecosystem| selectors.get(&ecosystem))
        .map(|selector| match selector {
            Ok(selector) => selector.inspect(&package),
            Err(reason) => Inspection {
                evidence: Evidence::Unknown,
                result: Err(anyhow::anyhow!(reason.clone())),
            },
        });
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
        // `selectors` holds an entry for every ecosystem the manifest names,
        // so `inspection` is `Some` exactly when `ecosystem` is; zipping the
        // two collapses them into the states that are actually reachable.
        None => match entry.ecosystem.zip(inspection) {
            None => {
                Outcome::Unresolved("no ecosystem and no ref; add one with `docm add`".to_string())
            }
            Some((Ecosystem::Git, _)) => {
                Outcome::Unresolved("git entry with no ref pinned".to_string())
            }
            Some((_, inspection)) => match inspection.result {
                Ok(selection) => Outcome::Version {
                    version: selection.version,
                    workspace: relative_workspace(
                        &selection.workspace,
                        project_root,
                        &selection.lock_dir,
                    ),
                    lockfile: selection.lockfile,
                },
                Err(error) if error.downcast_ref::<Undeclared>().is_some() => Outcome::Undeclared,
                // Top-level message only: the `undeclared` diagnostic is three
                // lines, and that belongs in `docm info`, not injected context.
                Err(error) => Outcome::Unresolved(format!("{error}")),
            },
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
///
/// Anchored on the project root first, falling back to the lockfile's own
/// directory when there is no project root (a globally-registered library
/// resolved outside any `devkit.toml`) or the workspace sits outside it.
/// `lock_dir` is an ancestor of `workspace` by construction — every importer
/// resolves `workspace` by walking up from it to find the lockfile — so the
/// absolute fallback below is unreachable and kept only as a safety net.
fn relative_workspace(workspace: &Path, project_root: Option<&Path>, lock_dir: &Path) -> PathBuf {
    let relative = project_root
        .and_then(|root| workspace.strip_prefix(root).ok())
        .or_else(|| workspace.strip_prefix(lock_dir).ok());
    match relative {
        Some(relative) if relative.as_os_str().is_empty() => PathBuf::from("."),
        Some(relative) => relative.to_path_buf(),
        None => workspace.to_path_buf(),
    }
}

/// Registrations the filter withheld, split by why. The split matters:
/// `undeclared` is a checked answer, `unknown` means the check could not run,
/// and a project seeing several `unknown` has a configuration problem rather
/// than a short dependency list.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Dropped {
    pub undeclared: usize,
    pub unknown: usize,
}

impl Dropped {
    pub fn total(&self) -> usize {
        self.undeclared + self.unknown
    }
}

/// A project-scoped registration always renders. A machine-wide one renders
/// only when the importer graph confirms this workspace declares the package —
/// read off `declared`, never off `outcome`.
pub fn relevant(pins: &[Pin]) -> (Vec<&Pin>, Dropped) {
    let mut rows = Vec::new();
    let mut dropped = Dropped::default();
    for pin in pins {
        if pin.project_scoped {
            rows.push(pin);
            continue;
        }
        match pin.declared {
            Evidence::Declared => rows.push(pin),
            Evidence::Undeclared => dropped.undeclared += 1,
            Evidence::Unknown => dropped.unknown += 1,
        }
    }
    (rows, dropped)
}

/// `ui::table` bounds line width, not total size: wrapping a 40 KB cell across
/// 100 columns yields 400 lines, not a truncation. These bound bytes.
const CELL_BUDGET: usize = 200;
const SECTION_BUDGET: usize = 4096;
/// Reserved out of `SECTION_BUDGET` when estimating how many rows to try, so
/// the estimate leaves room for the marker line. The final output is
/// measured and shrunk to fit regardless, so this reserve affects only how
/// many shrink iterations that fitting pass needs, never correctness.
const MARKER_RESERVE: usize = 64;

/// Renders the pins table plus the dropped-count footer, fit to
/// `SECTION_BUDGET` bytes. Newline-terminated.
pub fn render(pins: &[Pin]) -> String {
    let (relevant_pins, dropped) = relevant(pins);
    let rows: Vec<[String; 3]> = relevant_pins.iter().map(|pin| row(pin)).collect();

    let mut shown = estimate_fit(&rows);
    let mut out = render_section(&rows, shown, &dropped);
    // `ui::table`'s dynamic arrangement sizes each column to the widest cell
    // across *every* row, so a single near-`CELL_BUDGET` cell can widen (and
    // wrap) every admitted row at once — `estimate_fit` prices each row only
    // against itself and cannot see that. Measure the real rendered output
    // and drop whole rows until it fits; the estimate errs low (it ignores
    // shared-column widening, which only ever adds bytes), so this usually
    // converges in a few steps rather than one per row. At `shown == 0` the
    // loop stops regardless: the marker and footer alone are the floor this
    // function can reach, and that floor is not itself budget-checked.
    while out.len() > SECTION_BUDGET && shown > 0 {
        shown -= 1;
        out = render_section(&rows, shown, &dropped);
    }
    out
}

/// A cheap starting guess for how many rows fit in `SECTION_BUDGET`, pricing
/// each row from its own cell lengths. `render` treats this as a starting
/// point only and re-measures the actual output, so under- or over-shooting
/// here costs iterations, never correctness.
fn estimate_fit(rows: &[[String; 3]]) -> usize {
    // comfy-table pads and separates each of the 3 columns (2 chars padding
    // per column plus a 1-char separator between columns) and joins rows
    // with a newline; this approximates that per-row overhead.
    const ROW_OVERHEAD: usize = 16;
    let mut budget = SECTION_BUDGET.saturating_sub(MARKER_RESERVE);
    let mut shown = 0usize;
    for row in rows {
        let cost = row.iter().map(String::len).sum::<usize>() + ROW_OVERHEAD;
        if cost > budget {
            break;
        }
        budget -= cost;
        shown += 1;
    }
    shown
}

/// Renders `shown` of `rows` as a table, a marker line naming how many were
/// left out, and the dropped-count footer.
fn render_section(rows: &[[String; 3]], shown: usize, dropped: &Dropped) -> String {
    let mut out = String::new();
    if rows.is_empty() {
        out.push_str("no registered libraries are evidenced in this checkout\n");
    } else {
        let mut table = devkit_common::ui::table(&["LIBRARY", "VERSION", "SOURCE"]);
        for row in rows.iter().take(shown) {
            table.add_row(row.to_vec());
        }
        out.push_str(&format!("{table}\n"));
        // A marker line, not a table row: folding it into the table would let
        // its own width widen every column, which can itself blow the
        // section budget it exists to protect.
        if shown < rows.len() {
            out.push_str(&format!(
                "… {} more (see `docm list --project`)\n",
                rows.len() - shown
            ));
        }
    }
    if let Some(footer) = footer(dropped) {
        out.push_str(&footer);
        out.push('\n');
    }
    out
}

fn row(pin: &Pin) -> [String; 3] {
    let (version, source) = match &pin.outcome {
        Outcome::Version {
            version,
            workspace,
            lockfile,
        } => (
            version.clone(),
            format!("{lockfile} ({})", workspace.display()),
        ),
        Outcome::Ref(git_ref) => (git_ref.clone(), "ref".to_string()),
        Outcome::Undeclared => (
            "—".to_string(),
            "not declared by this workspace".to_string(),
        ),
        Outcome::Unresolved(reason) => ("—".to_string(), reason.clone()),
    };
    [cell(&pin.name), cell(&version), cell(&source)]
}

/// A filtered listing must not read as an empty catalog: `skills/docs/SKILL.md`
/// tells an agent that a library absent from the listing is unregistered, and
/// against this view that inference is false.
fn footer(dropped: &Dropped) -> Option<String> {
    let total = dropped.total();
    if total == 0 {
        return None;
    }
    let mut parts = Vec::new();
    if dropped.undeclared > 0 {
        parts.push(format!("{} undeclared", dropped.undeclared));
    }
    if dropped.unknown > 0 {
        parts.push(format!("{} unknown", dropped.unknown));
    }
    let noun = if total == 1 { "library" } else { "libraries" };
    Some(format!(
        "{total} registered {noun} not evidenced here ({}) — see `docm list`",
        parts.join(", ")
    ))
}

/// Sanitize then bound one cell. Values come from checked-in manifests and
/// land in agent context; control and bidi characters are the hazard being
/// closed, and the reason text is lockfile-derived so it gets the same
/// treatment as names, versions and refs.
fn cell(value: &str) -> String {
    let mut clean = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\n' | '\r' | '\t' => clean.push(' '),
            '\u{061c}' | '\u{200e}' | '\u{200f}' => {}
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => {}
            c if c.is_control() => {}
            c => clean.push(c),
        }
    }
    if clean.len() <= CELL_BUDGET {
        return clean;
    }
    let mut end = CELL_BUDGET - '…'.len_utf8();
    while !clean.is_char_boundary(end) {
        end -= 1;
    }
    clean.truncate(end);
    clean.push('…');
    clean
}

/// The `--project --json` envelope. An array cannot distinguish an empty
/// catalog from a catalog whose every entry went unevidenced, and those call
/// for opposite responses: register something, versus find out why the check
/// could not run. Values here are untruncated — truncation is a property of
/// the context-injection rendering, not of the data.
pub fn envelope(pins: &[Pin]) -> serde_json::Value {
    let (relevant_pins, dropped) = relevant(pins);
    let rows: Vec<serde_json::Value> = relevant_pins
        .iter()
        .map(|pin| {
            serde_json::json!({
                "name": pin.name,
                "project_scoped": pin.project_scoped,
                "declared": match pin.declared {
                    Evidence::Declared => "declared",
                    Evidence::Undeclared => "undeclared",
                    Evidence::Unknown => "unknown",
                },
                "outcome": outcome_json(&pin.outcome),
            })
        })
        .collect();
    serde_json::json!({
        "pins": rows,
        "dropped": {
            "undeclared": dropped.undeclared,
            "unknown": dropped.unknown,
        },
    })
}

fn outcome_json(outcome: &Outcome) -> serde_json::Value {
    match outcome {
        Outcome::Version {
            version,
            workspace,
            lockfile,
        } => serde_json::json!({
            "kind": "version",
            "version": version,
            "lockfile": lockfile,
            "workspace": workspace.to_string_lossy().replace('\\', "/"),
        }),
        Outcome::Ref(git_ref) => serde_json::json!({ "kind": "ref", "ref": git_ref }),
        Outcome::Unresolved(reason) => {
            serde_json::json!({ "kind": "unresolved", "reason": reason })
        }
        Outcome::Undeclared => serde_json::json!({ "kind": "undeclared" }),
    }
}
