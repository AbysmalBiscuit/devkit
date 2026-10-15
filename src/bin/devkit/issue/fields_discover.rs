//! `ticket fields discover`: read the tracker once and write the ticket
//! field declarations it offers into the repository's `devkit.toml`, keeping
//! what a person set there.

use std::{
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use devkit_common::{
    caller::Caller,
    tracker::{
        discover::{Discovered, DiscoveredField},
        github_discover, linear_discover,
    },
};
use devkit_config::{
    CALLER_CONDITION, Config, EnumValue, FieldDecl, FieldType, ISSUE_CONTEXT_KEYS, LayerKind,
    PendingLayer, TrackerKind,
};
use serde::Serialize;
use toml_edit::{Array, DocumentMut, Item, Table, TableLike, Value};

use super::field_write;

pub(crate) struct DiscoverArgs {
    pub yes: bool,
    pub teams: Vec<String>,
    pub dir: Option<String>,
    pub config: Option<String>,
}

/// One change `merge` made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Change {
    /// A field no layer declared, added at `info`.
    Added(String),
    /// A declared field whose `type`, `list`, `where` or `values` changed.
    Refreshed(String),
    /// A declared value the tracker no longer offers.
    ValueDropped { field: String, value: String },
    /// A declared value kept though the tracker did not answer it, since it
    /// answered that list only in part.
    ValueKept { field: String, value: String },
    /// A tracker field not added, because its name is declared for another
    /// place, or is a template variable or a name devkit supplies.
    NameTaken { field: String, place: String },
}

pub(crate) fn run(args: DiscoverArgs) -> Result<()> {
    let start = super::start(&args.dir);
    let config = args.config.as_deref().map(Path::new);
    let mut sel = devkit_common::tracker::select(config, &start, None);
    let cfg = sel
        .config
        .take()
        .context("`ticket fields discover` needs a loadable devkit.toml")?;
    let kind = cfg
        .tracker
        .kind
        .unwrap_or_else(|| sel.tracker.tracker.kind());
    if !args.teams.is_empty() && kind != TrackerKind::Linear {
        bail!("--team names Linear teams, and this project's tracker is {kind}");
    }
    let found = match kind {
        TrackerKind::Github => github_discover::fetch(
            sel.forge.repos.issues()?,
            cfg.github.project.as_ref(),
            cfg.github.status_field(),
            &cfg.people,
        )?,
        TrackerKind::Linear => {
            linear_discover::fetch(&field_write::linear_key()?, &args.teams, &cfg.people)?
        }
        TrackerKind::None => bail!(
            "`ticket fields discover` reads a GitHub or Linear tracker, and this project has \
             none. Set `[tracker] kind` in devkit.toml."
        ),
    };
    let path = target(config, Path::new(&start))?;
    let before = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut doc: DocumentMut = before
        .parse()
        .with_context(|| format!("parsing {}", path.display()))?;
    let changes = merge(&mut doc, kind, &cfg, &found)?;
    let after = doc.to_string();
    print!("{}", summary(kind, &changes, &found));
    if after == before {
        println!(
            "{} already declares every field the tracker offers",
            path.display()
        );
        return Ok(());
    }
    refuse_unloadable(config, Path::new(&start), kind, &path, &after)?;
    if !approved(&path, &before, &after, args.yes)? {
        println!("nothing written");
        return Ok(());
    }
    std::fs::write(&path, &after).with_context(|| format!("writing {}", path.display()))?;
    println!("wrote {}; review it with `git diff`", path.display());
    Ok(())
}

/// The file discover writes: `--config`, else `$DEVKIT_CONFIG`, each the
/// one file the run read, else the nearest tracked `devkit.toml`, else a new
/// one beside the nearest layer inside the checkout, which a `[config] root =
/// true` barrier leaves standing where the checkout root's file may be cut
/// off, else a new one at the checkout root.
fn target(config: Option<&Path>, start: &Path) -> Result<PathBuf> {
    if let Some(p) = config
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("DEVKIT_CONFIG").map(PathBuf::from))
    {
        return Ok(p);
    }
    let main = devkit_common::vcs::main_checkout(start).ok().flatten();
    let layers = devkit_config::project_layers(start, main.as_deref())?;
    if let Some(p) = devkit_config::nearest_tracked(&layers) {
        return Ok(p.to_path_buf());
    }
    let root = devkit_common::vcs::checkout_root(start)?;
    let inside = |dir: &Path| match (dir.canonicalize(), root.canonicalize()) {
        (Ok(dir), Ok(root)) => dir.starts_with(root),
        _ => false,
    };
    let nearest = layers
        .iter()
        .rev()
        .find(|l| l.kind != LayerKind::MainCheckout)
        .and_then(|l| l.path.parent())
        .filter(|dir| inside(dir));
    let dir = nearest.map_or(root, Path::to_path_buf);
    Ok(dir.join(devkit_config::CONFIG_FILE))
}

/// Refuse to write `after` to `path` when the config would then fail to
/// load, or fail the field checks every ticket command runs on `kind`: an
/// error-level `enum` left with no values, say.
fn refuse_unloadable(
    config: Option<&Path>,
    start: &Path,
    kind: TrackerKind,
    path: &Path,
    after: &str,
) -> Result<()> {
    let pending = PendingLayer { path, text: after };
    devkit_common::config::resolve_pending(config, start, pending)
        .and_then(|(cfg, provenance)| {
            devkit_config::check_ticket_fields(&cfg, kind, &provenance.origin)
        })
        .with_context(|| {
            format!(
                "not writing {}: the config would no longer load with the tracker's fields",
                path.display()
            )
        })
}

/// Whether to write: an agent always writes, a human after `--yes` or a
/// `y` at the prompt, which needs a terminal.
fn approved(path: &Path, before: &str, after: &str, yes: bool) -> Result<bool> {
    if devkit_common::caller::caller() == Caller::Agent || yes {
        return Ok(true);
    }
    print!("{}", diff(path, before, after));
    if !std::io::stdin().is_terminal() {
        bail!(
            "not writing {}: there is no terminal to confirm the change above on. Pass --yes to \
             write it",
            path.display()
        );
    }
    print!("write these changes to {}? [y/N] ", path.display());
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

fn diff(path: &Path, before: &str, after: &str) -> String {
    let name = path.display().to_string();
    similar::TextDiff::from_lines(before, after)
        .unified_diff()
        .header(&name, &name)
        .to_string()
}

/// What changed, the users no `[people]` entry names, and the lists the
/// tracker answered only in part.
fn summary(kind: TrackerKind, changes: &[Change], found: &Discovered) -> String {
    let mut out = String::new();
    for c in changes {
        out.push_str(&match c {
            Change::Added(f) => format!("added [ticket.fields.{kind}.{f}] at level \"info\"\n"),
            Change::Refreshed(f) => format!("refreshed [ticket.fields.{kind}.{f}]\n"),
            Change::ValueDropped { field, value } => format!(
                "dropped `{value}` from [ticket.fields.{kind}.{field}]: the tracker no longer \
                 offers it\n"
            ),
            Change::ValueKept { field, value } => format!(
                "kept `{value}` in [ticket.fields.{kind}.{field}] unchecked: the tracker's list \
                 was read only in part\n"
            ),
            Change::NameTaken { field, place } => format!(
                "not added: the tracker's `{place}` would be [ticket.fields.{kind}.{field}], \
                 whose name is taken by another field, a template variable or devkit; declare \
                 it under another name by hand\n"
            ),
        });
    }
    if !found.unmatched_users.is_empty() {
        out.push_str(&format!(
            "tracker users no [people] entry names (add `{key} = \"...\"` under a \
             [people.<alias>] entry to pass them as `user` values):\n",
            key = found.people_key
        ));
        for u in &found.unmatched_users {
            out.push_str(&format!("  {u}\n"));
        }
    }
    for f in &found.unwritable {
        out.push_str(&format!(
            "not written: the tracker's {f}: devkit cannot set a field of that type\n"
        ));
    }
    for t in &found.truncated {
        out.push_str(&format!("note: {t}\n"));
    }
    out
}

/// Write `found` into `doc`'s `[ticket.fields.<kind>]`. A field any layer of
/// `merged` declares at the same place, or by the same name with no place,
/// has its `type`, `list`, `where` and `values` refreshed, keeping each
/// value's declared name; nothing else of it is written. Any other field is
/// added at `level = "info"`. Comments and layout survive.
pub(crate) fn merge(
    doc: &mut DocumentMut,
    kind: TrackerKind,
    merged: &Config,
    found: &Discovered,
) -> Result<Vec<Change>> {
    let declared = merged.ticket.fields.for_tracker(kind);
    let variables = merged.templates.declared();
    let taken = |name: &str| {
        declared.contains_key(name)
            || variables.contains(name)
            || ISSUE_CONTEXT_KEYS.contains(&name)
            || name == CALLER_CONDITION
    };
    let mut changes = Vec::new();
    for (name, field) in &found.fields {
        let by_place = declared.iter().find(|(_, d)| {
            d.r#where
                .as_ref()
                .is_some_and(|w| w.same_place(&field.r#where))
        });
        let by_name = || {
            declared
                .get_key_value(name)
                .filter(|(_, d)| d.r#where.is_none())
        };
        match by_place.or_else(by_name) {
            Some((target, decl)) => {
                let values = refreshed_values(decl, field, &found.teams_read);
                for unseen in decl
                    .values
                    .iter()
                    .filter(|v| !field.values.iter().any(|n| n.id == v.id))
                {
                    let (field_name, value) = (target.clone(), unseen.name.clone());
                    match values.iter().any(|v| v.id == unseen.id) {
                        false => changes.push(Change::ValueDropped {
                            field: field_name,
                            value,
                        }),
                        true if field.partial => changes.push(Change::ValueKept {
                            field: field_name,
                            value,
                        }),
                        true => {}
                    }
                }
                let same = decl.r#type == Some(field.r#type)
                    && decl.list == field.list
                    && decl.r#where.as_ref() == Some(&field.r#where)
                    && (field.r#type != FieldType::Enum || decl.values == values);
                if same {
                    continue;
                }
                write_facts(entry(doc, kind, target)?, field, &values)?;
                changes.push(Change::Refreshed(target.clone()));
            }
            None if taken(name) => changes.push(Change::NameTaken {
                field: name.clone(),
                place: field.r#where.label(),
            }),
            None => {
                let t = entry(doc, kind, name)?;
                write_facts(t, field, &field.values)?;
                t.insert("level", toml_edit::value("info"));
                changes.push(Change::Added(name.clone()));
            }
        }
    }
    Ok(changes)
}

/// `decl`'s values refreshed from `field`, matched by id: each value the
/// tracker offers, under the name `decl` gives it, then each declared value
/// it did not answer that the read could not judge. A read covering only
/// the teams `teams_read` replaces a value's teams within them and keeps its
/// teams outside them, and keeps a declared value it did not see while it
/// is valid in a team outside them. A read of `field` in part keeps every
/// declared value it did not answer.
fn refreshed_values(
    decl: &FieldDecl,
    field: &DiscoveredField,
    teams_read: &[String],
) -> Vec<EnumValue> {
    let outside = |d: &EnumValue| -> Vec<String> {
        d.teams
            .iter()
            .filter(|t| !teams_read.is_empty() && !teams_read.contains(t))
            .cloned()
            .collect()
    };
    let mut out: Vec<EnumValue> = field
        .values
        .iter()
        .map(|v| match decl.values.iter().find(|d| d.id == v.id) {
            Some(d) => {
                let mut teams = v.teams.clone();
                if !teams.is_empty() {
                    teams.extend(outside(d).into_iter().filter(|t| !v.teams.contains(t)));
                }
                EnumValue {
                    name: d.name.clone(),
                    teams,
                    ..v.clone()
                }
            }
            None => v.clone(),
        })
        .collect();
    let unseen: Vec<EnumValue> = decl
        .values
        .iter()
        .filter(|d| !out.iter().any(|v| v.id == d.id))
        .filter_map(|d| match outside(d) {
            _ if field.partial => Some(d.clone()),
            teams if teams.is_empty() => None,
            teams => Some(EnumValue { teams, ..d.clone() }),
        })
        .collect();
    out.extend(unseen);
    out
}

/// The table `[ticket.fields.<kind>.<name>]` of `doc`, created when absent
/// with its parents left implicit.
fn entry<'a>(
    doc: &'a mut DocumentMut,
    kind: TrackerKind,
    name: &str,
) -> Result<&'a mut dyn TableLike> {
    let mut at: &mut dyn TableLike = doc.as_table_mut();
    for (key, implicit) in [
        ("ticket", true),
        ("fields", true),
        (kind.as_str(), true),
        (name, false),
    ] {
        if !at.contains_key(key) {
            let mut t = Table::new();
            t.set_implicit(implicit);
            at.insert(key, Item::Table(t));
        }
        at = at
            .get_mut(key)
            .and_then(Item::as_table_like_mut)
            .with_context(|| format!("`{key}` in devkit.toml is not a table"))?;
    }
    Ok(at)
}

/// Set `key` to `v`, keeping the decor (a trailing comment, say) of the
/// value it replaces.
fn set(t: &mut dyn TableLike, key: &str, mut v: Value) {
    match t.get_mut(key) {
        Some(Item::Value(old)) => {
            *v.decor_mut() = old.decor().clone();
            *old = v;
        }
        _ => {
            t.insert(key, Item::Value(v));
        }
    }
}

fn to_value<T: Serialize>(v: &T) -> Result<Value> {
    Ok(v.serialize(toml_edit::ser::ValueSerializer::new())?)
}

/// Write `field`'s `type`, `list`, `where` and `values` into `t`.
fn write_facts(t: &mut dyn TableLike, field: &DiscoveredField, values: &[EnumValue]) -> Result<()> {
    set(t, "type", to_value(&field.r#type)?);
    if field.list {
        set(t, "list", true.into());
    } else {
        t.remove("list");
    }
    set(t, "where", to_value(&field.r#where)?);
    if field.r#type == FieldType::Enum {
        let mut array = Array::new();
        for v in values {
            let mut v = to_value(v)?;
            v.decor_mut().set_prefix("\n  ");
            array.push_formatted(v);
        }
        if !array.is_empty() {
            array.set_trailing_comma(true);
            array.set_trailing("\n");
        }
        set(t, "values", Value::Array(array));
    } else {
        t.remove("values");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use devkit_common::tracker::discover::named;
    use devkit_config::{Config, Place};

    use super::*;

    fn found(fields: &[(&str, DiscoveredField)]) -> Discovered {
        Discovered {
            fields: fields
                .iter()
                .map(|(n, f)| (n.to_string(), f.clone()))
                .collect(),
            ..Discovered::default()
        }
    }

    fn priority(options: &[(&str, &str)]) -> DiscoveredField {
        DiscoveredField::new(FieldType::Enum, Place::ProjectField {
            id: "PVTSSF_p".into(),
            name: "Priority".into(),
        })
        .with_values(
            options
                .iter()
                .map(|(name, id)| EnumValue {
                    name: name.to_string(),
                    id: id.to_string(),
                    teams: Vec::new(),
                })
                .collect(),
        )
    }

    /// Merge `found` into `repo`, with `home` as a lower layer.
    fn merged(home: &str, repo: &str, found: &Discovered) -> (String, Vec<Change>) {
        merged_on(TrackerKind::Github, home, repo, found)
    }

    fn merged_on(
        kind: TrackerKind,
        home: &str,
        repo: &str,
        found: &Discovered,
    ) -> (String, Vec<Change>) {
        let cfg = Config::parse(&format!("{home}\n{repo}")).unwrap();
        let mut doc: DocumentMut = repo.parse().unwrap();
        let changes = merge(&mut doc, kind, &cfg, found).unwrap();
        (doc.to_string(), changes)
    }

    const REPO: &str = r#"# ticket fields for the board
[github]
project = 7

[ticket.fields.github.prio] # the board's Priority
type   = "enum"
where  = { project_field = "PVTSSF_p", name = "Priority" }
values = [{ name = "urgent", id = "o0" }]
level  = "error" # agents must set it
description = "how soon"
"#;

    /// A git checkout holding `files`, each a path and its text.
    fn checkout(files: &[(&str, &str)]) -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        devkit_git::Git::fixture(repo.path())
            .args(["init", "-q"])
            .output()
            .unwrap();
        std::fs::create_dir(repo.path().join("app")).unwrap();
        for (path, text) in files {
            std::fs::write(repo.path().join(path), text).unwrap();
        }
        repo
    }

    /// The directory discover's file sits in, started from `start`.
    fn target_dir(start: &Path) -> PathBuf {
        let file = target(None, start).unwrap();
        assert_eq!(file.file_name().unwrap(), "devkit.toml");
        file.parent().unwrap().canonicalize().unwrap()
    }

    #[test]
    fn with_no_tracked_layer_the_file_sits_beside_the_nearest_layer() {
        let repo = checkout(&[
            ("devkit.toml", ""),
            ("app/devkit.local.toml", "[config]\nroot = true\n"),
        ]);
        assert_eq!(
            target_dir(&repo.path().join("app")),
            repo.path().join("app").canonicalize().unwrap(),
            "not the checkout root's file, which the barrier cuts off"
        );
        let bare = checkout(&[]);
        assert_eq!(
            target_dir(&bare.path().join("app")),
            bare.path().canonicalize().unwrap(),
            "with no layer, the checkout root"
        );
        std::fs::write(bare.path().join("app/devkit.local.toml"), "").unwrap();
        let inner = bare.path().join("app/inner");
        std::fs::create_dir(&inner).unwrap();
        devkit_git::Git::fixture(&inner)
            .args(["init", "-q"])
            .output()
            .unwrap();
        assert_eq!(
            target_dir(&inner),
            inner.canonicalize().unwrap(),
            "a layer above the checkout leaves it the checkout root"
        );
    }

    #[test]
    fn the_summary_names_fields_it_cannot_write() {
        let found = Discovered {
            unwritable: vec!["`Sprint` (ITERATION)".into()],
            ..Discovered::default()
        };
        let out = summary(TrackerKind::Github, &[], &found);
        assert!(
            out.contains("not written: the tracker's `Sprint` (ITERATION)"),
            "{out}"
        );
    }

    #[test]
    fn merge_keeps_comments_and_person_set_keys() {
        let (text, changes) = merged(
            "",
            REPO,
            &found(&[("priority", priority(&[("P0", "o0"), ("P1", "o1")]))]),
        );
        assert_eq!(changes, [Change::Refreshed("prio".into())]);
        for kept in [
            "# ticket fields for the board",
            "# the board's Priority",
            "level  = \"error\" # agents must set it",
            "description = \"how soon\"",
        ] {
            assert!(text.contains(kept), "lost `{kept}`:\n{text}");
        }
        let cfg = Config::parse(&text).unwrap();
        let prio = &cfg.ticket.fields.github["prio"];
        let values: Vec<(&str, &str)> = prio
            .values
            .iter()
            .map(|v| (v.name.as_str(), v.id.as_str()))
            .collect();
        assert_eq!(
            values,
            [("urgent", "o0"), ("P1", "o1")],
            "a declared value keeps its name; a new one takes the tracker's"
        );
        assert!(!cfg.ticket.fields.github.contains_key("priority"));
    }

    #[test]
    fn merge_adds_undeclared_fields_at_info_only() {
        let home = "[ticket.fields.github.labels]\nlevel = \"warning\"\n";
        let labels = DiscoveredField::new(FieldType::Enum, Place::Labels)
            .list()
            .with_values(vec![named("bug")]);
        let milestone =
            DiscoveredField::new(FieldType::Enum, Place::Milestone).with_values(vec![named("v1")]);
        let (text, changes) = merged(
            home,
            "[github]\nproject = 7\n",
            &found(&[("labels", labels), ("milestone", milestone)]),
        );
        assert_eq!(changes, [
            Change::Refreshed("labels".into()),
            Change::Added("milestone".into())
        ]);
        let doc: DocumentMut = text.parse().unwrap();
        let fields = &doc["ticket"]["fields"]["github"];
        assert!(
            fields["labels"].get("level").is_none(),
            "the home layer's level stays there:\n{text}"
        );
        assert_eq!(fields["labels"]["list"].as_bool(), Some(true));
        assert_eq!(fields["milestone"]["level"].as_str(), Some("info"));
        assert!(
            !text.contains("[ticket]") && !text.contains("[ticket.fields]\n"),
            "parents stay implicit:\n{text}"
        );
    }

    #[test]
    fn merge_drops_and_reports_values_gone_from_the_tracker() {
        let (text, changes) = merged("", REPO, &found(&[("priority", priority(&[("P1", "o1")]))]));
        assert_eq!(changes, [
            Change::ValueDropped {
                field: "prio".into(),
                value: "urgent".into()
            },
            Change::Refreshed("prio".into())
        ]);
        assert!(!text.contains("o0"), "{text}");
    }

    #[test]
    fn a_list_read_in_part_keeps_the_declared_values_it_did_not_answer() {
        let (text, changes) = merged(
            "",
            REPO,
            &found(&[("priority", priority(&[("P1", "o1")]).partly(true))]),
        );
        assert_eq!(changes, [
            Change::ValueKept {
                field: "prio".into(),
                value: "urgent".into()
            },
            Change::Refreshed("prio".into())
        ]);
        let cfg = Config::parse(&text).unwrap();
        let ids: Vec<&str> = cfg.ticket.fields.github["prio"]
            .values
            .iter()
            .map(|v| v.id.as_str())
            .collect();
        assert_eq!(ids, ["o1", "o0"], "{text}");
        let out = summary(TrackerKind::Github, &changes, &Discovered::default());
        assert!(out.contains("kept `urgent`"), "{out}");
    }

    #[test]
    fn a_read_of_some_teams_keeps_what_other_teams_hold() {
        let repo = r#"[ticket.fields.linear.project]
type   = "enum"
where  = "project"
values = [
  { name = "Platform", id = "prj-1", teams = ["ENG", "OPS"] },
  { name = "Pager", id = "prj-2", teams = ["OPS"] },
  { name = "Infra", id = "prj-3", teams = ["ENG", "OPS"] },
  { name = "Old", id = "prj-4", teams = ["ENG"] },
]
"#;
        let value = |name: &str, id: &str| EnumValue {
            name: name.into(),
            id: id.into(),
            teams: vec!["ENG".into()],
        };
        let read = Discovered {
            teams_read: vec!["ENG".into()],
            ..found(&[(
                "project",
                DiscoveredField::new(FieldType::Enum, Place::Project)
                    .with_values(vec![value("Platform", "prj-1"), value("New", "prj-5")]),
            )])
        };
        let (text, changes) = merged_on(TrackerKind::Linear, "", repo, &read);
        assert_eq!(changes, [
            Change::ValueDropped {
                field: "project".into(),
                value: "Old".into()
            },
            Change::Refreshed("project".into())
        ]);
        let cfg = Config::parse(&text).unwrap();
        let values: Vec<(&str, Vec<&str>)> = cfg.ticket.fields.linear["project"]
            .values
            .iter()
            .map(|v| (v.id.as_str(), v.teams.iter().map(String::as_str).collect()))
            .collect();
        assert_eq!(
            values,
            [
                ("prj-1", vec!["ENG", "OPS"]),
                ("prj-5", vec!["ENG"]),
                ("prj-2", vec!["OPS"]),
                ("prj-3", vec!["OPS"]),
            ],
            "{text}"
        );
    }

    #[test]
    fn an_unchanged_field_leaves_the_file_alone() {
        let (text, changes) = merged("", REPO, &found(&[("priority", priority(&[("P0", "o0")]))]));
        assert!(changes.is_empty(), "{changes:?}");
        assert_eq!(text, REPO);
    }

    #[test]
    fn a_template_variable_or_context_name_is_not_taken() {
        let input = DiscoveredField::new(FieldType::String, Place::ProjectField {
            id: "PVTF_i".into(),
            name: "Input".into(),
        });
        let risk = DiscoveredField::new(FieldType::String, Place::ProjectField {
            id: "PVTF_r".into(),
            name: "Risk".into(),
        });
        let repo = "[github]\nproject = 7\n[templates.variables]\nrisk = {}\n";
        let (_, changes) = merged("", repo, &found(&[("input", input), ("risk", risk)]));
        assert!(
            changes
                .iter()
                .all(|c| matches!(c, Change::NameTaken { .. })),
            "{changes:?}"
        );
    }

    #[test]
    fn a_name_declared_for_another_place_is_not_reused() {
        let labels = DiscoveredField::new(FieldType::Enum, Place::Labels).list();
        let repo = "[ticket.fields.github.labels]\ntype = \"string\"\nwhere = \"milestone\"\n";
        let (text, changes) = merged("", repo, &found(&[("labels", labels)]));
        assert_eq!(changes, [Change::NameTaken {
            field: "labels".into(),
            place: "labels".into()
        }]);
        assert_eq!(text, repo);
    }
}
