//! Resolve the version installed by one workspace from its lockfile importer graph.

use crate::manifest::Ecosystem;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Map as JsonMap, Value as JsonValue};
use serde_yaml_ng::{Mapping as YamlMap, Value as YamlValue};
use std::cell::{Ref, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A parse failure kept for replay. `anyhow::Error` is neither `Clone` nor
/// itself a `std::error::Error`, so neither it nor an `Arc` of it can be
/// handed out twice; a boxed std error can.
type CachedErr = Arc<dyn std::error::Error + Send + Sync + 'static>;

/// Re-wraps a cached error so `anyhow` can walk its cause chain again, which
/// is what keeps `{:#}` and `Debug` identical across replays.
#[derive(Debug, Clone)]
struct Replay(CachedErr);

impl std::fmt::Display for Replay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for Replay {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

fn cache_err(error: anyhow::Error) -> CachedErr {
    let boxed: Box<dyn std::error::Error + Send + Sync + 'static> = error.into();
    Arc::from(boxed)
}

fn replay(error: &CachedErr) -> anyhow::Error {
    anyhow::Error::new(Replay(Arc::clone(error)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub workspace: PathBuf,
    pub version: String,
    pub source: String,
    /// The lockfile that carried the version, by file name (`pnpm-lock.yaml`,
    /// `Cargo.lock`, …). Carried rather than derived: `source` is prose and
    /// `workspace` alone cannot say which of three JS lockfiles was consulted.
    pub lockfile: String,
    /// The directory containing `lockfile`. An ancestor of `workspace` by
    /// construction (every manager resolves `workspace` by walking up from
    /// it to find the lockfile), so a caller can name `workspace` relative to
    /// something even when it has no project root of its own to anchor on.
    pub lock_dir: PathBuf,
}

/// `package` is present in the lockfile only transitively, or not at all —
/// this workspace does not declare it. Typed so a caller resolving many
/// libraries can tell an uninteresting miss from a broken lockfile.
#[derive(Debug)]
pub struct Undeclared {
    pub package: String,
    pub workspace: PathBuf,
    /// The full diagnostic, rendered verbatim by `Display` so no output
    /// changes. Keeping this the outer error with no cause is what keeps
    /// `{:#}` and `Debug` byte-identical to the untyped form.
    message: String,
}

impl std::fmt::Display for Undeclared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Undeclared {}

/// What the importer graph can say about this workspace depending on a
/// package, independent of whether a version was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// A manifest in this workspace declares the package.
    Declared,
    /// The importer graph ran and the package is transitive-only or absent.
    Undeclared,
    /// Nothing could be established — no importer manifest, a malformed
    /// lockfile, or an ecosystem with no importer to ask.
    Unknown,
}

pub struct Inspection {
    pub evidence: Evidence,
    pub result: Result<Selection>,
}

/// One checkout's importer graph, ready to answer about any number of
/// packages. Everything that does not depend on the package — manifest
/// discovery, lockfile location, and the lockfile parse itself — happens once
/// per `Selector` rather than once per query.
pub struct Selector {
    context: LockContext,
}

enum LockContext {
    Js(JsContext),
    Toml(TomlContext),
}

impl Selector {
    /// Locate the ecosystem's lockfile and manifest for the workspace at
    /// `start`. JS lockfiles are parsed on first use rather than here, so a
    /// malformed lockfile `packageManager` did not select stays ignored.
    pub fn new(start: &Path, ecosystem: Ecosystem) -> Result<Self> {
        let context = match ecosystem {
            Ecosystem::Js => LockContext::Js(JsContext::new(start)?),
            Ecosystem::Rust => LockContext::Toml(TomlContext::cargo(start)?),
            Ecosystem::Python => LockContext::Toml(TomlContext::uv(start)?),
            Ecosystem::Git => bail!("git entries resolve by ref, not by lockfile"),
        };
        Ok(Selector { context })
    }

    /// The full report: declaration evidence plus the resolution result.
    /// Evidence is recorded where each manager establishes declaration, ahead
    /// of the checks that can fail afterwards, so a post-declaration failure
    /// still reports `Declared`.
    pub fn inspect(&self, package: &str) -> Inspection {
        self.report(package, Diagnostics::Collect)
    }

    /// The same evidence and the same selected version, without the candidate
    /// diagnostics: an `Undeclared` error enumerates no versions or declarers,
    /// and `Selection::source` names no other versions present. Collecting
    /// those costs a traversal of the whole lockfile per package, which a
    /// caller resolving a whole catalog against one checkout pays even for the
    /// packages that turn out to be undeclared. Rows outside the resolution's
    /// own path are not read at all, so a lockfile this rejects for a malformed
    /// unrelated row still resolves here.
    pub fn inspect_undiagnosed(&self, package: &str) -> Inspection {
        self.report(package, Diagnostics::Skip)
    }

    fn report(&self, package: &str, diagnostics: Diagnostics) -> Inspection {
        let mut evidence = Evidence::Unknown;
        let result = match &self.context {
            LockContext::Js(context) => context.select(package, diagnostics, &mut evidence),
            LockContext::Toml(context) => context.select(package, diagnostics, &mut evidence),
        };
        Inspection { evidence, result }
    }

    pub fn select(&self, package: &str) -> Result<Selection> {
        self.inspect(package).result
    }
}

/// The full report for a single package. A projection of [`Selector`], which a
/// caller resolving several packages against one checkout should build itself.
pub fn inspect(start: &Path, ecosystem: Ecosystem, package: &str) -> Inspection {
    match Selector::new(start, ecosystem) {
        Ok(selector) => selector.inspect(package),
        Err(error) => Inspection {
            evidence: Evidence::Unknown,
            result: Err(error),
        },
    }
}

/// Compatibility projection — every existing caller keeps this shape.
pub fn select(start: &Path, ecosystem: Ecosystem, package: &str) -> Result<Selection> {
    inspect(start, ecosystem, package).result
}

fn find_up(start: &Path, file: &str) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|directory| directory.join(file).is_file())
        .map(Path::to_path_buf)
}

fn rel_key(lock_dir: &Path, workspace: &Path) -> Result<String> {
    let relative = workspace.strip_prefix(lock_dir).with_context(|| {
        format!(
            "{} is not under {}",
            workspace.display(),
            lock_dir.display()
        )
    })?;
    Ok(relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}

/// A workspace's path relative to its lockfile, named for a reader. The root
/// workspace's relative path is empty, and rendering it as `.` reads as a
/// formatting fault rather than a location.
fn display_key(key: &str) -> &str {
    if key.is_empty() {
        "the root workspace"
    } else {
        key
    }
}

#[derive(Default)]
struct Candidates {
    versions: BTreeMap<String, BTreeSet<String>>,
    declarers: BTreeSet<String>,
    resolved: BTreeSet<(String, String)>,
}

impl Candidates {
    fn add_version(&mut self, version: impl Into<String>, at: impl Into<String>) {
        self.versions
            .entry(version.into())
            .or_default()
            .insert(at.into());
    }

    fn add_declarer(&mut self, declarer: impl Into<String>) {
        self.declarers.insert(declarer.into());
    }

    fn add_resolved(&mut self, version: impl Into<String>, declarer: impl Into<String>) {
        self.resolved.insert((version.into(), declarer.into()));
    }

    fn other_versions(&self, selected: &str) -> usize {
        self.versions
            .keys()
            .filter(|version| version.as_str() != selected)
            .count()
    }
}

/// Whether a resolution collects the candidate set behind the `undeclared`
/// diagnostic and `selection_source`'s "N other versions present" suffix.
///
/// Collecting it walks the whole lockfile once per package, so a caller
/// resolving every registered library against one checkout pays a full
/// traversal for each — including the ones it drops unread.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Diagnostics {
    Collect,
    Skip,
}

impl Diagnostics {
    fn collect(self, gather: impl FnOnce() -> Result<Candidates>) -> Result<Candidates> {
        match self {
            Diagnostics::Collect => gather(),
            Diagnostics::Skip => Ok(Candidates::default()),
        }
    }
}

fn undeclared(workspace: &Path, package: &str, candidates: &Candidates) -> anyhow::Error {
    let versions = if candidates.versions.is_empty() {
        "none".to_string()
    } else {
        candidates
            .versions
            .iter()
            .map(|(version, locations)| {
                format!(
                    "{version} (at {})",
                    locations.iter().cloned().collect::<Vec<_>>().join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let declarers = if candidates.declarers.is_empty() {
        "nothing in the lockfile declares it".to_string()
    } else {
        candidates
            .declarers
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    let resolved = if candidates.resolved.is_empty() {
        String::new()
    } else {
        format!(
            "\nresolved edges: {}",
            candidates
                .resolved
                .iter()
                .map(|(version, declarer)| format!("{version} (required by {declarer})"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let message = format!(
        "{} does not declare `{package}` (it is transitive); pin the version with --ref.\n\
         versions present in the lockfile: {versions}\n\
         declared by: {declarers}{resolved}",
        workspace.display()
    );
    anyhow::Error::new(Undeclared {
        package: package.to_string(),
        workspace: workspace.to_path_buf(),
        message,
    })
}

fn selection_source(
    workspace: &str,
    detail: &str,
    candidates: &Candidates,
    version: &str,
) -> String {
    match candidates.other_versions(version) {
        0 => format!("{} installs it ({detail})", display_key(workspace)),
        1 => format!(
            "{} installs it ({detail}; 1 other version present)",
            display_key(workspace)
        ),
        count => format!(
            "{} installs it ({detail}; {count} other versions present)",
            display_key(workspace)
        ),
    }
}

const JS_LOCKS: [(&str, &str); 3] = [
    ("bun", "bun.lock"),
    ("pnpm", "pnpm-lock.yaml"),
    ("npm", "package-lock.json"),
];

/// The lockfile one JS package manager writes.
fn js_lock_file(manager: &str) -> &'static str {
    JS_LOCKS
        .iter()
        .find(|(supported, _)| *supported == manager)
        .map_or("lockfile", |(_, file)| *file)
}

#[derive(Clone)]
enum ParsedLock {
    Bun(JsonValue),
    Pnpm(YamlValue),
    Npm(JsonValue),
}

/// One lockfile present in a lock directory: the manager that writes it, and
/// its parse outcome once something has asked for it.
type LockSlot = (&'static str, Option<Result<ParsedLock, CachedErr>>);

struct JsContext {
    workspace: PathBuf,
    lock_dir: PathBuf,
    relative: String,
    package_manager: Option<String>,
    /// Lockfiles present in `lock_dir`, parsed on first use and cached.
    /// A parse failure is stored, not propagated: the ambiguity arm needs it
    /// as one lockfile's *outcome*, and the non-ambiguous path must keep
    /// ignoring lockfiles `packageManager` did not select.
    present: RefCell<Vec<LockSlot>>,
}

impl JsContext {
    fn new(start: &Path) -> Result<Self> {
        let workspace = find_up(start, "package.json")
            .with_context(|| format!("no package.json at or above {}", start.display()))?;
        let lock_dir = workspace
            .ancestors()
            .find(|directory| {
                JS_LOCKS
                    .iter()
                    .any(|(_, file)| directory.join(file).is_file())
            })
            .with_context(|| format!("no JS lockfile at or above {}", workspace.display()))?
            .to_path_buf();
        let relative = rel_key(&lock_dir, &workspace)?;
        let package_manager = nearest_package_manager(&workspace, &lock_dir)?;
        let present = JS_LOCKS
            .iter()
            .copied()
            .filter(|(_, file)| lock_dir.join(file).is_file())
            .map(|(manager, _)| (manager, None))
            .collect::<Vec<_>>();
        Ok(JsContext {
            workspace,
            lock_dir,
            relative,
            package_manager,
            present: RefCell::new(present),
        })
    }

    fn present_managers(&self) -> Vec<&'static str> {
        self.present.borrow().iter().map(|(m, _)| *m).collect()
    }

    /// The parsed lockfile for `manager`, parsing on first use. The result —
    /// success or failure — is memoized, so a malformed lockfile yields the
    /// same error to every package that asks for it.
    ///
    /// Borrowed rather than cloned: a monorepo lockfile parses into tens of
    /// megabytes of `Value` tree, and handing out a copy per query would make
    /// resolving a whole catalog cost one deep clone per library.
    fn parsed(&self, manager: &str) -> Result<Ref<'_, ParsedLock>> {
        let index = self
            .present
            .borrow()
            .iter()
            .position(|(m, _)| *m == manager)
            .with_context(|| format!("no {manager} lockfile in {}", self.lock_dir.display()))?;
        // Hoisted so the shared borrow visibly ends before the mutable borrow
        // below, rather than resting on the `if` condition's temporary scope.
        let needs_parse = self.present.borrow()[index].1.is_none();
        if needs_parse {
            let parsed = parse_js_lock(manager, &self.lock_dir).map_err(cache_err);
            self.present.borrow_mut()[index].1 = Some(parsed);
        }
        // The failure case is taken before the borrow is mapped: `Ref::map`
        // cannot fail, and the replay needs the error by reference too.
        if let Err(error) = self.present.borrow()[index]
            .1
            .as_ref()
            .expect("just filled")
        {
            return Err(replay(error));
        }
        Ok(Ref::map(self.present.borrow(), |slots| {
            slots[index]
                .1
                .as_ref()
                .expect("just filled")
                .as_ref()
                .expect("the error case returned above")
        }))
    }

    /// Which present lockfile governs. Ambiguity is only reportable per
    /// package, because the message enumerates what each lockfile would have
    /// answered.
    fn choose(&self, package: &str) -> Result<&'static str> {
        let present = self.present_managers();
        match (present.as_slice(), self.package_manager.as_deref()) {
            ([manager], None) => Ok(*manager),
            ([manager], Some(selected)) if *manager == selected => Ok(*manager),
            ([manager], Some(selected)) => {
                let file = js_lock_file(manager);
                bail!(
                    "packageManager selects `{selected}`, but {} contains only {file} for `{manager}`",
                    self.lock_dir.display()
                )
            }
            (_, Some(selected)) => present
                .iter()
                .copied()
                .find(|manager| *manager == selected)
                .with_context(|| {
                    format!(
                        "packageManager selects `{selected}`, but {} holds only {}",
                        self.lock_dir.display(),
                        present
                            .iter()
                            .map(|manager| js_lock_file(manager))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }),
            (_, None) => {
                let outcomes = present
                    .iter()
                    .map(|manager| {
                        let file = js_lock_file(manager);
                        let mut probe = Evidence::Unknown;
                        // The message enumerates what each lockfile would have
                        // answered, and each answer is the diagnosed one.
                        let outcome =
                            self.select_from(manager, package, Diagnostics::Collect, &mut probe);
                        match outcome {
                            Ok(selection) => format!("{file} → {}", selection.version),
                            Err(error) => format!("{file} → {error}"),
                        }
                    })
                    .collect::<Vec<_>>();
                bail!(
                    "{} holds {} and no `packageManager` field says which one governs: {}; \
                     add \"packageManager\" to package.json",
                    self.lock_dir.display(),
                    present
                        .iter()
                        .map(|manager| js_lock_file(manager))
                        .collect::<Vec<_>>()
                        .join(" and "),
                    outcomes.join("; ")
                )
            }
        }
    }

    fn select_from(
        &self,
        manager: &str,
        package: &str,
        diagnostics: Diagnostics,
        evidence: &mut Evidence,
    ) -> Result<Selection> {
        let lock = self.parsed(manager)?;
        match &*lock {
            ParsedLock::Bun(value) => bun(
                value,
                &self.lock_dir,
                &self.workspace,
                &self.relative,
                package,
                diagnostics,
                evidence,
            ),
            ParsedLock::Pnpm(value) => pnpm(
                value,
                &self.lock_dir,
                &self.workspace,
                &self.relative,
                package,
                diagnostics,
                evidence,
            ),
            ParsedLock::Npm(value) => npm(
                value,
                &self.lock_dir,
                &self.workspace,
                &self.relative,
                package,
                diagnostics,
                evidence,
            ),
        }
    }

    fn select(
        &self,
        package: &str,
        diagnostics: Diagnostics,
        evidence: &mut Evidence,
    ) -> Result<Selection> {
        let manager = self.choose(package)?;
        self.select_from(manager, package, diagnostics, evidence)
    }
}

/// Read and parse one lockfile, including its `lockfileVersion` gate. This is
/// the only IO on the JS path: each manager's traversal takes the parsed value.
fn parse_js_lock(manager: &str, lock_dir: &Path) -> Result<ParsedLock> {
    match manager {
        "bun" => {
            let lock_path = lock_dir.join("bun.lock");
            let raw = std::fs::read_to_string(&lock_path)
                .with_context(|| format!("reading {}", lock_path.display()))?;
            let value = json5_ish(&raw)?;
            value
                .get("lockfileVersion")
                .and_then(JsonValue::as_u64)
                .context("bun.lock has no numeric `lockfileVersion`")?;
            Ok(ParsedLock::Bun(value))
        }
        "pnpm" => {
            let lock_path = lock_dir.join("pnpm-lock.yaml");
            let raw = std::fs::read_to_string(&lock_path)
                .with_context(|| format!("reading {}", lock_path.display()))?;
            let value: YamlValue =
                serde_yaml_ng::from_str(&raw).context("parsing pnpm-lock.yaml")?;
            let lockfile_version = value
                .get("lockfileVersion")
                .context("pnpm-lock.yaml has no `lockfileVersion`")?;
            match lockfile_version {
                YamlValue::String(version) if !version.is_empty() => {}
                YamlValue::Number(_) => {}
                _ => bail!("pnpm-lock.yaml `lockfileVersion` must be a string or numeric scalar"),
            }
            Ok(ParsedLock::Pnpm(value))
        }
        "npm" => {
            let lock_path = lock_dir.join("package-lock.json");
            let raw = std::fs::read_to_string(&lock_path)
                .with_context(|| format!("reading {}", lock_path.display()))?;
            let value: JsonValue =
                serde_json::from_str(&raw).context("parsing package-lock.json")?;
            let lockfile_version = value
                .get("lockfileVersion")
                .and_then(JsonValue::as_u64)
                .context("package-lock.json has no numeric `lockfileVersion`")?;
            if !matches!(lockfile_version, 2 | 3) {
                bail!("unsupported package-lock.json version {lockfile_version}; expected 2 or 3");
            }
            Ok(ParsedLock::Npm(value))
        }
        _ => bail!("unsupported JS package manager `{manager}`"),
    }
}

fn nearest_package_manager(workspace: &Path, lock_dir: &Path) -> Result<Option<String>> {
    for directory in workspace.ancestors() {
        if !directory.starts_with(lock_dir) {
            break;
        }
        let manifest = directory.join("package.json");
        if manifest.is_file() {
            let raw = std::fs::read_to_string(&manifest)
                .with_context(|| format!("reading {}", manifest.display()))?;
            let value: JsonValue = serde_json::from_str(&raw)
                .with_context(|| format!("parsing {}", manifest.display()))?;
            let object = value
                .as_object()
                .with_context(|| format!("{} must contain a JSON object", manifest.display()))?;
            if let Some(field) = object.get("packageManager") {
                let declaration = field.as_str().with_context(|| {
                    format!("{} packageManager must be a string", manifest.display())
                })?;
                let manager = declaration.split('@').next().unwrap_or(declaration);
                if !JS_LOCKS.iter().any(|(supported, _)| *supported == manager) {
                    bail!(
                        "{} declares unsupported packageManager `{declaration}`; supported managers are bun, pnpm, and npm",
                        manifest.display()
                    );
                }
                return Ok(Some(manager.to_string()));
            }
        }
        if directory == lock_dir {
            break;
        }
    }
    Ok(None)
}

const BUN_DEP_CLASSES: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
];
const PNPM_DEP_CLASSES: [&str; 3] = ["dependencies", "devDependencies", "optionalDependencies"];
const NPM_DEP_CLASSES: [&str; 4] = BUN_DEP_CLASSES;

fn json_class_maps<'a>(
    row: &'a JsonValue,
    classes: &[&str],
    location: &str,
) -> Result<Vec<&'a JsonMap<String, JsonValue>>> {
    let object = row
        .as_object()
        .with_context(|| format!("{location} must be an object"))?;
    classes
        .iter()
        .filter_map(|class| object.get(*class).map(|value| (*class, value)))
        .map(|(class, value)| {
            value
                .as_object()
                .with_context(|| format!("{location}.{class} must be an object"))
        })
        .collect()
}

fn json_declares(row: &JsonValue, classes: &[&str], package: &str, location: &str) -> Result<bool> {
    Ok(json_class_maps(row, classes, location)?
        .iter()
        .any(|dependencies| dependencies.contains_key(package)))
}

/// The specs a row declares for one package, across every dependency class it
/// appears in. The spec is what the depender asked for, which carries
/// provenance the installed row can lose.
fn json_declared_specs<'a>(
    row: &'a JsonValue,
    classes: &[&str],
    package: &str,
    location: &str,
) -> Result<Vec<&'a str>> {
    json_class_maps(row, classes, location)?
        .iter()
        .filter_map(|dependencies| dependencies.get(package))
        .map(|spec| {
            spec.as_str()
                .with_context(|| format!("{location} dependency `{package}` must be a string spec"))
        })
        .collect()
}

/// Resolution prefixes a Bun package spec's `name@resolution` tail may start
/// with. A git+ssh or basic-auth URL resolution can carry its own `@` (the
/// ssh-user marker in `git@github.com`, or userinfo in `user:pass@host`), so
/// the true name/resolution boundary is the `@` immediately before one of
/// these, not necessarily the last `@` in the spec.
const BUN_RESOLUTION_PREFIXES: [&str; 8] = [
    "workspace:",
    "root:",
    "link:",
    "file:",
    "http:",
    "https:",
    "git+",
    "github:",
];

fn name_and_version<'a>(spec: &'a str, location: &str) -> Result<(&'a str, &'a str)> {
    let scheme_split = spec.match_indices('@').find_map(|(index, _)| {
        let (name, rest) = spec.split_at(index);
        let resolution = &rest[1..];
        (!name.is_empty()
            && BUN_RESOLUTION_PREFIXES
                .iter()
                .any(|prefix| resolution.starts_with(prefix)))
        .then_some((name, resolution))
    });
    let (name, version) = scheme_split
        .or_else(|| {
            spec.rsplit_once('@')
                .filter(|(name, version)| !name.is_empty() && !version.is_empty())
        })
        .with_context(|| format!("{location} value `{spec}` is not a name@version spec"))?;
    Ok((name, version))
}

fn json5_ish(source: &str) -> Result<JsonValue> {
    jsonc_parser::parse_to_serde_value(source, &Default::default())
        .context("parsing bun.lock as JSONC")?
        .context("bun.lock is empty")
}

struct BunPackageRow<'a> {
    name: &'a str,
    resolution: &'a str,
    version: Option<&'a str>,
    info: Option<&'a JsonValue>,
}

fn bun_tuple_len(tuple: &[JsonValue], expected: usize, location: &str) -> Result<()> {
    if tuple.len() != expected {
        bail!(
            "{location} has {} fields; expected {expected} for this Bun package variant",
            tuple.len()
        );
    }
    Ok(())
}

fn bun_tuple_info<'a>(
    tuple: &'a [JsonValue],
    index: usize,
    location: &str,
) -> Result<&'a JsonValue> {
    let info = tuple
        .get(index)
        .with_context(|| format!("{location}[{index}] is missing"))?;
    info.as_object()
        .with_context(|| format!("{location}[{index}] must be a package info object"))?;
    Ok(info)
}

fn bun_package_row<'a>(row: &'a JsonValue, location: &str) -> Result<BunPackageRow<'a>> {
    let tuple = row
        .as_array()
        .with_context(|| format!("{location} must be an array"))?;
    let spec = tuple
        .first()
        .and_then(JsonValue::as_str)
        .with_context(|| format!("{location}[0] must be a name@resolution string"))?;
    let (name, resolution) = name_and_version(spec, &format!("{location}[0]"))?;

    let (version, info) = if resolution.starts_with("workspace:") {
        bun_tuple_len(tuple, 1, location)?;
        (None, None)
    } else if resolution.starts_with("root:")
        || resolution.starts_with("link:")
        || resolution.starts_with("file:")
        || resolution.starts_with("http:")
        || resolution.starts_with("https:")
    {
        bun_tuple_len(tuple, 2, location)?;
        (None, Some(bun_tuple_info(tuple, 1, location)?))
    } else if resolution.starts_with("git+") || resolution.starts_with("github:") {
        bun_tuple_len(tuple, 3, location)?;
        let bun_tag = tuple
            .get(2)
            .and_then(JsonValue::as_str)
            .with_context(|| format!("{location}[2] must be a Bun tag string"))?;
        if bun_tag.is_empty() {
            bail!("{location}[2] must not be empty");
        }
        (None, Some(bun_tuple_info(tuple, 1, location)?))
    } else {
        if resolution.contains(':') {
            bail!("{location} has unsupported non-registry resolution `{resolution}`");
        }
        bun_tuple_len(tuple, 4, location)?;
        tuple
            .get(1)
            .and_then(JsonValue::as_str)
            .with_context(|| format!("{location}[1] must be a registry string"))?;
        tuple
            .get(3)
            .and_then(JsonValue::as_str)
            .with_context(|| format!("{location}[3] must be an integrity string"))?;
        (Some(resolution), Some(bun_tuple_info(tuple, 2, location)?))
    };

    Ok(BunPackageRow {
        name,
        resolution,
        version,
        info,
    })
}

fn bun(
    value: &JsonValue,
    lock_dir: &Path,
    workspace: &Path,
    relative: &str,
    package: &str,
    diagnostics: Diagnostics,
    evidence: &mut Evidence,
) -> Result<Selection> {
    let workspaces = value
        .get("workspaces")
        .and_then(JsonValue::as_object)
        .context("bun.lock has no `workspaces` object")?;
    let entry = workspaces
        .get(relative)
        .with_context(|| format!("bun.lock has no workspace entry for `{relative}`"))?;
    let candidates = diagnostics.collect(|| bun_candidates(value, package))?;
    if !json_declares(
        entry,
        &BUN_DEP_CLASSES,
        package,
        &format!("workspaces.{relative}"),
    )? {
        *evidence = Evidence::Undeclared;
        return Err(undeclared(workspace, package, &candidates));
    }
    *evidence = Evidence::Declared;

    let packages = value
        .get("packages")
        .and_then(JsonValue::as_object)
        .context("bun.lock has no `packages` object")?;
    let workspace_name = match entry.get("name") {
        Some(name) => name
            .as_str()
            .with_context(|| format!("workspaces.{relative}.name must be a string"))?,
        None => "",
    };
    let scoped = format!("{workspace_name}/{package}");
    let (package_key, row) = packages
        .get_key_value(&scoped)
        .or_else(|| packages.get_key_value(package))
        .with_context(|| format!("bun.lock has no package row for `{package}`"))?;
    let decoded = bun_package_row(row, &format!("packages.{package_key}"))?;
    let version = decoded.version.with_context(|| {
        format!(
            "bun package `{package_key}` uses non-registry resolution `{}`; pin it with --ref",
            decoded.resolution
        )
    })?;
    Ok(Selection {
        workspace: workspace.to_path_buf(),
        version: version.to_string(),
        source: selection_source(relative, "bun.lock", &candidates, version),
        lockfile: "bun.lock".to_string(),
        lock_dir: lock_dir.to_path_buf(),
    })
}

fn bun_candidates(value: &JsonValue, package: &str) -> Result<Candidates> {
    let workspaces = value
        .get("workspaces")
        .and_then(JsonValue::as_object)
        .context("bun.lock has no `workspaces` object")?;
    let packages = value
        .get("packages")
        .and_then(JsonValue::as_object)
        .context("bun.lock has no `packages` object")?;
    let mut candidates = Candidates::default();

    for (key, row) in workspaces {
        if json_declares(row, &BUN_DEP_CLASSES, package, &format!("workspaces.{key}"))? {
            candidates.add_declarer(display_key(key));
        }
    }
    for (key, row) in packages {
        let decoded = bun_package_row(row, &format!("packages.{key}"))?;
        if decoded.name == package
            && let Some(version) = decoded.version
        {
            candidates.add_version(version, format!("packages.{key}"));
        }
        if let Some(info) = decoded.info
            && json_declares(
                info,
                &BUN_DEP_CLASSES,
                package,
                &format!("packages.{key} info"),
            )?
        {
            candidates.add_declarer(key);
        }
    }
    Ok(candidates)
}

fn yaml_mapping<'a>(value: &'a YamlValue, location: &str) -> Result<&'a YamlMap> {
    value
        .as_mapping()
        .with_context(|| format!("{location} must be a mapping"))
}

fn yaml_class_maps<'a>(
    row: &'a YamlValue,
    classes: &[&str],
    location: &str,
) -> Result<Vec<(&'static str, &'a YamlMap)>> {
    yaml_mapping(row, location)?;
    classes
        .iter()
        .filter_map(|class| row.get(*class).map(|value| (*class, value)))
        .map(|(class, value)| {
            Ok((
                match class {
                    "dependencies" => "dependencies",
                    "devDependencies" => "devDependencies",
                    "optionalDependencies" => "optionalDependencies",
                    _ => unreachable!("dependency class is fixed"),
                },
                value
                    .as_mapping()
                    .with_context(|| format!("{location}.{class} must be a mapping"))?,
            ))
        })
        .collect()
}

fn pnpm_locator(locator: &str) -> Result<(Option<String>, String)> {
    let unsupported = [
        "link:",
        "file:",
        "workspace:",
        "portal:",
        "patch:",
        "git:",
        "git+",
        "http:",
        "https:",
        "ssh:",
    ];
    if unsupported.iter().any(|prefix| locator.starts_with(prefix)) {
        bail!("unsupported non-registry pnpm locator `{locator}`");
    }
    let head = match locator.split_once('(') {
        Some((head, qualifiers)) if qualifiers.ends_with(')') => head,
        Some(_) => bail!("malformed peer-qualified pnpm locator `{locator}`"),
        None => locator,
    };
    let head = head.strip_prefix("npm:").unwrap_or(head);
    let (identity, version) = match head.rsplit_once('@') {
        Some((identity, version)) if !identity.is_empty() => (Some(identity.to_string()), version),
        _ => (None, head),
    };
    if version.is_empty()
        || !version.starts_with(|character: char| character.is_ascii_digit())
        || version.contains(['/', ':', ' ', '\t', '\n'])
    {
        bail!("unsupported pnpm registry locator `{locator}`");
    }
    Ok((identity, version.to_string()))
}

fn pnpm_locator_version(locator: &str) -> Result<String> {
    Ok(pnpm_locator(locator)?.1)
}

fn pnpm_entry_locator<'a>(entry: &'a YamlValue, importer: bool, location: &str) -> Result<&'a str> {
    if importer {
        return entry
            .as_mapping()
            .with_context(|| format!("{location} must be a mapping"))?
            .get("version")
            .and_then(YamlValue::as_str)
            .with_context(|| format!("{location}.version must be a string locator"));
    }
    entry
        .as_str()
        .with_context(|| format!("{location} must be a string locator"))
}

fn pnpm_package_key_version(key: &str, package: &str) -> Result<Option<String>> {
    let key = key.trim_start_matches('/');
    let head = key.split_once('(').map_or(key, |(head, _)| head);
    for separator in ['@', '/'] {
        let prefix = format!("{package}{separator}");
        if let Some(version) = head.strip_prefix(&prefix) {
            if version.is_empty() {
                bail!("pnpm package key `{key}` has no version");
            }
            return Ok(Some(pnpm_locator_version(version)?));
        }
    }
    Ok(None)
}

fn pnpm_has_package_row(value: &YamlValue, package: &str, version: &str) -> Result<bool> {
    for table_name in ["packages", "snapshots"] {
        let Some(table) = value.get(table_name) else {
            continue;
        };
        for key in yaml_mapping(table, table_name)?.keys() {
            let key = key
                .as_str()
                .with_context(|| format!("{table_name} contains a non-string key"))?;
            if pnpm_package_key_version(key, package)?.as_deref() == Some(version) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn pnpm_candidates(value: &YamlValue, package: &str) -> Result<Candidates> {
    let mut candidates = Candidates::default();
    for table_name in ["importers", "snapshots", "packages"] {
        let Some(table_value) = value.get(table_name) else {
            if table_name == "importers" {
                bail!("pnpm-lock.yaml has no `importers` mapping");
            }
            continue;
        };
        let table = yaml_mapping(table_value, table_name)?;
        for (owner, row) in table {
            let owner = owner
                .as_str()
                .with_context(|| format!("{table_name} contains a non-string key"))?;
            yaml_mapping(row, &format!("{table_name}.{owner}"))?;
            if table_name != "importers"
                && let Some(version) = pnpm_package_key_version(owner, package)?
            {
                candidates.add_version(version, format!("{table_name}.{owner}"));
            }
            for (class, dependencies) in
                yaml_class_maps(row, &PNPM_DEP_CLASSES, &format!("{table_name}.{owner}"))?
            {
                let package_key = YamlValue::String(package.to_string());
                let Some(entry) = dependencies.get(&package_key) else {
                    continue;
                };
                let location = format!("{table_name}.{owner}.{class}.{package}");
                let locator = pnpm_entry_locator(entry, table_name == "importers", &location)?;
                let version = pnpm_locator_version(locator)?;
                candidates.add_declarer(display_key(owner));
                candidates.add_resolved(&version, display_key(owner));
                candidates.add_version(version, location);
            }
        }
    }
    Ok(candidates)
}

fn pnpm(
    value: &YamlValue,
    lock_dir: &Path,
    workspace: &Path,
    relative: &str,
    package: &str,
    diagnostics: Diagnostics,
    evidence: &mut Evidence,
) -> Result<Selection> {
    let importer_key = if relative.is_empty() { "." } else { relative };
    let importer = value
        .get("importers")
        .and_then(|importers| importers.get(importer_key))
        .with_context(|| format!("pnpm-lock.yaml has no importer `{importer_key}`"))?;
    let candidates = diagnostics.collect(|| pnpm_candidates(value, package))?;
    let package_key = YamlValue::String(package.to_string());
    let mut matches = Vec::new();
    for (class, dependencies) in yaml_class_maps(
        importer,
        &PNPM_DEP_CLASSES,
        &format!("importers.{importer_key}"),
    )? {
        if let Some(entry) = dependencies.get(&package_key) {
            *evidence = Evidence::Declared;
            let location = format!("importers.{importer_key}.{class}.{package}");
            let locator = pnpm_entry_locator(entry, true, &location)?;
            let (identity, version) = pnpm_locator(locator)?;
            let target = identity.as_deref().unwrap_or(package);
            if !pnpm_has_package_row(value, target, &version)? {
                bail!(
                    "pnpm locator `{locator}` has no matching package row for `{target}@{version}`"
                );
            }
            matches.push(version);
        }
    }
    let version = match matches.as_slice() {
        [] => {
            *evidence = Evidence::Undeclared;
            return Err(undeclared(workspace, package, &candidates));
        }
        [version] => version.clone(),
        versions => bail!(
            "pnpm importer `{importer_key}` maps `{package}` through multiple dependency classes: {}",
            versions.join(", ")
        ),
    };
    Ok(Selection {
        workspace: workspace.to_path_buf(),
        source: selection_source(relative, "pnpm-lock.yaml", &candidates, &version),
        version,
        lockfile: "pnpm-lock.yaml".to_string(),
        lock_dir: lock_dir.to_path_buf(),
    })
}

fn npm_candidates(packages: &JsonMap<String, JsonValue>, package: &str) -> Result<Candidates> {
    let mut candidates = Candidates::default();
    let suffix = format!("node_modules/{package}");
    for (key, row) in packages {
        let location = format!("packages.{key}");
        if key == &suffix || key.ends_with(&format!("/{suffix}")) {
            let object = row
                .as_object()
                .with_context(|| format!("{location} must be an object"))?;
            // A `link` entry points at a workspace directory and records no
            // version of its own, so it contributes no candidate; the row it
            // points at is rejected by name when it is the selected one. A slot
            // holding an aliased package carries that package's release number,
            // which is no version of the queried package to suggest.
            let aliased = npm_row_identity(object, key)?.is_some_and(|name| name != package);
            if object.get("link").and_then(JsonValue::as_bool) != Some(true) && !aliased {
                let version = object
                    .get("version")
                    .and_then(JsonValue::as_str)
                    .with_context(|| format!("{location}.version must be a string"))?;
                candidates.add_version(version, &location);
            }
        }
        if json_declares(row, &NPM_DEP_CLASSES, package, &location)? {
            candidates.add_declarer(display_key(key));
        }
    }
    Ok(candidates)
}

/// A version number identifies upstream's code only when the row that carries
/// it was installed from the registry the docs repository publishes to. A git,
/// path, link or archive install can carry any version number over an
/// unrelated tree, so it is refused rather than mapped to an upstream tag.
fn assert_npm_registry_row(row: &JsonMap<String, JsonValue>, key: &str) -> Result<()> {
    let resolved = match row.get("resolved") {
        Some(value) => value
            .as_str()
            .with_context(|| format!("packages.{key}.resolved must be a string"))?,
        None => bail!("npm package `{key}` has no registry resolution; pin it with --ref"),
    };
    let linked = row.get("link").and_then(JsonValue::as_bool) == Some(true);
    if linked || !is_web_url(resolved) {
        bail!("npm package `{key}` uses non-registry resolution `{resolved}`; pin it with --ref");
    }
    Ok(())
}

fn is_web_url(value: &str) -> bool {
    value.starts_with("https://") || value.starts_with("http://")
}

/// npm names an install slot after the dependency key, not after the package
/// installed into it, so an `npm:` alias fills `node_modules/<key>` with a
/// different package and records that package's own release number. Mapping
/// that number onto the queried package's git tags would serve an unrelated
/// repository's tree, so a row carrying another package's `name` is refused.
/// An ordinary registry install records no `name` at all, and one that records
/// its own is equally ordinary; only a mismatch is a different package.
fn npm_row_identity<'a>(row: &'a JsonMap<String, JsonValue>, key: &str) -> Result<Option<&'a str>> {
    match row.get("name") {
        Some(value) => {
            Ok(Some(value.as_str().with_context(|| {
                format!("packages.{key}.name must be a string")
            })?))
        }
        None => Ok(None),
    }
}

fn assert_npm_row_is_package(
    row: &JsonMap<String, JsonValue>,
    key: &str,
    package: &str,
) -> Result<()> {
    if let Some(name) = npm_row_identity(row, key)?
        && name != package
    {
        bail!("npm package `{key}` installs `{name}`, not `{package}`; pin it with --ref");
    }
    Ok(())
}

/// A remote-tarball install writes an ordinary https `resolved` — for a tarball
/// served by the registry host, byte-identical to what a registry range install
/// writes — so the installed row cannot tell the two apart. npm copies the
/// tarball URL verbatim into the declaring dependency spec, where a registry
/// install leaves a semver range, dist-tag or `npm:` alias, so the spec
/// distinguishes them exactly. The tarball's own version number describes
/// whatever tree it holds, which need not be upstream's release of that number.
fn assert_npm_registry_spec(spec: &str, package: &str, declarer: &str) -> Result<()> {
    if is_web_url(spec) {
        bail!(
            "npm dependency `{package}` in {declarer} uses non-registry tarball spec `{spec}`; \
             pin it with --ref"
        );
    }
    Ok(())
}

fn npm(
    value: &JsonValue,
    lock_dir: &Path,
    workspace: &Path,
    relative: &str,
    package: &str,
    diagnostics: Diagnostics,
    evidence: &mut Evidence,
) -> Result<Selection> {
    let packages = value
        .get("packages")
        .and_then(JsonValue::as_object)
        .context("package-lock.json has no `packages` object")?;
    let entry = packages
        .get(relative)
        .with_context(|| format!("package-lock.json has no entry for `{relative}`"))?;
    let candidates = diagnostics.collect(|| npm_candidates(packages, package))?;
    let specs = json_declared_specs(
        entry,
        &NPM_DEP_CLASSES,
        package,
        &format!("packages.{relative}"),
    )?;
    if specs.is_empty() {
        *evidence = Evidence::Undeclared;
        return Err(undeclared(workspace, package, &candidates));
    }
    *evidence = Evidence::Declared;
    for spec in specs {
        assert_npm_registry_spec(spec, package, display_key(relative))?;
    }

    let mut directory = relative.to_string();
    loop {
        let probe = if directory.is_empty() {
            format!("node_modules/{package}")
        } else {
            format!("{directory}/node_modules/{package}")
        };
        if let Some(row) = packages.get(&probe) {
            let row = row
                .as_object()
                .with_context(|| format!("packages.{probe} must be an object"))?;
            assert_npm_registry_row(row, &probe)?;
            assert_npm_row_is_package(row, &probe, package)?;
            let version = row
                .get("version")
                .and_then(JsonValue::as_str)
                .with_context(|| format!("packages.{probe}.version must be a string"))?;
            return Ok(Selection {
                workspace: workspace.to_path_buf(),
                version: version.to_string(),
                source: selection_source(
                    relative,
                    &format!("{probe}; package-lock.json"),
                    &candidates,
                    version,
                ),
                lockfile: "package-lock.json".to_string(),
                lock_dir: lock_dir.to_path_buf(),
            });
        }
        match directory.rsplit_once('/') {
            Some((parent, _)) => directory = parent.to_string(),
            None if directory.is_empty() => break,
            None => directory.clear(),
        }
    }
    bail!("package-lock.json resolves no `{package}` reachable from `{relative}`")
}

#[derive(Debug, Deserialize)]
struct PackageLock {
    #[serde(default, rename = "package")]
    packages: Vec<LockPackage>,
}

#[derive(Debug, Deserialize)]
struct LockPackage {
    name: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    source: Option<toml::Value>,
    #[serde(default)]
    dependencies: Vec<toml::Value>,
    #[serde(default, rename = "dev-dependencies")]
    dev_dependencies: BTreeMap<String, Vec<toml::Value>>,
    #[serde(default, rename = "optional-dependencies")]
    optional_dependencies: BTreeMap<String, Vec<toml::Value>>,
    #[serde(default, rename = "dependency-groups")]
    dependency_groups: BTreeMap<String, Vec<toml::Value>>,
}

#[derive(Debug, Clone)]
struct DependencyEdge {
    name: String,
    version: Option<String>,
}

fn dependency_edge(value: &toml::Value, location: &str) -> Result<DependencyEdge> {
    if let Some(edge) = value.as_str() {
        let mut parts = edge.split_whitespace();
        let name = parts
            .next()
            .filter(|name| !name.is_empty())
            .with_context(|| format!("{location} contains an empty dependency edge"))?;
        let version = parts.next();
        let source = parts.collect::<Vec<_>>().join(" ");
        if !source.is_empty() && !(source.starts_with('(') && source.ends_with(')')) {
            bail!("{location} has unsupported dependency edge `{edge}`");
        }
        return Ok(DependencyEdge {
            name: name.to_string(),
            version: version.map(str::to_string),
        });
    }
    let table = value
        .as_table()
        .with_context(|| format!("{location} must be a string or table dependency edge"))?;
    let name = table
        .get("name")
        .and_then(toml::Value::as_str)
        .with_context(|| format!("{location}.name must be a string"))?;
    let version = match table.get("version") {
        Some(version) => Some(
            version
                .as_str()
                .with_context(|| format!("{location}.version must be a string"))?
                .to_string(),
        ),
        None => None,
    };
    Ok(DependencyEdge {
        name: name.to_string(),
        version,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PackageFormat {
    Cargo,
    Uv,
}

fn package_edges(package: &LockPackage, format: PackageFormat) -> Result<Vec<DependencyEdge>> {
    let mut values = package.dependencies.iter().collect::<Vec<_>>();
    if format == PackageFormat::Uv {
        for groups in [
            &package.dev_dependencies,
            &package.optional_dependencies,
            &package.dependency_groups,
        ] {
            for group in groups.values() {
                values.extend(group);
            }
        }
    }
    values
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            dependency_edge(
                value,
                &format!("package {} dependency {index}", package.name),
            )
        })
        .collect()
}

fn cargo_manifest_version(manifest: &toml::Value, lock_dir: &Path) -> Result<Option<String>> {
    let Some(version) = manifest
        .get("package")
        .and_then(|package| package.get("version"))
    else {
        return Ok(None);
    };
    if let Some(version) = version.as_str() {
        return Ok(Some(version.to_string()));
    }
    if version.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
        let root_manifest = lock_dir.join("Cargo.toml");
        let raw = std::fs::read_to_string(&root_manifest)
            .with_context(|| format!("reading {}", root_manifest.display()))?;
        let root: toml::Value =
            toml::from_str(&raw).with_context(|| format!("parsing {}", root_manifest.display()))?;
        return root
            .get("workspace")
            .and_then(|workspace| workspace.get("package"))
            .and_then(|package| package.get("version"))
            .and_then(toml::Value::as_str)
            .map(str::to_string)
            .context("workspace package version is not a string")
            .map(Some);
    }
    bail!("package.version must be a string or inherit workspace.package.version")
}

fn canonical_same(left: &Path, right: &Path) -> Result<bool> {
    Ok(
        std::fs::canonicalize(left)
            .with_context(|| format!("canonicalizing {}", left.display()))?
            == std::fs::canonicalize(right)
                .with_context(|| format!("canonicalizing {}", right.display()))?,
    )
}

fn uv_local_match(source: &Option<toml::Value>, lock_dir: &Path, workspace: &Path) -> Result<bool> {
    let Some(source) = source else {
        return Ok(false);
    };
    let table = source
        .as_table()
        .context("uv package source must be a table")?;
    for field in ["editable", "virtual", "directory"] {
        if let Some(path) = table.get(field) {
            let path = path
                .as_str()
                .with_context(|| format!("uv package source.{field} must be a string"))?;
            return canonical_same(&lock_dir.join(path), workspace);
        }
    }
    Ok(false)
}

/// The index of the `[[package]]` row that is this workspace itself. An index
/// rather than a borrow so a caller can keep it alongside the lock it points
/// into.
fn choose_member(
    packages: &[LockPackage],
    format: PackageFormat,
    member: &str,
    manifest_version: Option<&str>,
    lock_dir: &Path,
    workspace: &Path,
) -> Result<usize> {
    let named = packages
        .iter()
        .enumerate()
        .filter(|(_, package)| package.name == member)
        .collect::<Vec<_>>();
    if named.is_empty() {
        bail!("{} has no [[package]] for `{member}`", lock_dir.display());
    }

    if format == PackageFormat::Cargo {
        for (_, package) in &named {
            if let Some(source) = &package.source
                && source.as_str().is_none()
            {
                bail!("Cargo package `{member}` has a non-string source");
            }
        }
        let local = named
            .iter()
            .copied()
            .filter(|(_, package)| package.source.is_none())
            .filter(|(_, package)| {
                manifest_version.is_none_or(|version| package.version.as_deref() == Some(version))
            })
            .collect::<Vec<_>>();
        return match local.as_slice() {
            [(index, _)] => Ok(*index),
            [] => bail!(
                "{} has no local [[package]] matching Cargo member `{member}`{}",
                lock_dir.display(),
                manifest_version.map_or(String::new(), |version| format!(" {version}"))
            ),
            _ => bail!(
                "{} has ambiguous local [[package]] rows for Cargo member `{member}`",
                lock_dir.display()
            ),
        };
    }

    let local = named
        .iter()
        .copied()
        .filter_map(
            |(index, package)| match uv_local_match(&package.source, lock_dir, workspace) {
                Ok(true) => Some(Ok(index)),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            },
        )
        .collect::<Result<Vec<_>>>()?;
    match local.as_slice() {
        [index] => return Ok(*index),
        [] => {}
        _ => bail!(
            "{} has ambiguous local [[package]] rows for uv member `{member}`",
            lock_dir.display()
        ),
    }
    if let Some(version) = manifest_version {
        let matching = named
            .iter()
            .copied()
            .filter(|(_, package)| package.version.as_deref() == Some(version))
            .collect::<Vec<_>>();
        match matching.as_slice() {
            [(index, _)] => return Ok(*index),
            [] => bail!(
                "{} has no [[package]] matching uv member `{member}` {version}",
                lock_dir.display()
            ),
            _ => bail!(
                "{} has ambiguous [[package]] rows for uv member `{member}` {version}",
                lock_dir.display()
            ),
        }
    }
    match named.as_slice() {
        [(index, _)] => Ok(*index),
        _ => bail!(
            "{} has ambiguous [[package]] rows for uv member `{member}`",
            lock_dir.display()
        ),
    }
}

/// The versions and declarers of `package` across a `[[package]]` lockfile.
///
/// `versions` is load-bearing — `TomlContext::select` picks the version out of
/// it — so it is collected either way; matching a row costs one name compare.
/// The declarer and resolved-edge halves feed the `undeclared` diagnostic
/// alone, and decoding every row's dependency edges to build them is what
/// makes this a traversal rather than a scan.
fn package_candidates(
    packages: &[LockPackage],
    format: PackageFormat,
    package: &str,
    lock: &Path,
    diagnostics: Diagnostics,
) -> Result<Candidates> {
    let mut candidates = Candidates::default();
    for row in packages {
        if row.name == package {
            let version = row.version.as_deref().with_context(|| {
                format!(
                    "{} package `{}` is a dependency candidate but has no version",
                    lock.display(),
                    row.name
                )
            })?;
            candidates.add_version(version, lock.display().to_string());
        }
        if diagnostics == Diagnostics::Skip {
            continue;
        }
        for edge in package_edges(row, format)? {
            if edge.name == package {
                candidates.add_declarer(&row.name);
                if let Some(version) = edge.version {
                    candidates.add_resolved(version, &row.name);
                }
            }
        }
    }
    Ok(candidates)
}

/// Cargo source prefixes that name a package registry. `registry+` is the git
/// index form and `sparse+` the HTTP index form; every other prefix, and a
/// missing source, is a git, path or workspace install.
const CARGO_REGISTRY_PREFIXES: [&str; 2] = ["registry+", "sparse+"];

/// A version number identifies upstream's code only when the row that carries
/// it was installed from the registry the docs repository publishes to. A git,
/// path or URL install can carry any version number over an unrelated tree, so
/// it is refused rather than mapped to an upstream tag.
///
/// This runs on the rows that supply the already-selected version, so a
/// non-registry row is never quietly dropped from candidate collection — the
/// refusal names the locator instead of surfacing as a missing version.
fn assert_registry_source(
    packages: &[LockPackage],
    format: PackageFormat,
    package: &str,
    version: &str,
) -> Result<()> {
    let supplying = packages
        .iter()
        .filter(|row| row.name == package && row.version.as_deref() == Some(version));
    for row in supplying {
        match (format, &row.source) {
            (PackageFormat::Cargo, Some(source)) => {
                let source = source.as_str().with_context(|| {
                    format!("Cargo package `{package}` has a non-string source")
                })?;
                if !CARGO_REGISTRY_PREFIXES
                    .iter()
                    .any(|prefix| source.starts_with(prefix))
                {
                    bail!(
                        "Cargo package `{package}` {version} uses non-registry source `{source}`; \
                         pin it with --ref"
                    );
                }
            }
            (PackageFormat::Cargo, None) => bail!(
                "Cargo package `{package}` {version} has no registry source (it is a path or \
                 workspace dependency); pin it with --ref"
            ),
            (PackageFormat::Uv, Some(source)) => {
                let table = source
                    .as_table()
                    .with_context(|| format!("uv package `{package}` source must be a table"))?;
                if !table.contains_key("registry") {
                    bail!(
                        "uv package `{package}` {version} uses non-registry source `{source}`; \
                         pin it with --ref"
                    );
                }
            }
            (PackageFormat::Uv, None) => {
                bail!("uv package `{package}` {version} has no registry source; pin it with --ref")
            }
        }
    }
    Ok(())
}

/// One `[[package]]`-array lockfile — `Cargo.lock` or `uv.lock` — read, parsed
/// and located against the workspace that asked for it.
struct TomlContext {
    lock_path: PathBuf,
    lock_dir: PathBuf,
    workspace: PathBuf,
    format: PackageFormat,
    member: String,
    parsed: PackageLock,
    own_index: usize,
}

impl TomlContext {
    fn new(
        lock_path: PathBuf,
        lock_dir: PathBuf,
        workspace: PathBuf,
        format: PackageFormat,
        member: String,
        manifest_version: Option<&str>,
    ) -> Result<Self> {
        let raw = std::fs::read_to_string(&lock_path)
            .with_context(|| format!("reading {}", lock_path.display()))?;
        let parsed: PackageLock =
            toml::from_str(&raw).with_context(|| format!("parsing {}", lock_path.display()))?;
        if format == PackageFormat::Cargo {
            for package in &parsed.packages {
                if package.version.is_none() {
                    bail!(
                        "{} Cargo package `{}` has no version",
                        lock_path.display(),
                        package.name
                    );
                }
            }
        }
        let own_index = choose_member(
            &parsed.packages,
            format,
            &member,
            manifest_version,
            &lock_dir,
            &workspace,
        )?;
        Ok(TomlContext {
            lock_path,
            lock_dir,
            workspace,
            format,
            member,
            parsed,
            own_index,
        })
    }

    fn cargo(start: &Path) -> Result<Self> {
        let workspace = find_up(start, "Cargo.toml")
            .with_context(|| format!("no Cargo.toml at or above {}", start.display()))?;
        let manifest_path = workspace.join("Cargo.toml");
        let raw = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?;
        let manifest: toml::Value =
            toml::from_str(&raw).with_context(|| format!("parsing {}", manifest_path.display()))?;
        let member = manifest
            .get("package")
            .and_then(|package| package.get("name"))
            .and_then(toml::Value::as_str)
            .with_context(|| format!("{} has no [package] name", manifest_path.display()))?;
        let lock_dir = find_up(&workspace, "Cargo.lock")
            .with_context(|| format!("no Cargo.lock at or above {}", workspace.display()))?;
        let manifest_version = cargo_manifest_version(&manifest, &lock_dir)?;
        TomlContext::new(
            lock_dir.join("Cargo.lock"),
            lock_dir,
            workspace,
            PackageFormat::Cargo,
            member.to_string(),
            manifest_version.as_deref(),
        )
    }

    fn uv(start: &Path) -> Result<Self> {
        let workspace = find_up(start, "pyproject.toml")
            .with_context(|| format!("no pyproject.toml at or above {}", start.display()))?;
        let manifest_path = workspace.join("pyproject.toml");
        let raw = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?;
        let manifest: toml::Value =
            toml::from_str(&raw).with_context(|| format!("parsing {}", manifest_path.display()))?;
        let project = manifest
            .get("project")
            .and_then(toml::Value::as_table)
            .with_context(|| format!("{} has no [project] table", manifest_path.display()))?;
        let member = project
            .get("name")
            .and_then(toml::Value::as_str)
            .with_context(|| format!("{} has no [project] name", manifest_path.display()))?;
        let manifest_version = match project.get("version") {
            Some(version) => Some(version.as_str().with_context(|| {
                format!(
                    "{} project.version must be a string",
                    manifest_path.display()
                )
            })?),
            None => None,
        };
        let lock_dir = find_up(&workspace, "uv.lock")
            .with_context(|| format!("no uv.lock at or above {}", workspace.display()))?;
        TomlContext::new(
            lock_dir.join("uv.lock"),
            lock_dir,
            workspace,
            PackageFormat::Uv,
            member.to_string(),
            manifest_version,
        )
    }

    fn select(
        &self,
        package: &str,
        diagnostics: Diagnostics,
        evidence: &mut Evidence,
    ) -> Result<Selection> {
        let lock = self.lock_path.as_path();
        let member = self.member.as_str();
        let workspace = self.workspace.as_path();
        let own = &self.parsed.packages[self.own_index];
        let candidates = package_candidates(
            &self.parsed.packages,
            self.format,
            package,
            lock,
            diagnostics,
        )?;
        let matching_edges = package_edges(own, self.format)?
            .into_iter()
            .filter(|edge| edge.name == package)
            .collect::<Vec<_>>();
        let pinned = match matching_edges.as_slice() {
            [] => {
                *evidence = Evidence::Undeclared;
                return Err(undeclared(workspace, package, &candidates));
            }
            [edge] => {
                *evidence = Evidence::Declared;
                edge.version.as_deref()
            }
            _ => {
                *evidence = Evidence::Declared;
                bail!(
                    "{member} maps `{package}` through multiple dependency edges in {}",
                    lock.display()
                )
            }
        };
        let versions = candidates.versions.keys().cloned().collect::<Vec<_>>();
        let version = if let Some(pinned) = pinned {
            if !candidates.versions.contains_key(pinned) {
                bail!(
                    "{member} pins `{package}` {pinned}, but {} records no matching package row",
                    lock.display()
                );
            }
            pinned.to_string()
        } else {
            match versions.as_slice() {
                [] => bail!(
                    "{} declares `{package}` but {} records no version",
                    workspace.display(),
                    lock.display()
                ),
                [version] => version.clone(),
                _ => bail!(
                    "{} records a resolution fork for `{package}` ({}) and the dependency edge from \
                     `{member}` does not name one; pin one with --ref",
                    lock.display(),
                    versions.join(", ")
                ),
            }
        };
        assert_registry_source(&self.parsed.packages, self.format, package, &version)?;
        let relative = rel_key(&self.lock_dir, workspace)?;
        let detail = lock
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("lockfile");
        Ok(Selection {
            workspace: workspace.to_path_buf(),
            source: selection_source(&relative, detail, &candidates, &version),
            version,
            lockfile: detail.to_string(),
            lock_dir: self.lock_dir.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc_handles_urls_trailing_commas_comments_and_empty_input() {
        let value = json5_ish(r#"{ "url": "https://example.com/x", "a": [1, 2,], }"#).unwrap();
        assert_eq!(value["url"], "https://example.com/x");
        assert_eq!(value["a"].as_array().unwrap().len(), 2);

        let value = json5_ish("{\n  \"a\": 1, // note\n}").unwrap();
        assert_eq!(value["a"], 1);

        let value = json5_ish("{\n  \"a\": 1, /* note */ }").unwrap();
        assert_eq!(value["a"], 1);

        let value = json5_ish("{ /* lead */ \"a\": [1, 2 /* tail */ ] }").unwrap();
        assert_eq!(value["a"].as_array().unwrap().len(), 2);

        let error = json5_ish("").unwrap_err().to_string();
        assert!(error.contains("empty"), "{error}");
    }
}
