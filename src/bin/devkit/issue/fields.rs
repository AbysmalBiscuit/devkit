//! `ticket fields`: the ticket fields this repository declares for its
//! tracker, at their level for this caller. It reads config only.

use std::path::Path;

use anyhow::Result;
use devkit_common::fields::{FieldView, listing};
use devkit_config::{Cond, TrackerKind};

use super::render;
use crate::template::VarArgs;

pub(crate) struct FieldsArgs {
    pub json: bool,
    pub vars: VarArgs,
    pub dir: Option<String>,
    pub config: Option<String>,
}

pub(crate) fn run(args: FieldsArgs) -> Result<()> {
    let start = super::start(&args.dir);
    let (cfg, _) =
        devkit_common::config::resolve(args.config.as_deref().map(Path::new), Path::new(&start))?;
    let kind = match render::fields_tracker(&cfg, None)? {
        TrackerKind::None if cfg.tracker.kind.is_none() => {
            devkit_common::tracker::select(args.config.as_deref().map(Path::new), &start, None)
                .tracker
                .tracker
                .kind()
        }
        kind => kind,
    };
    let decls = cfg.ticket.fields.for_tracker(kind);
    let input = render::ticket_args(&cfg, &args.vars, &Default::default(), decls)?;
    let views = listing(decls, devkit_common::caller::caller(), &input.fields);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&views)?);
    } else if views.is_empty() {
        println!("no ticket fields declared for {kind} ([ticket.fields.{kind}])");
    } else {
        print!("{}", text(&views));
    }
    Ok(())
}

fn cond_text(key: &str, cond: &Cond) -> String {
    let show = |v: &String| match v.as_str() {
        "" => "unset".to_string(),
        "*" => "set".to_string(),
        v => v.to_string(),
    };
    format!(
        "{key} {}",
        cond.options()
            .iter()
            .map(show)
            .collect::<Vec<_>>()
            .join("|")
    )
}

/// One block per field: its name, type and level, then what an agent needs
/// to pass it.
fn text(views: &[FieldView]) -> String {
    let mut out = String::new();
    for v in views {
        let ty = v.r#type.map_or("?", |t| t.as_str());
        let list = if v.list { " list" } else { "" };
        out.push_str(&format!("{}  {ty}{list}  {}\n", v.name, v.level.as_str()));
        if let Some(d) = &v.description {
            out.push_str(&format!("  {d}\n"));
        }
        if !v.values.is_empty() {
            out.push_str(&format!("  values: {}\n", v.values.join(", ")));
        }
        if let Some(d) = &v.default {
            out.push_str(&format!("  default: {d}\n"));
        }
        for w in &v.when {
            let conds: Vec<String> = w.r#if.iter().map(|(k, c)| cond_text(k, c)).collect();
            out.push_str(&format!(
                "  when {}: {}\n",
                conds.join(" and "),
                w.level.as_str()
            ));
        }
        let none = if v.allows_none {
            format!(" or --none {}", v.name)
        } else {
            String::new()
        };
        out.push_str(&format!("  pass: --arg {}=...{none}\n", v.name));
    }
    out
}
