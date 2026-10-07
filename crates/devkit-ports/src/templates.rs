//! Named templates a caller renders and reads back instead of a command
//! rendering them: `[templates.custom]`, and the built-ins the issue commands
//! render. Shared by `devkit template` and MCP.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use anyhow::{Context, Result, anyhow, ensure};
use devkit_common::{
    caller::Caller,
    record::{self, IssueRecord},
    required::{ensure_supplied, missing_args},
    template,
    vcs::{self, Vcs, VersionControl},
};
use devkit_config::{Config, Templates};
use serde::Serialize;

use crate::task::{TaskArg, arg_rows};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    BuiltIn,
    Custom,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::BuiltIn => "built-in",
            Kind::Custom => "custom",
        }
    }
}

/// One named template and the args it takes from this worktree.
#[derive(Debug, Serialize)]
pub struct Template {
    pub name: String,
    pub kind: Kind,
    pub description: String,
    pub source: String,
    pub args: Vec<TaskArg>,
}

/// A built-in an issue command renders. Every context key the command
/// computes that [`checkout_context`] does not supply (`input`, `pr_url`,
/// `short_slug`, ...) is an arg here.
struct BuiltIn {
    name: &'static str,
    description: &'static str,
    source: fn(&Templates) -> &str,
}

const BUILT_INS: [BuiltIn; 11] = [
    BuiltIn {
        name: "branch",
        description: "Branch name `issue setup` creates",
        source: Templates::branch,
    },
    BuiltIn {
        name: "worktree_dir",
        description: "Worktree directory name `issue setup` creates",
        source: Templates::worktree_dir,
    },
    BuiltIn {
        name: "checkout_worktree_dir",
        description: "Worktree directory name `issue pr checkout` creates",
        source: Templates::checkout_worktree_dir,
    },
    BuiltIn {
        name: "issue_summary_path",
        description: "Where `issue setup --summary` writes the summary",
        source: Templates::issue_summary_path,
    },
    BuiltIn {
        name: "issue_summary",
        description: "Summary file `issue setup --summary` writes",
        source: Templates::issue_summary,
    },
    BuiltIn {
        name: "pr_title",
        description: "Title of a PR rendered by `issue pr render` or opened by `issue pr create`",
        source: Templates::pr_title,
    },
    BuiltIn {
        name: "pr_body",
        description: "Body of a PR rendered by `issue pr render` or opened by `issue pr create`",
        source: Templates::pr_body,
    },
    BuiltIn {
        name: "issue_title",
        description: "Title of an issue `issue render`, `issue create` or `issue edit --title` writes",
        source: Templates::issue_title,
    },
    BuiltIn {
        name: "issue_body",
        description: "Body of an issue `issue render`, `issue create` or `issue edit` writes",
        source: Templates::issue_body,
    },
    BuiltIn {
        name: "review_request",
        description: "Slack message sent by `issue review request`",
        source: Templates::review_request,
    },
    BuiltIn {
        name: "review_finish",
        description: "Slack message sent by `issue review finish`",
        source: Templates::review_finish,
    },
];

/// The context every template renders over: `branch`, and `issue`, `slug`,
/// `apps` from the worktree's record when it has one. A field with no source
/// stays undefined rather than empty.
pub fn worktree_context(record: Option<&IssueRecord>, branch: Option<&str>) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    if let Some(b) = branch {
        m.insert("branch".into(), serde_json::json!(b));
    }
    if let Some(r) = record {
        m.insert("issue".into(), serde_json::json!(r.issue));
        m.insert("slug".into(), serde_json::json!(r.slug));
        m.insert("apps".into(), serde_json::json!(r.apps));
    }
    serde_json::Value::Object(m)
}

/// What the checkout holding `start` supplies every template: `prefix` from
/// config, then [`worktree_context`]. A name it leaves out is an arg.
fn checkout_context(cfg: &Config, start: &Path) -> serde_json::Map<String, serde_json::Value> {
    let root = vcs::checkout_root(start).unwrap_or_else(|_| start.to_path_buf());
    let branch = Vcs::at(&root).branch(&root).ok();
    let serde_json::Value::Object(mut m) = worktree_context(
        record::read(&root)
            .filter(|r| branch.as_deref().is_none_or(|b| r.binds(b)))
            .as_ref(),
        branch.as_deref(),
    ) else {
        unreachable!("worktree_context builds an object")
    };
    m.insert(
        "prefix".into(),
        serde_json::json!(cfg.defaults.branch_prefix),
    );
    m
}

/// Every custom template by name, then every built-in no custom one shadows,
/// with the args each takes in the checkout holding `start`.
pub fn list(cfg: &Config, start: &Path, caller: Caller) -> Result<Vec<Template>> {
    let custom = cfg.templates.custom.keys().map(String::as_str);
    let built_in = BUILT_INS
        .iter()
        .map(|b| b.name)
        .filter(|n| !cfg.templates.custom.contains_key(*n));
    custom
        .chain(built_in)
        .map(|n| show(cfg, start, n, caller))
        .collect()
}

/// Template `name` with its source, and the args it takes in the checkout
/// holding `start`. A custom template wins over a built-in of the same name.
pub fn show(cfg: &Config, start: &Path, name: &str, caller: Caller) -> Result<Template> {
    let (kind, description, source) = lookup(cfg, name)?;
    let supplied = checkout_context(cfg, start);
    let names = args(source, &supplied).with_context(|| format!("reading template `{name}`"))?;
    Ok(Template {
        name: name.to_string(),
        kind,
        description,
        source: source.to_string(),
        args: arg_rows(cfg, None, names, caller),
    })
}

/// Render template `name` in the checkout holding `start`, with `given` on
/// top of [`checkout_context`] and `[templates.variables]` underneath. An arg
/// the template never reads, and a required one left out, are refused before
/// anything renders. No length limit such as `branch_max` is applied.
pub fn render(
    cfg: &Config,
    start: &Path,
    name: &str,
    given: &BTreeMap<String, String>,
    caller: Caller,
) -> Result<String> {
    let (_, _, source) = lookup(cfg, name)?;
    let reads = template::undeclared(&[source])?;
    for k in given.keys() {
        ensure!(
            reads.contains(k),
            "template `{name}` reads no variable `{k}`"
        );
    }
    let mut ctx = checkout_context(cfg, start);
    let missing = missing_args(cfg, None, &args(source, &ctx)?, given, caller);
    ensure_supplied(&format!("template `{name}`"), &missing)?;
    for (k, v) in given {
        ctx.insert(k.clone(), serde_json::json!(v));
    }
    template::render(source, &ctx, &cfg.templates.defaults())
        .with_context(|| format!("rendering template `{name}`"))
}

fn lookup<'a>(cfg: &'a Config, name: &str) -> Result<(Kind, String, &'a str)> {
    if let Some(c) = cfg.templates.custom.get(name) {
        return Ok((
            Kind::Custom,
            c.description.clone().unwrap_or_default(),
            &c.body,
        ));
    }
    BUILT_INS
        .iter()
        .find(|b| b.name == name)
        .map(|b| {
            (
                Kind::BuiltIn,
                b.description.to_string(),
                (b.source)(&cfg.templates),
            )
        })
        .ok_or_else(|| anyhow!("unknown template `{name}` (run `devkit template list`)"))
}

/// The names `source` reads that `supplied` does not hold.
fn args(
    source: &str,
    supplied: &serde_json::Map<String, serde_json::Value>,
) -> Result<BTreeSet<String>> {
    let mut names = template::undeclared(&[source])?;
    names.retain(|n| !supplied.contains_key(n));
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(body: &str) -> Config {
        Config::parse(&format!(
            "[defaults]\nworktree_root='w'\nbranch_prefix='x/'\nbaseline_ref='m'\n{body}"
        ))
        .unwrap()
    }

    #[test]
    fn a_custom_template_shadows_a_built_in_of_the_same_name() {
        let c = cfg("[templates.custom.pr_body]\nbody = 'mine {{ x }}'\n");
        let t = show(&c, Path::new("."), "pr_body", Caller::Agent).unwrap();
        assert_eq!(t.kind, Kind::Custom);
        assert_eq!(t.source, "mine {{ x }}");
        let names: Vec<String> = list(&c, Path::new("."), Caller::Agent)
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(
            names.iter().filter(|n| *n == "pr_body").count(),
            1,
            "{names:?}"
        );
    }

    #[test]
    fn the_issue_templates_are_built_ins() {
        let names: Vec<String> = list(&cfg(""), Path::new("."), Caller::Agent)
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect();
        for name in ["issue_title", "issue_body"] {
            assert_eq!(names.iter().filter(|n| *n == name).count(), 1, "{names:?}");
        }
    }

    #[test]
    fn what_the_checkout_supplies_is_no_arg_and_everything_else_is() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".devkit")).unwrap();
        std::fs::write(
            record::path(dir.path()),
            "issue = 'ENG-1'\nslug = 'fix'\napps = []\n",
        )
        .unwrap();
        let c = cfg(
            "[templates]\nreview_finish = '{{ prefix }}{{ issue }} {{ author }} {{ pr_url }}'\n",
        );
        let names = |start: &Path| -> Vec<String> {
            show(&c, start, "review_finish", Caller::Agent)
                .unwrap()
                .args
                .into_iter()
                .map(|a| a.name)
                .collect()
        };
        assert_eq!(names(dir.path()), ["author", "pr_url"]);

        let bare = tempfile::tempdir().unwrap();
        assert_eq!(
            names(bare.path()),
            ["author", "issue", "pr_url"],
            "with no record, `issue` is the caller's to pass"
        );
    }
}
