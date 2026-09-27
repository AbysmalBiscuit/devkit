//! Which project config files apply where. The one definition of the file set,
//! its order, its cutoff, and its dedupe — shared by the full config resolver,
//! the lock harness, and the docs manifest, each of which composes its own
//! global inputs on top.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Tracked, committed config: the project's own settings.
pub const CONFIG_FILE: &str = "devkit.toml";
/// Untracked overrides beside it, for what one machine or checkout needs and
/// the repository should not carry.
pub const LOCAL_CONFIG_FILE: &str = "devkit.local.toml";

/// Where a layer came from. `Ancestor` and `Checkout` split at the nearest
/// directory at or above `start` that contains a config file — not at any
/// git checkout root, which this crate has no way to ask about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerKind {
    /// Above the nearest config-bearing directory.
    Ancestor,
    /// The nearest config-bearing directory itself.
    Checkout,
    /// Inherited from this repository's main checkout.
    MainCheckout,
}

/// One config file location, plus which side of the `Ancestor`/`Checkout`
/// split it falls on.
#[derive(Debug, Clone)]
pub struct Layer {
    pub path: PathBuf,
    pub kind: LayerKind,
}

/// What a discovery pass found: the surviving layers, whether a
/// `[config] root = true` barrier fired, and the bodies the barrier scan
/// already parsed. `parsed` is aligned with `layers` and holds `None` only
/// where the scan stopped before reaching that layer, so a caller that needs
/// every body reads just those back rather than re-reading the whole stack.
pub(crate) struct Discovery {
    pub layers: Vec<Layer>,
    pub rooted: bool,
    pub parsed: Vec<Option<toml::Table>>,
}

/// Project config layers applying at `start`, lowest precedence first.
/// Excludes the home config and any `--config` / `$DEVKIT_CONFIG` override:
/// those differ per reader, so each composes its own.
pub fn project_layers(start: &Path, main_checkout: Option<&Path>) -> Result<Vec<Layer>> {
    Ok(project_layers_rooted(start, main_checkout)?.layers)
}

/// `project_layers`, plus whether a `[config] root = true` marker fired and
/// cut off layers above it. `discover` needs the flag to decide whether the
/// home config still applies on top; callers that don't merge in a home
/// config use `project_layers` instead.
pub(crate) fn project_layers_rooted(
    start: &Path,
    main_checkout: Option<&Path>,
) -> Result<Discovery> {
    let mut layers = candidates(start, main_checkout);
    let (barrier, bodies) = scan_cutoff(&layers);
    let cut = barrier.unwrap_or(0);
    layers.drain(..cut);
    // Nearest first, so the error reported is the one closest to `start`.
    let mut parsed = Vec::with_capacity(layers.len());
    for body in bodies.into_iter().skip(cut).rev() {
        parsed.push(body.transpose()?);
    }
    parsed.reverse();
    Ok(Discovery {
        layers,
        rooted: barrier.is_some(),
        parsed,
    })
}

/// A project layer that will not read or parse.
#[derive(Debug)]
pub struct BrokenLayer {
    pub path: PathBuf,
    pub error: anyhow::Error,
}

/// What [`read_project_layers`] found: every layer that parsed, with its
/// body, lowest precedence first, and every one that did not.
#[derive(Debug, Default)]
pub struct ReadLayers {
    pub layers: Vec<(Layer, toml::Table)>,
    pub broken: Vec<BrokenLayer>,
}

/// The layers [`project_layers`] finds, parsed, for a reader that must not
/// lose every layer to one bad file. A layer that will not read or parse goes
/// to `broken` instead of failing the walk. It cannot declare a
/// `[config] root = true` barrier, so the layers above it stay in.
pub fn read_project_layers(start: &Path, main_checkout: Option<&Path>) -> ReadLayers {
    let layers = candidates(start, main_checkout);
    let (barrier, bodies) = scan_cutoff(&layers);
    let mut read = ReadLayers::default();
    for (layer, body) in layers.into_iter().zip(bodies).skip(barrier.unwrap_or(0)) {
        match body.unwrap_or_else(|| read_table(&layer.path)) {
            Ok(table) => read.layers.push((layer, table)),
            Err(error) => read.broken.push(BrokenLayer {
                path: layer.path,
                error,
            }),
        }
    }
    read
}

/// Every config file that could apply at `start`, lowest precedence first,
/// before any `[config] root = true` barrier is applied.
fn candidates(start: &Path, main_checkout: Option<&Path>) -> Vec<Layer> {
    let root = start
        .ancestors()
        .find(|d| d.join(CONFIG_FILE).is_file() || d.join(LOCAL_CONFIG_FILE).is_file())
        .unwrap_or(start);

    let mut ordered: Vec<Layer> = Vec::new();

    // Ancestors, outermost first, above the nearest config-bearing directory.
    let mut ancestors: Vec<&Path> = start
        .ancestors()
        .skip_while(|d| *d != root)
        .skip(1)
        .collect();
    ancestors.reverse();
    for dir in ancestors {
        ordered.extend(files_in(dir, LayerKind::Ancestor));
    }

    // The main checkout sits above every ancestor and below this checkout.
    if let Some(main) = main_checkout {
        ordered.extend(files_in(main, LayerKind::MainCheckout));
    }

    // `root` is by construction the nearest config-bearing directory to
    // `start`, so no directory strictly between it and `start` can hold a
    // config file — only `root`'s own files ever contribute here.
    ordered.extend(files_in(root, LayerKind::Checkout));

    dedupe(&mut ordered);
    ordered
}

/// The config files present in one directory, tracked first so the untracked
/// one outranks it.
fn files_in(dir: &Path, kind: LayerKind) -> Vec<Layer> {
    [CONFIG_FILE, LOCAL_CONFIG_FILE]
        .into_iter()
        .map(|name| dir.join(name))
        .filter(|p| p.is_file())
        .map(|path| Layer { path, kind })
        .collect()
}

/// Keep the highest-precedence occurrence of each file. Canonicalizing is
/// only how two layers are recognized as the same file — the surviving
/// `Layer` keeps its original path spelling and the `LayerKind` it was
/// found under, rather than being replaced by the canonical form.
fn dedupe(layers: &mut Vec<Layer>) {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut keep = vec![true; layers.len()];
    for i in (0..layers.len()).rev() {
        let key = std::fs::canonicalize(&layers[i].path).unwrap_or_else(|_| layers[i].path.clone());
        if seen.contains(&key) {
            keep[i] = false;
        } else {
            seen.push(key);
        }
    }
    let mut iter = keep.into_iter();
    layers.retain(|_| iter.next().unwrap_or(true));
}

/// One layer file's parsed body.
fn read_table(path: &Path) -> Result<toml::Table> {
    let body = std::fs::read_to_string(path)
        .with_context(|| format!("reading config layer {}", path.display()))?;
    toml::from_str(&body).with_context(|| format!("parsing config layer {}", path.display()))
}

/// `[config] root = true` drops every layer lower in precedence than the
/// directory that declares it. A directory can hold two files — the tracked
/// `devkit.toml` and the untracked `devkit.local.toml` beside it — and the
/// marker in either one draws the barrier at the directory: both of that
/// directory's layers survive, and everything above the directory is
/// dropped. Scans from the nearest-to-`start` layer backward and stops at
/// the first (i.e. last in precedence order) match, so nothing below the
/// barrier is ever read, and a malformed or unreadable ancestor layer the
/// barrier was meant to hide never gets parsed. A layer that will not parse
/// declares no barrier, and the scan goes on past it; whether it fails the
/// walk is the caller's call.
///
/// Returns the index of the first surviving layer when a barrier fired, and
/// what the scan read of each layer, so the caller need not read the
/// surviving layers a second time; those past the scan's stopping point stay
/// `None`.
fn scan_cutoff(layers: &[Layer]) -> (Option<usize>, Vec<Option<Result<toml::Table>>>) {
    let mut bodies: Vec<_> = layers.iter().map(|_| None).collect();
    for i in (0..layers.len()).rev() {
        let body = read_table(&layers[i].path);
        let is_root = body.as_ref().is_ok_and(crate::is_root_layer);
        bodies[i] = Some(body);
        if is_root {
            let mut cut = i;
            while cut > 0 && layers[cut - 1].path.parent() == layers[i].path.parent() {
                cut -= 1;
            }
            return (Some(cut), bodies);
        }
    }
    (None, bodies)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(at: &Path, name: &str, body: &str) -> PathBuf {
        std::fs::create_dir_all(at).unwrap();
        let p = at.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn local_outranks_tracked_in_one_directory() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "devkit.toml", "");
        write(dir.path(), "devkit.local.toml", "");
        let layers = project_layers(dir.path(), None).unwrap();
        let names: Vec<_> = layers
            .iter()
            .map(|l| l.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["devkit.toml", "devkit.local.toml"]);
    }

    #[test]
    fn deeper_directories_outrank_shallower() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("a/b");
        write(dir.path(), "devkit.toml", "");
        write(&deep, "devkit.toml", "");
        let layers = project_layers(&deep, None).unwrap();
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[1].path.parent().unwrap(), deep);
    }

    /// The marker is a positional barrier: it drops everything lower in
    /// precedence and leaves everything nearer `start` alone.
    #[test]
    fn root_marker_drops_only_lower_precedence_layers() {
        let dir = tempfile::tempdir().unwrap();
        let mid = dir.path().join("mid");
        let deep = mid.join("deep");
        write(dir.path(), "devkit.toml", "");
        write(&mid, "devkit.toml", "[config]\nroot = true\n");
        write(&deep, "devkit.toml", "");
        let layers = project_layers(&deep, None).unwrap();
        assert_eq!(layers.len(), 2, "the outermost layer is cut off");
        assert_eq!(layers[0].path.parent().unwrap(), mid);
        assert_eq!(layers[1].path.parent().unwrap(), deep);
    }

    /// The marker can land in either file of a directory that holds both.
    /// The barrier falls at the directory, not at the individual file: a
    /// `root = true` in the untracked `devkit.local.toml` must not discard
    /// the tracked `devkit.toml` sitting beside it.
    #[test]
    fn root_marker_in_local_file_keeps_tracked_file_beside_it() {
        let dir = tempfile::tempdir().unwrap();
        let barrier = dir.path().join("barrier");
        write(dir.path(), "devkit.toml", "");
        write(&barrier, "devkit.toml", "");
        write(&barrier, "devkit.local.toml", "[config]\nroot = true\n");
        let layers = project_layers(&barrier, None).unwrap();
        let names: Vec<_> = layers
            .iter()
            .map(|l| l.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            ["devkit.toml", "devkit.local.toml"],
            "both files in the barrier directory survive; the layer above is dropped"
        );
    }

    /// A barrier hides what is above it entirely: a malformed layer the
    /// barrier makes irrelevant must not fail the lookup.
    #[test]
    fn root_marker_hides_a_malformed_layer_above_it() {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child");
        write(dir.path(), "devkit.toml", "not valid toml [[[");
        write(&child, "devkit.toml", "[config]\nroot = true\n");
        let layers = project_layers(&child, None).unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].path.parent().unwrap(), child);
    }

    /// The barrier applies to a `MainCheckout` layer exactly as it does to
    /// any other: a repository that declares `root = true` in its own
    /// `devkit.toml` cuts off the ancestors above a linked worktree that
    /// inherits it, rather than letting them re-merge with the main
    /// checkout's config.
    #[test]
    fn root_marker_in_main_checkout_cuts_off_ancestors_above_it() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().join("outer");
        let start = outer.join("linked");
        let main = dir.path().join("main");
        write(&outer, "devkit.toml", "");
        write(&start, "devkit.toml", "");
        write(&main, "devkit.toml", "[config]\nroot = true\n");

        let layers = project_layers(&start, Some(&main)).unwrap();

        assert_eq!(
            layers
                .iter()
                .map(|l| l.path.parent().unwrap().to_path_buf())
                .collect::<Vec<_>>(),
            vec![main.clone(), start.clone()],
            "the ancestor above the main checkout's barrier is dropped"
        );
    }

    /// A directory holding only the tracked file, nested beneath one
    /// holding both, still finds its own directory as the nearest
    /// config-bearing one — `root` selection cannot require both files
    /// to be present, only either.
    #[test]
    fn a_directory_with_only_the_tracked_file_beneath_one_with_both() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().join("outer");
        let nested = outer.join("nested");
        write(&outer, "devkit.toml", "");
        write(&outer, "devkit.local.toml", "");
        write(&nested, "devkit.toml", "");
        let layers = project_layers(&nested, None).unwrap();
        assert_eq!(
            layers.iter().map(|l| l.path.clone()).collect::<Vec<_>>(),
            vec![
                outer.join("devkit.toml"),
                outer.join("devkit.local.toml"),
                nested.join("devkit.toml"),
            ]
        );
    }

    /// Two directories on the path to `start` can each declare `root = true`.
    /// The barrier is the one nearest `start` — the outer marker is moot
    /// because the walk it once broke never reached that far.
    #[test]
    fn nested_root_markers_use_the_one_nearest_start() {
        let dir = tempfile::tempdir().unwrap();
        let mid = dir.path().join("mid");
        write(dir.path(), "devkit.toml", "[config]\nroot = true\n");
        write(&mid, "devkit.toml", "[config]\nroot = true\n");
        let layers = project_layers(&mid, None).unwrap();
        assert_eq!(
            layers.len(),
            1,
            "only the inner marker's directory survives"
        );
        assert_eq!(layers[0].path.parent().unwrap(), mid);
    }

    /// `main_checkout` can name a directory the ancestor walk also visits,
    /// producing two `Layer`s for the same file. Dedupe keeps the
    /// higher-precedence one — here the `MainCheckout` layer, inserted after
    /// the ancestor walk — without replacing its path with the canonical
    /// form.
    #[test]
    fn dedupe_keeps_the_higher_precedence_spelling_and_kind() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().join("outer");
        let mid = outer.join("mid");
        write(&outer, "devkit.toml", "");
        write(&mid, "devkit.toml", "");
        // Names the same file the ancestor walk already found under `outer`,
        // spelled differently so a survived-verbatim path is distinguishable
        // from one silently replaced by its canonical form.
        let main_checkout = outer.join(".");

        let layers = project_layers(&mid, Some(&main_checkout)).unwrap();

        assert_eq!(
            layers.len(),
            2,
            "the ancestor duplicate of `outer` is dropped"
        );
        assert_eq!(layers[0].kind, LayerKind::MainCheckout);
        assert_eq!(layers[0].path, main_checkout.join(CONFIG_FILE));
        assert_eq!(layers[1].kind, LayerKind::Checkout);
        assert_eq!(layers[1].path.parent().unwrap(), mid);
    }
}
