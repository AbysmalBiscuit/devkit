//! Which project config files apply where. The one definition of the file set,
//! its order, its cutoff, and its dedupe — shared by the full config resolver,
//! the lock harness, and the docs manifest, each of which composes its own
//! global inputs on top.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

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

/// Project config layers applying at `start`, lowest precedence first.
/// Excludes the home config and any `--config` / `$DEVKIT_CONFIG` override:
/// those differ per reader, so each composes its own.
pub fn project_layers(start: &Path, main_checkout: Option<&Path>) -> Result<Vec<Layer>> {
    Ok(project_layers_rooted(start, main_checkout)?.0)
}

/// `project_layers`, plus whether a `[config] root = true` marker fired and
/// cut off layers above it. `discover` needs the flag to decide whether the
/// home config still applies on top; callers that don't merge in a home
/// config use `project_layers` instead.
pub(crate) fn project_layers_rooted(
    start: &Path,
    main_checkout: Option<&Path>,
) -> Result<(Vec<Layer>, bool)> {
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
    let rooted = apply_cutoff(&mut ordered)?;
    Ok((ordered, rooted))
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

/// Whether a layer file declares `[config] root = true`.
fn declares_root(path: &Path) -> Result<bool> {
    let body = std::fs::read_to_string(path)
        .with_context(|| format!("reading config layer {}", path.display()))?;
    let table: toml::Table = toml::from_str(&body)
        .with_context(|| format!("parsing config layer {}", path.display()))?;
    Ok(crate::is_root_layer(&table))
}

/// `[config] root = true` drops every layer lower in precedence than the
/// directory that declares it. A directory can hold two files — the tracked
/// `devkit.toml` and the untracked `devkit.local.toml` beside it — and the
/// marker in either one draws the barrier at the directory: both of that
/// directory's layers survive, and everything above the directory is
/// dropped. Scans from the nearest-to-`start` layer backward and stops at
/// the first (i.e. last in precedence order) match, so nothing below the
/// barrier is ever read — a malformed or unreadable ancestor layer the
/// barrier was meant to hide never gets parsed. Returns whether a barrier
/// was found.
fn apply_cutoff(layers: &mut Vec<Layer>) -> Result<bool> {
    let mut barrier = None;
    for (i, layer) in layers.iter().enumerate().rev() {
        if declares_root(&layer.path)? {
            barrier = Some(i);
            break;
        }
    }
    let Some(mut cut) = barrier else {
        return Ok(false);
    };
    while cut > 0 && layers[cut - 1].path.parent() == layers[cut].path.parent() {
        cut -= 1;
    }
    layers.drain(..cut);
    Ok(true)
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

    /// A barrier hides what is above it, not just what is below the point
    /// where the walk used to stop: a malformed layer the barrier makes
    /// irrelevant must not fail the lookup.
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
