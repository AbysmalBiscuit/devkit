//! What this checkout pins, per registered library.
//!
//! Filesystem reads only: the merged docs manifest plus whatever lockfile the
//! importer graph consults. No clone, no fetch, no worktree, no cache lock —
//! so a session-start hook can call this on a cold machine.

use crate::importers::{Evidence, Inspection, Selector, Undeclared};
use crate::manifest::{self, Ecosystem, LibEntry};
use crate::names;
use anyhow::Result;
use std::collections::{BTreeMap, HashMap};
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
    /// Members of this lockfile declare the library; the directory in hand —
    /// a workspace container — does not. One entry per distinct version, each
    /// with the members that resolve it, both sorted.
    Rollup {
        versions: Vec<(String, Vec<String>)>,
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
    /// The checkout this project last resolved for the library, from the
    /// reference registry rather than from any lockfile. Evidence the project
    /// uses the library, and — where it disagrees with the lockfile — the
    /// version an agent reading that checkout would actually see.
    pub resolved: Option<String>,
}

/// Every registered library's pin for the checkout at `start`, alphabetical.
///
/// `Err` means the registrations could not be enumerated at all — a caller
/// must report that rather than print an empty listing. A single library
/// failing to resolve is data, and lands in that row's `Outcome`.
pub fn pins(start: &Path, global: Option<&Path>) -> Result<Vec<Pin>> {
    pins_with_cache(start, global, None)
}

/// `pins` against a named cache root, for tests and for a caller that already
/// resolved one. `None` reads the machine's own.
pub fn pins_with_cache(
    start: &Path,
    global: Option<&Path>,
    cache_root: Option<&Path>,
) -> Result<Vec<Pin>> {
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

    let resolved = resolved_here(start, &selectors, cache_root);

    let mut out: Vec<Pin> = discovered
        .manifest
        .libs
        .iter()
        .map(|entry| {
            let mut pin = pin_for(entry, &selectors, &global_path, project_root.as_deref());
            pin.resolved = resolved.get(&pin.name).cloned();
            pin
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// The checkout each library was last resolved to *for this project*, read from
/// the reference registry. Keyed by library name.
///
/// Rows are matched on the project root in hand and on the member workspaces
/// its lockfile names — never on an arbitrary descendant. Worktrees sit beside
/// each other under a shared parent, and a prefix match run from that parent
/// would report another branch's versions as this checkout's.
///
/// Fail-soft and lock-free: an unreadable registry reads as no rows, because a
/// session-start summary must not fail on the cache's state.
fn resolved_here(
    start: &Path,
    selectors: &HashMap<Ecosystem, Result<Selector, String>>,
    cache_root: Option<&Path>,
) -> HashMap<String, String> {
    let cache_root = cache_root
        .map(Path::to_path_buf)
        .unwrap_or_else(crate::cache::docs_root);
    let mut keys: Vec<PathBuf> = vec![start.to_path_buf()];
    for selector in selectors.values().filter_map(|s| s.as_ref().ok()) {
        keys.extend(selector.member_dirs());
    }

    crate::refs::RefStore::at(&cache_root)
        .snapshot()
        .rows
        .into_iter()
        .filter(|row| keys.iter().any(|key| Path::new(&row.project) == key))
        // A row carries the checkout *directory*, where a ref's `/` is encoded
        // as `~`. Decoding restores the ref, which is both what a reader
        // recognises and what a comparison against a pin can be made against.
        .map(|row| (row.lib, names::decode(&row.version)))
        .collect()
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
            // Undiagnosed: this readout renders neither the `undeclared`
            // enumeration nor `Selection::source`, and collecting them costs a
            // traversal of the whole lockfile for every registered library —
            // including the ones the relevance filter drops.
            Ok(selector) => selector.inspect_undiagnosed(&package),
            Err(reason) => Inspection {
                evidence: Evidence::Unknown,
                result: Err(anyhow::anyhow!(reason.clone())),
            },
        });
    let mut declared = inspection
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
                // A workspace container declares nothing of its own, so the
                // undeclared answer is technically right and practically
                // useless: the versions its members resolve are what a session
                // started here needs.
                Err(error) if error.downcast_ref::<Undeclared>().is_some() => {
                    match rollup(entry, selectors, &package) {
                        Some(outcome) => {
                            declared = Evidence::Declared;
                            outcome
                        }
                        None => Outcome::Undeclared,
                    }
                }
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
        resolved: None,
    }
}

/// What the members of this entry's lockfile resolve `package` to, grouped by
/// version. `None` when the directory in hand is not a lockfile root, when the
/// lockfile names no other members, or when none of them declares the package.
fn rollup(
    entry: &LibEntry,
    selectors: &HashMap<Ecosystem, Result<Selector, String>>,
    package: &str,
) -> Option<Outcome> {
    let selector = entry
        .ecosystem
        .and_then(|ecosystem| selectors.get(&ecosystem))
        .and_then(|selector| selector.as_ref().ok())
        .filter(|selector| selector.at_lock_root())?;

    let mut by_version: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut lockfile = String::new();
    for pin in selector.rollup(package) {
        lockfile = pin.lockfile;
        by_version
            .entry(pin.version)
            .or_default()
            .push(pin.workspace);
    }
    if by_version.is_empty() {
        return None;
    }
    Some(Outcome::Rollup {
        versions: by_version
            .into_iter()
            .map(|(version, mut workspaces)| {
                workspaces.sort();
                (version, workspaces)
            })
            .collect(),
        lockfile,
    })
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
/// `undeclared` is a checked answer, `unknown` means no check ran.
///
/// `unknown` has both a benign and a diagnostic contributor, and the count
/// alone cannot separate them. A git-ecosystem or ref-only registration has no
/// importer to ask, so it lands here permanently and by design — a machine-wide
/// `docm add <git-url> --ref <tag>` shows up in every checkout on the machine.
/// A registration whose ecosystem does have an importer lands here only when
/// that importer could not run: no manifest, or a lockfile that would not parse.
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
        if pin.project_scoped || pin.resolved.is_some() {
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
    let rows: Vec<[String; 3]> = relevant_pins.iter().flat_map(|pin| rows_for(pin)).collect();

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

/// The table rows one pin contributes. A roll-up contributes one row per
/// version its members resolve: where they disagree, an agent needs to see
/// both and which workspaces hold them, not a single version picked for it.
fn rows_for(pin: &Pin) -> Vec<[String; 3]> {
    match &pin.outcome {
        Outcome::Rollup { versions, lockfile } => {
            // Decorate only when the resolved checkout is none of the versions
            // the members hold: naming it beside the very version it matches
            // reads as a disagreement that is not there.
            let stale = pin
                .resolved
                .as_deref()
                .filter(|resolved| versions.iter().all(|(version, _)| version != resolved));
            versions
                .iter()
                .map(|(version, workspaces)| {
                    [
                        cell(&pin.name),
                        cell(version),
                        cell(&decorate(
                            &format!("{lockfile} ({})", members(workspaces)),
                            stale,
                        )),
                    ]
                })
                .collect()
        }
        // The registry is the only evidence there is: it becomes the row.
        Outcome::Undeclared | Outcome::Unresolved(_) if pin.resolved.is_some() => {
            let version = pin.resolved.clone().unwrap_or_default();
            vec![[cell(&pin.name), cell(&version), cell("resolved checkout")]]
        }
        _ => {
            let mut row = row(pin);
            let stale = pin.resolved.as_deref().filter(|r| *r != row[1]);
            row[2] = cell(&decorate(&row[2], stale));
            vec![row]
        }
    }
}

/// Name the resolved checkout beside a lockfile-derived version when the two
/// disagree — that gap is the case where an agent reads one version and the
/// project builds another.
fn decorate(source: &str, stale: Option<&str>) -> String {
    match stale {
        Some(resolved) => format!("{source}; checkout {resolved}"),
        None => source.to_string(),
    }
}

/// Workspaces named for a source cell, bounded so one widely-used library
/// cannot spend the whole section budget on directory names.
fn members(workspaces: &[String]) -> String {
    const NAMED: usize = 3;
    let named = workspaces
        .iter()
        .take(NAMED)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    match workspaces.len().saturating_sub(NAMED) {
        0 => named,
        rest => format!("{named}, +{rest}"),
    }
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
        // Rendered by `rows_for`, which expands it across versions.
        Outcome::Rollup { versions, lockfile } => match versions.first() {
            Some((version, workspaces)) => (
                version.clone(),
                format!("{lockfile} ({})", members(workspaces)),
            ),
            None => ("—".to_string(), lockfile.clone()),
        },
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
                "declared": pin.declared.as_str(),
                "resolved": pin.resolved,
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
        Outcome::Rollup { versions, lockfile } => serde_json::json!({
            "kind": "rollup",
            "lockfile": lockfile,
            "versions": versions
                .iter()
                .map(|(version, workspaces)| serde_json::json!({
                    "version": version,
                    "workspaces": workspaces,
                }))
                .collect::<Vec<_>>(),
        }),
        Outcome::Ref(git_ref) => serde_json::json!({ "kind": "ref", "ref": git_ref }),
        Outcome::Unresolved(reason) => {
            serde_json::json!({ "kind": "unresolved", "reason": reason })
        }
        Outcome::Undeclared => serde_json::json!({ "kind": "undeclared" }),
    }
}
