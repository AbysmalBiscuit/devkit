//! Version → git tag probing. Repos tag `v1.2.3`, `1.2.3`, `pkg-1.2.3`,
//! `pkg-v1.2.3`, or `pkg@1.2.3`; the first shape that matches is cached in
//! the lib's meta.toml so later resolutions skip the probe.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TagPattern {
    V,
    Plain,
    NameDash,
    NameDashV,
    NameAt,
}

pub const ALL: [TagPattern; 5] = [
    TagPattern::V,
    TagPattern::Plain,
    TagPattern::NameDash,
    TagPattern::NameDashV,
    TagPattern::NameAt,
];

pub fn apply(p: TagPattern, package: &str, version: &str) -> String {
    // Scoped npm packages tag by the leaf name: @scope/pkg → pkg@1.2.3.
    let leaf = package.rsplit('/').next().unwrap_or(package);
    match p {
        TagPattern::V => format!("v{version}"),
        TagPattern::Plain => version.to_string(),
        TagPattern::NameDash => format!("{leaf}-{version}"),
        TagPattern::NameDashV => format!("{leaf}-v{version}"),
        TagPattern::NameAt => format!("{leaf}@{version}"),
    }
}

pub fn find(tags: &[String], package: &str, version: &str) -> Option<(TagPattern, String)> {
    ALL.iter()
        .copied()
        .map(|p| (p, apply(p, package, version)))
        .find(|(_, t)| tags.iter().any(|x| x == t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_covers_all_shapes_and_uses_package_leaf() {
        assert_eq!(apply(TagPattern::V, "tokio", "1.38.0"), "v1.38.0");
        assert_eq!(apply(TagPattern::Plain, "tokio", "1.38.0"), "1.38.0");
        assert_eq!(
            apply(TagPattern::NameDash, "tokio", "1.38.0"),
            "tokio-1.38.0"
        );
        assert_eq!(
            apply(TagPattern::NameDashV, "tokio", "1.38.0"),
            "tokio-v1.38.0"
        );
        assert_eq!(
            apply(TagPattern::NameAt, "@types/node", "20.1.0"),
            "node@20.1.0"
        );
    }

    #[test]
    fn find_probes_in_order_and_returns_first_match() {
        let tags: Vec<String> = vec!["tokio-1.38.0".into(), "v9.9.9".into()];
        assert_eq!(
            find(&tags, "tokio", "1.38.0"),
            Some((TagPattern::NameDash, "tokio-1.38.0".into()))
        );
        assert_eq!(find(&tags, "tokio", "2.0.0"), None);
    }

    #[test]
    fn pattern_round_trips_through_toml() {
        #[derive(serde::Serialize, serde::Deserialize)]
        struct W {
            p: TagPattern,
        }
        let s = toml::to_string(&W {
            p: TagPattern::NameDashV,
        })
        .unwrap();
        assert_eq!(s.trim(), "p = \"name-dash-v\"");
        let w: W = toml::from_str(&s).unwrap();
        assert_eq!(w.p, TagPattern::NameDashV);
    }
}
