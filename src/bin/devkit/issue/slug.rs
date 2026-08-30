//! Branch/worktree slug derivation, shared by `setup` and `checkout-pr`.

use anyhow::{Context, Result};
use devkit_common::tracker::IssueRef;
use std::collections::BTreeMap;

pub(crate) use devkit_common::slug::slugify;

/// Slug for `issue`, taken from its tracker title. The issue id is stripped
/// from the front so the branch template's `<issue>-<slug>` does not repeat it.
pub(crate) fn from_title(issue: &str, title: &str) -> Result<String> {
    let id = slugify(issue);
    let slug = slugify(title);
    let trimmed = slug
        .strip_prefix(&id)
        .map_or(slug.as_str(), |rest| rest.trim_start_matches('-'));
    let out = trimmed.to_string();
    anyhow::ensure!(
        !out.is_empty(),
        "title for {issue} slugifies to nothing (title: {title:?}) — pass --slug"
    );
    Ok(out)
}

/// The id and title slug in a Linear issue URL's `…/issue/<ID>/<title-slug>`
/// path. `None` when there is no `issue/<ID>` pair to read.
///
/// Both values come from their path position. Scanning for the first
/// letters-dash-digits run instead would read a workspace named `acme-2` as
/// the issue id.
pub(crate) fn from_linear_url(url: &str) -> Option<IssueRef> {
    let path = url.trim();
    let path = path.split_once('#').map_or(path, |(head, _)| head);
    let path = path.split_once('?').map_or(path, |(head, _)| head);
    let mut segments = path
        .split('/')
        .skip_while(|s| !s.eq_ignore_ascii_case("issue"));
    let id = segments.nth(1).filter(|s| !s.is_empty())?;
    Some(IssueRef {
        id: id.to_string(),
        slug: segments.next().map(slugify).filter(|s| !s.is_empty()),
    })
}

/// Split CLI input into an issue id and, for a Linear URL, the title slug its
/// path spells out. Pasting a URL supplies both without a network call.
/// Anything else is taken as a bare id.
///
/// Used only for a project whose tracker was not declared: a declared
/// tracker's own `issue_ref` owns parsing completely, and this permissive
/// linear.app read — which needs no key — would otherwise be lost for a
/// project that configured no tracker.
pub(crate) fn parse_issue_ref(input: &str) -> IssueRef {
    let trimmed = input.trim();
    if trimmed.contains("linear.app")
        && let Some(parsed) = from_linear_url(trimmed)
    {
        return parsed;
    }
    IssueRef {
        id: trimmed.to_string(),
        slug: None,
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

/// Characters each name in `names` may spend before `template` renders longer
/// than `max`.
///
/// The fixed cost is measured rather than assumed. The template renders twice
/// against the real context, once with every name one character long and once
/// with two, so the difference in length counts how many times those names are
/// rendered in total. A template that renders none of them constrains nothing.
///
/// `floor` carries the overflow policy, because the two kinds of limit want
/// opposite answers when the fixed text leaves no room. `Some(n)` clamps up to
/// `n` and cannot fail, which suits a limit on a display width. `None` fails,
/// which suits a limit on a filesystem path, where a cap that silently does not
/// hold is the whole problem.
pub(crate) fn budget(
    template: &str,
    ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
    names: &[&str],
    max: usize,
    floor: Option<usize>,
) -> Result<usize> {
    let probe = |len: usize| -> Result<usize> {
        let mut probe_ctx = ctx.clone();
        let obj = probe_ctx
            .as_object_mut()
            .context("template probe context is not an object")?;
        for name in names {
            obj.insert(
                (*name).to_string(),
                serde_json::Value::String("x".repeat(len)),
            );
        }
        Ok(devkit_common::template::render(template, &probe_ctx, vars)?
            .trim()
            .chars()
            .count())
    };
    let one = probe(1)?;
    let occurrences = probe(2)?.saturating_sub(one);
    if occurrences == 0 {
        return Ok(usize::MAX);
    }
    let fixed = one.saturating_sub(occurrences);
    let per_name = max.saturating_sub(fixed) / occurrences;
    match floor {
        Some(n) => Ok(per_name.max(n)),
        None if per_name == 0 => anyhow::bail!(
            "`{template}` renders {fixed} characters of fixed text, \
             which leaves no room within a limit of {max}"
        ),
        None => Ok(per_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn novars() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    #[test]
    fn budget_subtracts_measured_fixed_text() {
        let ctx = json!({"prefix": "lev/", "slug": ""});
        // "lev/" is four characters the slug does not get.
        let b = budget(
            "{{ prefix }}{{ slug }}",
            &ctx,
            &novars(),
            &["slug"],
            46,
            Some(12),
        )
        .unwrap();
        assert_eq!(b, 42);
    }

    #[test]
    fn budget_is_unconstrained_when_the_template_omits_the_name() {
        let ctx = json!({"issue": "142", "slug": ""});
        let b = budget("{{ issue }}", &ctx, &novars(), &["slug"], 46, None).unwrap();
        assert_eq!(b, usize::MAX);
    }

    #[test]
    fn budget_halves_for_a_name_rendered_twice() {
        let ctx = json!({"slug": ""});
        let b = budget("{{ slug }}{{ slug }}", &ctx, &novars(), &["slug"], 40, None).unwrap();
        assert_eq!(b, 20);
    }

    #[test]
    fn budget_splits_between_two_names() {
        let ctx = json!({"pr_title": "", "linear_title": ""});
        // One dash of fixed text, 40 characters left, two names.
        let b = budget(
            "{{ pr_title }}-{{ linear_title }}",
            &ctx,
            &novars(),
            &["pr_title", "linear_title"],
            41,
            None,
        )
        .unwrap();
        assert_eq!(b, 20);
    }

    /// The default `checkout_worktree_dir` wraps a conditional around the
    /// tracker id, so the fixed cost depends on whether one resolved. Probing
    /// against the real context is what makes that measurable.
    #[test]
    fn budget_measures_a_conditional_block_against_the_real_context() {
        let tmpl = "{{ pr_number }}-{{ pr_title }}{% if linear_id %}_[{{ linear_id }}]{% endif %}";
        let with = json!({"pr_number": 142, "pr_title": "", "linear_id": "ENG-1234"});
        let without = json!({"pr_number": 142, "pr_title": "", "linear_id": ""});
        // "142-" is 4; "_[ENG-1234]" adds 11 more.
        assert_eq!(
            budget(tmpl, &with, &novars(), &["pr_title"], 46, None).unwrap(),
            31
        );
        assert_eq!(
            budget(tmpl, &without, &novars(), &["pr_title"], 46, None).unwrap(),
            42
        );
    }

    #[test]
    fn budget_clamps_up_to_a_floor() {
        let ctx = json!({"prefix": "a-very-long-branch-prefix-indeed/", "slug": ""});
        // 33 characters of prefix against a limit of 36 leaves 3, below the floor.
        let b = budget(
            "{{ prefix }}{{ slug }}",
            &ctx,
            &novars(),
            &["slug"],
            36,
            Some(12),
        )
        .unwrap();
        assert_eq!(b, 12);
    }

    #[test]
    fn budget_without_a_floor_errors_when_fixed_text_fills_the_limit() {
        let ctx = json!({"pr_number": 142, "pr_title": ""});
        let err = budget(
            "worktree-for-pr-{{ pr_number }}-{{ pr_title }}",
            &ctx,
            &novars(),
            &["pr_title"],
            16,
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("20"), "{err}");
        assert!(err.contains("16"), "{err}");
    }

    #[test]
    fn from_title_slugifies() {
        assert_eq!(
            from_title("ENG-1234", "Fix BLI export").unwrap(),
            "fix-bli-export"
        );
    }

    #[test]
    fn from_title_strips_a_leading_issue_id() {
        assert_eq!(
            from_title("ENG-1234", "ENG-1234: fix BLI export").unwrap(),
            "fix-bli-export"
        );
        assert_eq!(
            from_title("eng-1234", "ENG-1234 fix BLI export").unwrap(),
            "fix-bli-export"
        );
    }

    #[test]
    fn from_title_keeps_an_unrelated_id_prefix() {
        assert_eq!(
            from_title("ENG-1234", "OPS-7 fix BLI export").unwrap(),
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
    fn from_title_rejects_a_title_with_no_slug_left() {
        assert!(from_title("ENG-1234", "!!!").is_err());
        assert!(from_title("ENG-1234", "ENG-1234").is_err());
    }
}
