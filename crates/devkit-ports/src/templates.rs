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
    required::{ensure_supplied, ensure_supplied_as, missing_args},
    template,
    vcs::{self, Vcs, VersionControl},
};
use devkit_config::{Config, RENAMED_TEMPLATES, Required, Templates};
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

const BUILT_INS: [BuiltIn; 12] = [
    BuiltIn {
        name: "branch",
        description: "Branch name `workspace setup` creates",
        source: Templates::branch,
    },
    BuiltIn {
        name: "worktree_dir",
        description: "Worktree directory name `workspace setup` creates",
        source: Templates::worktree_dir,
    },
    BuiltIn {
        name: "checkout_worktree_dir",
        description: "Worktree directory name `pr checkout` creates",
        source: Templates::checkout_worktree_dir,
    },
    BuiltIn {
        name: "issue_summary_path",
        description: "Where `workspace setup --summary` writes the summary",
        source: Templates::issue_summary_path,
    },
    BuiltIn {
        name: "issue_summary",
        description: "Summary file `workspace setup --summary` writes",
        source: Templates::issue_summary,
    },
    BuiltIn {
        name: "pr_title",
        description: "Title of a PR rendered by `pr render` or opened by `pr create`",
        source: Templates::pr_title,
    },
    BuiltIn {
        name: "pr_body",
        description: "Body of a PR rendered by `pr render` or opened by `pr create`",
        source: Templates::pr_body,
    },
    BuiltIn {
        name: "ticket_title",
        description: "Title of a ticket `ticket render`, `ticket create` or `ticket edit --title` writes",
        source: Templates::ticket_title,
    },
    BuiltIn {
        name: "ticket_body",
        description: "Body of a ticket `ticket render`, `ticket create` or `ticket edit` writes",
        source: Templates::ticket_body,
    },
    BuiltIn {
        name: "review_request",
        description: "Slack message sent by `pr review request`",
        source: Templates::review_request,
    },
    BuiltIn {
        name: "review_finish",
        description: "Slack message sent by `pr review finish`",
        source: Templates::review_finish,
    },
    BuiltIn {
        name: "commit_message",
        description: "Message `devkit commit` records",
        source: Templates::commit_message,
    },
];

/// The parts of a commit message, as `devkit commit`, `devkit template
/// render` and MCP `templates.render` take them for the `commit_message`
/// template.
#[derive(Debug, Clone, Copy, Default)]
pub struct CommitMessage<'a> {
    pub subject: Option<&'a str>,
    pub body: Option<&'a str>,
    pub coauthors: &'a [String],
}

impl CommitMessage<'_> {
    fn is_empty(&self) -> bool {
        self.subject.is_none() && self.body.is_none() && self.coauthors.is_empty()
    }
}

/// How a surface names the commit message parts it takes: by the `devkit
/// commit` flag, or by the MCP parameter of the same name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartNames {
    Flags,
    Params,
}

impl PartNames {
    /// The part `flag` supplies, as this surface spells it.
    fn part(self, flag: &str) -> String {
        match self {
            PartNames::Flags => flag.to_string(),
            PartNames::Params => format!("parameter `{}`", flag.trim_start_matches('-')),
        }
    }

    /// The template arg `name`, as this surface spells it.
    fn arg(self, name: &str) -> String {
        match self {
            PartNames::Flags => format!("--arg {name}"),
            PartNames::Params => format!("args.{name}"),
        }
    }

    /// The hint naming a missing part supplied by `flag`.
    fn missing(self, flag: &str) -> String {
        match self {
            PartNames::Flags => format!("{flag}=..."),
            PartNames::Params => self.part(flag),
        }
    }
}

const COMMIT_MESSAGE: &str = "commit_message";

/// Each part of a commit message, as the template variable that reads it and
/// the `devkit commit` flag that supplies it.
const COMMIT_PARTS: [(&str, &str); 3] = [
    ("subject", "--subject"),
    ("body", "--body"),
    ("coauthors", "--coauthor"),
];

/// The message `devkit commit` records: template `commit_message` over
/// [`checkout_context`], `given`, then the message's parts. A part the caller
/// left out falls back to `[templates.variables]`, which can require it, as a
/// project requires `coauthors` of agents; with no entry there it renders
/// empty.
pub fn commit_message(
    cfg: &Config,
    start: &Path,
    message: &CommitMessage<'_>,
    given: &BTreeMap<String, String>,
    caller: Caller,
) -> Result<String> {
    render_commit_parts(
        cfg,
        start,
        ("devkit commit", PartNames::Flags),
        message,
        given,
        caller,
    )
}

/// [`commit_message`] for `what`, naming a missing part as `what` takes it.
fn render_commit_parts(
    cfg: &Config,
    start: &Path,
    what: (&str, PartNames),
    message: &CommitMessage<'_>,
    given: &BTreeMap<String, String>,
    caller: Caller,
) -> Result<String> {
    let coauthors = message.coauthors.join("; ");
    let parts = [
        message.subject,
        message.body,
        (!coauthors.is_empty()).then_some(coauthors.as_str()),
    ];
    render_commit_message(cfg, start, parts, given, caller, what)
}

/// Whether `devkit commit` can leave out `part`: every part but the subject,
/// unless `[templates.variables]` declares it.
fn optional_part(cfg: &Config, part: &str) -> bool {
    part != "subject" && !cfg.templates.variables.contains_key(part)
}

/// [`commit_message`] with its parts in `parts`, in [`COMMIT_PARTS`] order,
/// on top of `given`. A missing part is named as `what` takes it, in a
/// refusal naming `what` as what needs it.
fn render_commit_message(
    cfg: &Config,
    start: &Path,
    parts: [Option<&str>; 3],
    given: &BTreeMap<String, String>,
    caller: Caller,
    (what, names): (&str, PartNames),
) -> Result<String> {
    let name = COMMIT_MESSAGE;
    let (_, _, source) = lookup(cfg, name)?;
    let reads = template::undeclared(&[source])?;
    for k in given.keys() {
        ensure!(
            reads.contains(k),
            "template `{name}` reads no variable `{k}`"
        );
    }
    let mut ctx = checkout_context(cfg, start);
    for (k, v) in given {
        ctx.insert(k.clone(), serde_json::json!(v));
    }
    for ((k, flag), v) in COMMIT_PARTS.into_iter().zip(parts) {
        if let Some(v) = v {
            ensure!(
                reads.contains(k),
                "template `{name}` reads no `{k}`, so it would drop {flag}"
            );
            ctx.insert(k.to_string(), serde_json::json!(v));
        }
    }
    let mut defaults = cfg.templates.defaults();
    let mut needed = args(source, &ctx)?;
    for (part, _) in COMMIT_PARTS {
        if optional_part(cfg, part) {
            needed.remove(part);
            defaults.insert(part.to_string(), String::new());
        }
    }
    ensure_supplied_as(
        what,
        &missing_args(cfg, None, &needed, given, caller),
        |m| match COMMIT_PARTS.iter().find(|(part, _)| *part == m.name) {
            Some((_, flag)) => m.hint_as(&names.missing(flag)),
            None => m.hint(),
        },
    )?;
    let text = template::render(source, &ctx, &defaults)
        .with_context(|| format!("rendering template `{name}`"))?;
    ensure!(
        !text.trim().is_empty(),
        "template `{name}` rendered an empty message"
    );
    Ok(text)
}

/// The context every template renders over: `branch`, and `ticket` (also
/// under its old name `issue`), `slug`, `apps` from the worktree's record
/// when it has one. A field with no source stays undefined rather than empty.
pub fn worktree_context(record: Option<&IssueRecord>, branch: Option<&str>) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    if let Some(b) = branch {
        m.insert("branch".into(), serde_json::json!(b));
    }
    if let Some(r) = record {
        m.insert("ticket".into(), serde_json::json!(r.issue));
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
    let mut rows = arg_rows(cfg, None, names, caller);
    if name == COMMIT_MESSAGE {
        for row in &mut rows {
            let Some((part, flag)) = COMMIT_PARTS.iter().find(|(part, _)| *part == row.name) else {
                continue;
            };
            if optional_part(cfg, part) {
                row.required = false;
                row.required_of = Required::Never;
                row.default = Some(String::new());
            }
            row.name = flag.to_string();
        }
    }
    Ok(Template {
        name: name.to_string(),
        kind,
        description,
        source: source.to_string(),
        args: rows,
    })
}

/// Render template `name` in the checkout holding `start`, with `given` on
/// top of [`checkout_context`] and `[templates.variables]` underneath. An arg
/// the template never reads, and a required one left out, are refused before
/// anything renders. No length limit such as `branch_max` is applied.
///
/// Template `commit_message` takes its parts from `parts`, named as `names`
/// say, and refuses them in `given`; any other template refuses a part.
pub fn render(
    cfg: &Config,
    start: &Path,
    name: &str,
    (parts, names): (&CommitMessage<'_>, PartNames),
    given: &BTreeMap<String, String>,
    caller: Caller,
) -> Result<String> {
    if name == COMMIT_MESSAGE {
        for (part, flag) in COMMIT_PARTS {
            ensure!(
                !given.contains_key(part),
                "template `{name}` takes `{part}` from {}, not {}",
                names.part(flag),
                names.arg(part)
            );
        }
        let what = format!("template `{name}`");
        return render_commit_parts(cfg, start, (&what, names), parts, given, caller);
    }
    let [subject, body, coauthor] = COMMIT_PARTS.map(|(_, flag)| names.part(flag));
    ensure!(
        parts.is_empty(),
        "{subject}, {body} and {coauthor} fill only template `{COMMIT_MESSAGE}`, not `{name}`"
    );
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
    let name = RENAMED_TEMPLATES
        .iter()
        .find_map(|(old, new)| (*old == name).then_some(*new))
        .unwrap_or(name);
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
        for name in ["ticket_title", "ticket_body"] {
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
