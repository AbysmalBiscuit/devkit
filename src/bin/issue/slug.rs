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
    fn from_linear_title_rejects_a_title_with_no_slug_left() {
        assert!(from_linear_title("ENG-1234", "!!!").is_err());
        assert!(from_linear_title("ENG-1234", "ENG-1234").is_err());
    }
}
