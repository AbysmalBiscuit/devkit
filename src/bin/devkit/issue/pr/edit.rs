//! `pr edit`: rewrite the parts of an open PR's title and body this run
//! passes, leaving the rest of the live body as it is.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result, bail};
use devkit_common::{
    forge::{self, PrEdit},
    progress::Steps,
    record::IssueRecord,
    vcs::{Vcs, VersionControl},
};
use devkit_config::Templates;

use super::{
    create::{attached_file, check_attachments},
    proof::gate_proof,
    render::{Texts, missing, require_pr_title},
    require_existing_pr,
    resolve::{Existing, assert_belongs, parse_pr_flag, record_pr, resolve_existing},
    splice::{slot, splice},
};
use crate::{
    issue::review::{guard_branch, parse_args},
    template::VarArgs,
};

pub struct Args {
    pub pr_title: Option<String>,
    pub pr_body: Option<String>,
    pub attach: Vec<String>,
    pub no_push: bool,
    pub pr: Option<String>,
    pub vars: VarArgs,
    pub dir: Option<String>,
    pub config: Option<String>,
}

/// Refuse a file no edited text embeds: gh would append it below everything
/// else in the body, outside every section.
fn require_embedded(dir: &Path, attach: &[String], edits: &BTreeMap<String, String>) -> Result<()> {
    for value in attach {
        let path = attached_file(dir, value).unwrap_or(value);
        let embed = format!("]({path})");
        if !edits.values().any(|text| text.contains(&embed)) {
            bail!(
                "--attach {path}: nothing this run edits embeds it, and gh would append it \
                 to the end of the body. Embed it under the line it proves: ![alt]({path})"
            );
        }
    }
    Ok(())
}

/// The `pr_body` template rendered with every variable as its slot.
fn skeleton(
    tmpls: &Templates,
    record: Option<&IssueRecord>,
    branch: &str,
    toplevel: &Path,
) -> Result<String> {
    let slots = tmpls.declared().into_iter().map(|n| {
        let s = slot(&n);
        (n, s)
    });
    Texts::new(
        tmpls,
        record,
        branch,
        toplevel,
        slots.collect(),
        None,
        Some(slot("input")),
    )
    .body(&slot("pr_title"))
}

pub fn run(args: Args) -> Result<()> {
    let start = args.dir.clone().unwrap_or_else(|| ".".to_string());
    let here = Path::new(&start);
    let loaded = devkit_ports::load::load(args.config.as_deref().map(Path::new), here)?;
    let tmpls = &loaded.config.templates;
    let forge = forge::resolve(&loaded.config.forge, &loaded.config.github, &start, None);
    if !forge.forge.edits_prs() {
        bail!(
            "pr edit needs a GitHub forge; this project's forge is {}",
            forge.forge.kind()
        );
    }
    check_attachments(forge.forge.as_ref(), here, &args.attach)?;

    let mut edits = parse_args(&args.vars, &tmpls.declared())?;
    if let Some(body) = args.pr_body {
        edits.insert("input".into(), body);
    }
    if edits.is_empty() && args.pr_title.is_none() {
        bail!("nothing to edit: pass --pr-title, --pr-body, or --arg for a part of the body");
    }
    let caller = devkit_common::caller::caller();
    let blanked: Vec<_> = missing(&loaded.config, &edits, caller)?
        .into_iter()
        .filter(|m| edits.contains_key(&m.name))
        .collect();
    devkit_common::required::ensure_supplied("pr edit", &blanked)?;
    require_embedded(here, &args.attach, &edits)?;

    let vcs = Vcs::at(here);
    let branch = vcs.branch(here)?;
    guard_branch(&branch)?;
    let toplevel = devkit_common::vcs::checkout_root(here)?;
    let record = devkit_common::record::read_on(&toplevel, &branch);

    if let Some(variable) = &loaded.config.defaults.pr_proof_variable
        && edits.contains_key(variable)
    {
        gate_proof(
            &loaded.config,
            here,
            &forge.repos,
            record.as_ref().and_then(|r| r.tracker_issue()).as_deref(),
            caller,
            &edits,
        )?;
    }

    let title = args
        .pr_title
        .map(|input| {
            let mut values = tmpls.defaults();
            values.extend(edits.clone());
            let title = Texts::new(
                tmpls,
                record.as_ref(),
                &branch,
                &toplevel,
                values,
                Some(input),
                None,
            )
            .title()?;
            require_pr_title(&title)?;
            Ok::<_, anyhow::Error>(title)
        })
        .transpose()?;

    let head = vcs.revision(here)?;
    let steps = Steps::persistent();
    let found = resolve_existing(&Existing {
        start: &start,
        branch: &branch,
        forge: &forge,
        record: record.as_ref(),
        explicit_pr: args
            .pr
            .as_deref()
            .map(|s| parse_pr_flag(s, forge.forge.as_ref()))
            .transpose()?,
        no_push: args.no_push,
        steps: &steps,
    })?;
    require_existing_pr(found.pr.as_ref().map(|p| p.state.as_str()))?;
    let pr = found.pr.expect("require_existing_pr rejects a missing PR");
    let locator = found.locator.expect("a resolved PR carries a locator");
    assert_belongs(&pr, &head)?;
    if record.is_some() {
        record_pr(&toplevel, locator)?;
    }

    let f = forge.forge.as_ref();
    let body = if edits.is_empty() {
        None
    } else {
        let skeleton = skeleton(tmpls, record.as_ref(), &branch, &toplevel)?;
        let mut body = steps.during_result("Reading the PR's body...", || {
            f.pr_body(&found.repo, pr.number)
        })?;
        for (name, value) in &edits {
            body = splice(&skeleton, &body, name, value)
                .with_context(|| format!("placing `{name}` in PR #{}'s body", pr.number))?;
        }
        Some(body)
    };

    let edit = PrEdit {
        title: title.as_deref(),
        body: body.as_deref(),
        attachments: &args.attach,
    };
    steps.during_result("Editing PR...", || {
        f.edit(&found.repo, pr.number, &edit, here)
    })?;
    println!("{}", pr.url);
    Ok(())
}
