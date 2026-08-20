//! Branch/worktree slug derivation, shared by `setup` and `checkout-pr`.

use anyhow::{Context, Result};

/// Lowercase, collapse non-alphanumerics to single dashes, trim dashes.
pub(crate) fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.extend(c.to_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// Slug for `issue`, taken from its Linear title. The issue id is stripped from
/// the front so the branch template's `<issue>-<slug>` does not repeat it.
pub(crate) fn from_linear_title(issue: &str, title: &str) -> Result<String> {
    let id = slugify(issue);
    let slug = slugify(title);
    let trimmed = slug
        .strip_prefix(&id)
        .map_or(slug.as_str(), |rest| rest.trim_start_matches('-'));
    let out = trimmed.to_string();
    anyhow::ensure!(
        !out.is_empty(),
        "Linear title for {issue} slugifies to nothing (title: {title:?}) — pass --slug"
    );
    Ok(out)
}

/// An issue id from CLI input, plus the title slug when the input carried one.
pub(crate) struct IssueRef {
    pub(crate) id: String,
    /// The slug a Linear URL already spells out, so no title lookup is needed.
    pub(crate) slug: Option<String>,
}

/// Split CLI input into an issue id and, for a Linear URL, the title slug its
/// path spells out. A Linear issue URL is `…/issue/<ID>/<title-slug>`, so
/// pasting one supplies both without a network call. Anything else is taken
/// as a bare id; `issue_title_query` rejects a malformed one downstream.
pub(crate) fn parse_issue_ref(input: &str) -> IssueRef {
    let trimmed = input.trim();
    let bare = || IssueRef {
        id: trimmed.to_string(),
        slug: None,
    };
    if !trimmed.contains("linear.app") {
        return bare();
    }
    let path = trimmed.split_once('#').map_or(trimmed, |(head, _)| head);
    let path = path.split_once('?').map_or(path, |(head, _)| head);
    let mut segments = path
        .split('/')
        .skip_while(|s| !s.eq_ignore_ascii_case("issue"));
    let Some(id) = segments.nth(1).filter(|s| !s.is_empty()) else {
        return bare();
    };
    let slug = segments.next().map(slugify).filter(|s| !s.is_empty());
    IssueRef {
        id: id.to_string(),
        slug,
    }
}

/// Shorten `slug` to `budget` characters, dropping whole words so the result
/// still reads. A first word longer than the budget has no boundary to cut on
/// and is hard-cut instead.
pub(crate) fn cap(slug: &str, budget: usize) -> String {
    if slug.chars().count() <= budget {
        return slug.to_string();
    }
    let mut out = String::new();
    for word in slug.split('-') {
        let need = if out.is_empty() {
            word.chars().count()
        } else {
            out.chars().count() + 1 + word.chars().count()
        };
        if need > budget {
            break;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(word);
    }
    if out.is_empty() {
        return slug.chars().take(budget).collect();
    }
    out
}

/// The Linear API key, with the message a caller needs when it is missing.
pub(crate) fn linear_key() -> Result<String> {
    devkit_common::secrets::resolve("LINEAR_API_KEY")
        .context("no Linear API key — run `devkit auth linear`, or pass --slug")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_cleans_titles() {
        assert_eq!(slugify("Fix the Login!! page"), "fix-the-login-page");
        assert_eq!(slugify("  Trailing  "), "trailing");
        assert_eq!(slugify("ALL_CAPS"), "all-caps");
    }

    #[test]
    fn slugify_empty_and_all_special() {
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("!!!"), "");
    }

    #[test]
    fn from_linear_title_slugifies() {
        assert_eq!(
            from_linear_title("ENG-1234", "Fix BLI export").unwrap(),
            "fix-bli-export"
        );
    }

    #[test]
    fn from_linear_title_strips_a_leading_issue_id() {
        assert_eq!(
            from_linear_title("ENG-1234", "ENG-1234: fix BLI export").unwrap(),
            "fix-bli-export"
        );
        assert_eq!(
            from_linear_title("eng-1234", "ENG-1234 fix BLI export").unwrap(),
            "fix-bli-export"
        );
    }

    #[test]
    fn from_linear_title_keeps_an_unrelated_id_prefix() {
        assert_eq!(
            from_linear_title("ENG-1234", "OPS-7 fix BLI export").unwrap(),
            "ops-7-fix-bli-export"
        );
    }

    #[test]
    fn cap_keeps_a_short_slug_whole() {
        assert_eq!(cap("fix-bli-export", 32), "fix-bli-export");
        assert_eq!(cap("exactly-ten", 11), "exactly-ten");
    }

    #[test]
    fn cap_cuts_on_a_word_boundary() {
        // 34 chars; the budget lands mid-"entry", so the whole word goes.
        let slug = "api-delete-the-dead-flag-entry-etc";
        assert_eq!(cap(slug, 32), "api-delete-the-dead-flag-entry");
    }

    #[test]
    fn cap_never_leaves_a_trailing_dash() {
        assert_eq!(cap("one-two-three", 8), "one-two");
        assert!(!cap("alpha-beta-gamma", 11).ends_with('-'));
    }

    /// A first word past the budget has no boundary to cut on; a hard cut beats
    /// returning nothing.
    #[test]
    fn cap_hard_cuts_a_single_overlong_word() {
        assert_eq!(cap("supercalifragilistic", 8), "supercal");
    }

    #[test]
    fn parse_issue_ref_passes_a_bare_id_through() {
        let r = parse_issue_ref("  ENG-1234 ");
        assert_eq!(r.id, "ENG-1234");
        assert!(r.slug.is_none());
    }

    #[test]
    fn parse_issue_ref_takes_id_and_slug_from_a_url() {
        let r = parse_issue_ref("https://linear.app/acme/issue/ENG-1234/fix-bli-export");
        assert_eq!(r.id, "ENG-1234");
        assert_eq!(r.slug.as_deref(), Some("fix-bli-export"));
    }

    #[test]
    fn parse_issue_ref_url_without_a_slug_has_none() {
        for url in [
            "https://linear.app/acme/issue/ENG-1234",
            "https://linear.app/acme/issue/ENG-1234/",
        ] {
            let r = parse_issue_ref(url);
            assert_eq!(r.id, "ENG-1234", "{url}");
            assert!(r.slug.is_none(), "{url}");
        }
    }

    #[test]
    fn parse_issue_ref_drops_a_fragment_or_query() {
        for url in [
            "https://linear.app/acme/issue/ENG-1234/fix-bli-export#comment-9f2",
            "https://linear.app/acme/issue/ENG-1234/fix-bli-export?tab=activity",
        ] {
            let r = parse_issue_ref(url);
            assert_eq!(r.id, "ENG-1234", "{url}");
            assert_eq!(r.slug.as_deref(), Some("fix-bli-export"), "{url}");
        }
    }

    /// A workspace whose name ends in `-<digits>` looks exactly like an issue id,
    /// so the id has to come from the path position, not the first match.
    #[test]
    fn parse_issue_ref_is_not_fooled_by_a_workspace_named_like_an_id() {
        let r = parse_issue_ref("https://linear.app/acme-2/issue/ENG-1234/fix-bli-export");
        assert_eq!(r.id, "ENG-1234");
        assert_eq!(r.slug.as_deref(), Some("fix-bli-export"));
    }

    #[test]
    fn from_linear_title_rejects_a_title_with_no_slug_left() {
        assert!(from_linear_title("ENG-1234", "!!!").is_err());
        assert!(from_linear_title("ENG-1234", "ENG-1234").is_err());
    }
}
