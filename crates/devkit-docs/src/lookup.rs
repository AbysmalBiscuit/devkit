//! Package-registry lookups: package name → repo URL, resolved once at
//! `docm add` time and stored in the manifest. HTTP sits behind a trait so
//! tests stub it and nothing else ever touches the network.

use crate::manifest::Ecosystem;
use anyhow::{Context, Result, bail};
use std::path::Path;

pub trait Registry {
    fn repo_url(&self, eco: Ecosystem, package: &str) -> Result<String>;
}

/// Real lookups. crates.io rejects requests without a User-Agent.
pub struct Http;

impl Registry for Http {
    fn repo_url(&self, eco: Ecosystem, package: &str) -> Result<String> {
        let v: serde_json::Value = match eco {
            Ecosystem::Rust => ureq::get(&format!("https://crates.io/api/v1/crates/{package}"))
                .set(
                    "User-Agent",
                    "devkit-docm (https://github.com/AbysmalBiscuit/devkit)",
                )
                .call()?
                .into_json()?,
            Ecosystem::Js => ureq::get(&format!("https://registry.npmjs.org/{package}"))
                .call()?
                .into_json()?,
            Ecosystem::Python => ureq::get(&format!("https://pypi.org/pypi/{package}/json"))
                .call()?
                .into_json()?,
            Ecosystem::Git => bail!("git entries carry an explicit repo URL"),
        };
        extract(eco, &v)
    }
}

/// Ecosystem markers checked closest-directory-first walking up from `cwd`.
const LOCAL_MARKERS: &[(Ecosystem, &[&str])] = &[
    (
        Ecosystem::Js,
        &[
            "bun.lock",
            "package-lock.json",
            "pnpm-lock.yaml",
            "package.json",
        ],
    ),
    (Ecosystem::Rust, &["Cargo.toml"]),
    (Ecosystem::Python, &["pyproject.toml", "uv.lock"]),
];

/// The ecosystem of the local project `cwd` sits in, found by walking up
/// from `cwd` to the closest directory carrying any ecosystem marker. A
/// directory carrying markers for more than one ecosystem (a polyglot
/// monorepo root) is an ambiguous signal, not a tie to break — this returns
/// `None` for it rather than picking one, so the caller's ambiguous-hits
/// hard error fires instead of a silent guess.
fn local_ecosystem(cwd: &Path) -> Option<Ecosystem> {
    for dir in cwd.ancestors() {
        let mut matched = LOCAL_MARKERS
            .iter()
            .filter(|(_, markers)| markers.iter().any(|m| dir.join(m).is_file()))
            .map(|(eco, _)| *eco);
        if let Some(eco) = matched.next() {
            return if matched.next().is_none() {
                Some(eco)
            } else {
                None
            };
        }
    }
    None
}

/// Probe crates.io, npm, and PyPI for `package`. A single hit wins; with no
/// hits this is a hard error. Multiple hits are ambiguous — the local
/// project's ecosystem (if any marker is found walking up from `cwd`)
/// resolves it, otherwise this is a hard error naming every match.
pub fn detect(reg: &dyn Registry, package: &str, cwd: &Path) -> Result<(Ecosystem, String)> {
    let mut hits = Vec::new();
    let mut errs = Vec::new();
    for eco in [Ecosystem::Rust, Ecosystem::Js, Ecosystem::Python] {
        match reg.repo_url(eco, package) {
            Ok(url) => hits.push((eco, url)),
            Err(e) => errs.push(format!("{eco}: {e}")),
        }
    }
    match hits.len() {
        0 => bail!(
            "`{package}` not found on crates.io, npm, or PyPI ({}); pass --eco or a git URL",
            errs.join("; ")
        ),
        1 => Ok(hits.into_iter().next().expect("checked len == 1")),
        _ => {
            if let Some(local) = local_ecosystem(cwd)
                && let Some(hit) = hits.iter().find(|(eco, _)| *eco == local)
            {
                return Ok(hit.clone());
            }
            let named = hits
                .iter()
                .map(|(eco, url)| format!("{eco} ({url})"))
                .collect::<Vec<_>>()
                .join(", ");
            bail!("`{package}` matches multiple ecosystems: {named}; pass --eco to disambiguate")
        }
    }
}

/// Pull the repository URL out of one registry's JSON response.
pub fn extract(eco: Ecosystem, v: &serde_json::Value) -> Result<String> {
    let raw = match eco {
        Ecosystem::Rust => v
            .pointer("/crate/repository")
            .and_then(|x| x.as_str())
            .context("crates.io response has no crate.repository")?,
        Ecosystem::Js => v
            .pointer("/repository/url")
            .and_then(|x| x.as_str())
            .context("npm response has no repository.url")?,
        Ecosystem::Python => {
            let urls = v
                .pointer("/info/project_urls")
                .and_then(|x| x.as_object())
                .context("PyPI response has no info.project_urls")?;
            ["Repository", "Source", "Source Code", "Code", "Homepage"]
                .iter()
                .filter_map(|k| urls.get(*k).and_then(|x| x.as_str()))
                .find(|u| u.contains("github.com") || u.contains("gitlab.com"))
                .context("PyPI project_urls has no recognizable repo link")?
        }
        Ecosystem::Git => bail!("git entries carry an explicit repo URL"),
    };
    let url = normalize(raw);
    if !url.contains("://") {
        bail!("`{url}` is not a full repo URL");
    }
    Ok(url)
}

pub fn normalize(raw: &str) -> String {
    let s = raw.trim().trim_start_matches("git+");
    let s = s.split('#').next().unwrap_or(s);
    s.trim_end_matches('/').to_string()
}

pub fn name_from_url(url: &str) -> String {
    url.trim_end_matches('/')
        .trim_end_matches(".git")
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(url)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Ecosystem;
    use serde_json::json;

    #[test]
    fn extract_per_registry_shapes() {
        let crates = json!({ "crate": { "repository": "https://github.com/tokio-rs/tokio" } });
        assert_eq!(
            extract(Ecosystem::Rust, &crates).unwrap(),
            "https://github.com/tokio-rs/tokio"
        );
        let npm = json!({ "repository": { "url": "git+https://github.com/facebook/react.git" } });
        assert_eq!(
            extract(Ecosystem::Js, &npm).unwrap(),
            "https://github.com/facebook/react.git"
        );
        let npm_str = json!({ "repository": "github-user/repo-shorthand-is-rejected" });
        assert!(extract(Ecosystem::Js, &npm_str).is_err());
        let pypi = json!({ "info": { "project_urls": { "Homepage": "https://requests.io", "Source": "https://github.com/psf/requests" } } });
        assert_eq!(
            extract(Ecosystem::Python, &pypi).unwrap(),
            "https://github.com/psf/requests"
        );
        assert!(extract(Ecosystem::Rust, &json!({})).is_err());
    }

    #[test]
    fn normalize_strips_git_plus_and_fragment() {
        assert_eq!(
            normalize("git+https://github.com/x/y.git"),
            "https://github.com/x/y.git"
        );
        assert_eq!(
            normalize("https://github.com/x/y#readme"),
            "https://github.com/x/y"
        );
        assert_eq!(
            normalize("https://github.com/x/y/"),
            "https://github.com/x/y"
        );
    }

    #[test]
    fn name_from_url_takes_repo_leaf() {
        assert_eq!(
            name_from_url("https://github.com/godotengine/godot"),
            "godot"
        );
        assert_eq!(name_from_url("git@github.com:tokio-rs/tokio.git"), "tokio");
        assert_eq!(name_from_url("https://github.com/x/y.git/"), "y");
    }

    #[test]
    fn detect_probes_ecosystems_in_order() {
        struct Stub;
        impl Registry for Stub {
            fn repo_url(&self, eco: Ecosystem, _p: &str) -> anyhow::Result<String> {
                match eco {
                    Ecosystem::Js => Ok("https://github.com/facebook/react".into()),
                    _ => anyhow::bail!("not found"),
                }
            }
        }
        let dir = std::env::temp_dir();
        let (eco, url) = detect(&Stub, "react", &dir).unwrap();
        assert_eq!(eco, Ecosystem::Js);
        assert_eq!(url, "https://github.com/facebook/react");

        struct Never;
        impl Registry for Never {
            fn repo_url(&self, _e: Ecosystem, _p: &str) -> anyhow::Result<String> {
                anyhow::bail!("nope")
            }
        }
        assert!(detect(&Never, "ghost", &dir).is_err());
    }

    #[test]
    fn a_name_in_two_registries_refuses_and_names_both() {
        struct Both;
        impl Registry for Both {
            fn repo_url(&self, eco: Ecosystem, _p: &str) -> anyhow::Result<String> {
                match eco {
                    Ecosystem::Rust => Ok("https://github.com/hyperium/h3".into()),
                    Ecosystem::Js => Ok("https://github.com/unjs/h3".into()),
                    _ => anyhow::bail!("not found"),
                }
            }
        }
        let dir = std::env::temp_dir().join(format!("docm-eco-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let err = detect(&Both, "h3", &dir).unwrap_err().to_string();
        assert!(err.contains("hyperium"), "{err}");
        assert!(err.contains("unjs"), "{err}");
        assert!(err.contains("--eco"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_js_project_reports_js_when_only_npm_has_the_name() {
        struct Npm;
        impl Registry for Npm {
            fn repo_url(&self, eco: Ecosystem, _p: &str) -> anyhow::Result<String> {
                match eco {
                    Ecosystem::Js => Ok("https://github.com/unjs/h3".into()),
                    _ => anyhow::bail!("not found"),
                }
            }
        }
        let dir = std::env::temp_dir().join(format!("docm-eco-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bun.lock"), "{}").unwrap();
        let (eco, _) = detect(&Npm, "h3", &dir).unwrap();
        assert_eq!(eco, Ecosystem::Js);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The brief's `a_js_project_reports_js_when_only_npm_has_the_name` fixture
    /// only ever produces one registry hit, so it passes with or without the
    /// cwd probe wired in — it does not discriminate local-first ordering.
    /// This variant makes Rust and Js both hit with different URLs so only a
    /// working local-project probe picks Js.
    #[test]
    fn a_js_project_resolves_ambiguity_to_js() {
        struct Both;
        impl Registry for Both {
            fn repo_url(&self, eco: Ecosystem, _p: &str) -> anyhow::Result<String> {
                match eco {
                    Ecosystem::Rust => Ok("https://github.com/hyperium/h3".into()),
                    Ecosystem::Js => Ok("https://github.com/unjs/h3".into()),
                    _ => anyhow::bail!("not found"),
                }
            }
        }
        let dir = std::env::temp_dir().join(format!("docm-eco-amb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bun.lock"), "{}").unwrap();
        let (eco, url) = detect(&Both, "h3", &dir).unwrap();
        assert_eq!(eco, Ecosystem::Js);
        assert_eq!(url, "https://github.com/unjs/h3");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A directory carrying markers for two ecosystems at once (an ordinary
    /// monorepo root) must not silently pick one by array order — that is
    /// exactly the ambiguous state the hard-error path exists for.
    #[test]
    fn a_polyglot_directory_does_not_resolve_ambiguity() {
        struct Both;
        impl Registry for Both {
            fn repo_url(&self, eco: Ecosystem, _p: &str) -> anyhow::Result<String> {
                match eco {
                    Ecosystem::Rust => Ok("https://github.com/hyperium/h3".into()),
                    Ecosystem::Js => Ok("https://github.com/unjs/h3".into()),
                    _ => anyhow::bail!("not found"),
                }
            }
        }
        let dir = std::env::temp_dir().join(format!("docm-eco-poly-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        std::fs::write(dir.join("package.json"), "{}").unwrap();
        let err = detect(&Both, "h3", &dir).unwrap_err().to_string();
        assert!(err.contains("hyperium"), "{err}");
        assert!(err.contains("unjs"), "{err}");
        assert!(err.contains("--eco"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
