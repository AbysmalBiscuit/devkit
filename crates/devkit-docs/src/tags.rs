//! Version → git tag probing. Repos tag `v1.2.3`, `1.2.3`, `pkg-1.2.3`,
//! `pkg-v1.2.3`, or `pkg@1.2.3`; a prior match helps preserve probe ordering.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TagPattern {
    PkgAt,
    LeafAt,
    PkgDashV,
    LeafDashV,
    LeafDash,
    V,
    Plain,
}

pub const ALL: [TagPattern; 7] = [
    TagPattern::PkgAt,
    TagPattern::LeafAt,
    TagPattern::PkgDashV,
    TagPattern::LeafDashV,
    TagPattern::LeafDash,
    TagPattern::V,
    TagPattern::Plain,
];

pub fn apply(p: TagPattern, package: &str, version: &str) -> String {
    let leaf = package.rsplit('/').next().unwrap_or(package);
    match p {
        TagPattern::PkgAt => format!("{package}@{version}"),
        TagPattern::LeafAt => format!("{leaf}@{version}"),
        TagPattern::PkgDashV => format!("{package}-v{version}"),
        TagPattern::LeafDashV => format!("{leaf}-v{version}"),
        TagPattern::LeafDash => format!("{leaf}-{version}"),
        TagPattern::V => format!("v{version}"),
        TagPattern::Plain => version.to_string(),
    }
}

pub fn find(tags: &[String], package: &str, version: &str) -> Option<(TagPattern, String)> {
    find_patterns(&ALL, tags, package, version)
}

pub(crate) fn find_with_hint(
    tags: &[String],
    package: &str,
    version: &str,
    hint: Option<TagPattern>,
) -> Option<(TagPattern, String)> {
    let Some(hint) = hint else {
        return find(tags, package, version);
    };
    let Some(hint_index) = ALL.iter().position(|pattern| *pattern == hint) else {
        return find(tags, package, version);
    };
    let (higher, hint_and_lower) = ALL.split_at(hint_index);

    find_patterns(higher, tags, package, version).or_else(|| {
        let tag = apply(hint, package, version);
        if tags.iter().any(|candidate| candidate == &tag) {
            Some((hint, tag))
        } else {
            find_patterns(&hint_and_lower[1..], tags, package, version)
        }
    })
}

fn find_patterns(
    patterns: &[TagPattern],
    tags: &[String],
    package: &str,
    version: &str,
) -> Option<(TagPattern, String)> {
    patterns
        .iter()
        .copied()
        .map(|p| (p, apply(p, package, version)))
        .find(|(_, t)| tags.iter().any(|x| x == t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_scoped_name_is_tried_before_the_leaf_and_before_generic() {
        let tags: Vec<String> = vec![
            "v0.13.1".into(),
            "client-fetch@0.13.1".into(),
            "@hey-api/client-fetch@0.13.1".into(),
        ];
        let (p, t) = find(&tags, "@hey-api/client-fetch", "0.13.1").unwrap();
        assert_eq!(p, TagPattern::PkgAt);
        assert_eq!(t, "@hey-api/client-fetch@0.13.1");
    }

    #[test]
    fn generic_patterns_still_match_when_nothing_specific_exists() {
        let tags: Vec<String> = vec!["v1.15.11".into()];
        let (p, t) = find(&tags, "h3", "1.15.11").unwrap();
        assert_eq!(p, TagPattern::V);
        assert_eq!(t, "v1.15.11");
        assert!(find(&tags, "h3", "9.9.9").is_none());
    }

    #[test]
    fn apply_renders_every_shape() {
        let pkg = "@hey-api/client-fetch";
        assert_eq!(
            apply(TagPattern::PkgAt, pkg, "0.13.1"),
            "@hey-api/client-fetch@0.13.1"
        );
        assert_eq!(
            apply(TagPattern::LeafAt, pkg, "0.13.1"),
            "client-fetch@0.13.1"
        );
        assert_eq!(
            apply(TagPattern::PkgDashV, pkg, "0.13.1"),
            "@hey-api/client-fetch-v0.13.1"
        );
        assert_eq!(
            apply(TagPattern::LeafDashV, pkg, "0.13.1"),
            "client-fetch-v0.13.1"
        );
        assert_eq!(
            apply(TagPattern::LeafDash, pkg, "0.13.1"),
            "client-fetch-0.13.1"
        );
        assert_eq!(apply(TagPattern::V, pkg, "0.13.1"), "v0.13.1");
        assert_eq!(apply(TagPattern::Plain, pkg, "0.13.1"), "0.13.1");
    }

    #[test]
    fn cached_generic_hint_does_not_beat_a_package_specific_tag() {
        let tags: Vec<String> = vec!["v0.13.1".into(), "@hey-api/client-fetch@0.13.1".into()];
        let (pattern, tag) = find_with_hint(
            &tags,
            "@hey-api/client-fetch",
            "0.13.1",
            Some(TagPattern::V),
        )
        .unwrap();
        assert_eq!(pattern, TagPattern::PkgAt);
        assert_eq!(tag, "@hey-api/client-fetch@0.13.1");
    }
}
