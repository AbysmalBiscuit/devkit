//! Every relative link in the living docs lands on a file that exists, and a
//! `#fragment` into a markdown file lands on one of its headings.
//!
//! Plans and specs under `docs/superpowers/` are snapshots of their day and
//! keep the links they were written with.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

fn living_docs() -> Vec<PathBuf> {
    let root = Path::new(ROOT);
    let mut out = vec![root.join("README.md"), root.join("AGENTS.md")];
    for entry in fs::read_dir(root.join("docs")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
    collect_markdown(&root.join("skills"), &mut out);
    out
}

fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_markdown(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

/// Markdown outside fenced code blocks, where a `](` is prose rather than code.
fn prose(text: &str) -> String {
    let mut fenced = false;
    let mut out = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
        } else if !fenced {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn link_targets(text: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("](") {
        rest = &rest[i + 2..];
        let end = rest.find(')').unwrap_or(rest.len());
        targets.push(rest[..end].trim().to_string());
        rest = &rest[end..];
    }
    targets
}

/// GitHub's anchor for a heading: lowercased, punctuation other than `-` and
/// `_` dropped, spaces turned into hyphens, repeats suffixed `-1`, `-2`.
fn anchors(markdown: &str) -> Vec<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut out = Vec::new();
    for line in prose(markdown).lines() {
        let Some(heading) = line.strip_prefix('#') else {
            continue;
        };
        let heading = heading.trim_start_matches('#').trim();
        let slug: String = heading
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
            .map(|c| if c == ' ' { '-' } else { c })
            .collect();
        let n = seen.entry(slug.clone()).or_insert(0);
        out.push(if *n == 0 {
            slug.clone()
        } else {
            format!("{slug}-{n}")
        });
        *n += 1;
    }
    out
}

fn broken_links(doc: &Path) -> Vec<String> {
    let text = fs::read_to_string(doc).unwrap();
    let mut broken = Vec::new();
    for target in link_targets(&prose(&text)) {
        if target.contains("://") || target.starts_with("mailto:") || target.starts_with('<') {
            continue;
        }
        let (file, fragment) = target.split_once('#').unwrap_or((&target, ""));
        let resolved = if file.is_empty() {
            doc.to_path_buf()
        } else {
            doc.parent().unwrap().join(file)
        };
        if !resolved.exists() {
            broken.push(format!("{target}: no such file"));
            continue;
        }
        if !fragment.is_empty() && resolved.extension().is_some_and(|e| e == "md") {
            let body = fs::read_to_string(&resolved).unwrap();
            if !anchors(&body).iter().any(|a| a == fragment) {
                broken.push(format!("{target}: no such heading"));
            }
        }
    }
    broken
}

#[test]
fn living_docs_link_only_to_what_exists() {
    let root = Path::new(ROOT);
    let broken: Vec<String> = living_docs()
        .iter()
        .flat_map(|doc| {
            let rel = doc.strip_prefix(root).unwrap().display().to_string();
            broken_links(doc)
                .into_iter()
                .map(move |b| format!("{rel}: {b}"))
        })
        .collect();
    assert!(broken.is_empty(), "broken links:\n{}", broken.join("\n"));
}

#[test]
fn anchors_follow_github_slug_rules() {
    let md = "# `[harness.log]`\n## `devrun down` scope\n## Tasks\n## Tasks\n";
    assert_eq!(anchors(md), [
        "harnesslog",
        "devrun-down-scope",
        "tasks",
        "tasks-1"
    ]);
}
