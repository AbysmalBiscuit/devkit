//! Config values set on the command line with `-o/--option KEY=VALUE`.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::Config;

/// The name a command-line value carries as its source: in the layer list, in
/// `devkit config show --origin`, and in every error that names a layer.
pub const COMMAND_LINE: &str = "<command line>";

/// The `--option` values of one command, as one layer above every config file.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Options(toml::Table);

impl Options {
    /// Parse each `KEY=VALUE` in order, so a later value for a key replaces an
    /// earlier one. `KEY` is a dotted TOML key that the `devkit.toml` schema
    /// knows, and `VALUE` is a TOML value: a string needs its quotes.
    pub fn parse<S: AsRef<str>>(raw: &[S]) -> Result<Self> {
        let schema = serde_json::to_value(schemars::schema_for!(Config))
            .context("generating the devkit.toml schema")?;
        let mut table = toml::Table::new();
        for arg in raw {
            let (path, value) = parse_one(arg.as_ref())?;
            let key = path.join(".");
            if !knows(&schema, &schema, &path) {
                bail!(
                    "--option {key}: devkit.toml has no key `{key}`; \
                     `devkit schema` lists the keys"
                );
            }
            insert(&mut table, &path, value);
        }
        Ok(Self(table))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// This set as a config layer, or `None` when it sets nothing.
    pub(crate) fn layer(&self) -> Option<(PathBuf, toml::Table)> {
        (!self.is_empty()).then(|| (PathBuf::from(COMMAND_LINE), self.0.clone()))
    }
}

/// One `KEY=VALUE`, as the key's segments and the parsed value.
fn parse_one(arg: &str) -> Result<(Vec<String>, toml::Value)> {
    let (key, value) =
        split_assignment(arg).ok_or_else(|| anyhow!("--option {arg}: expected KEY=VALUE"))?;
    let key = key.trim();
    let path =
        key_segments(key).with_context(|| format!("--option {arg}: `{key}` is not a TOML key"))?;
    let mut doc: toml::Table = toml::from_str(&format!("v = {value}")).map_err(|_| {
        anyhow!(
            "--option {key}: `{value}` is not a TOML value; \
             a string needs quotes, as in {key}='\"{value}\"'"
        )
    })?;
    match (doc.remove("v"), doc.is_empty()) {
        (Some(v), true) => Ok((path, v)),
        _ => bail!("--option {key}: `{value}` is more than one TOML value"),
    }
}

/// `arg` split at its first `=` outside a quoted key segment, so a quoted
/// segment such as `apps."a=b"` keeps its `=`.
fn split_assignment(arg: &str) -> Option<(&str, &str)> {
    let mut quote = None;
    let mut escaped = false;
    for (i, c) in arg.char_indices() {
        match (quote, c) {
            (Some('"'), _) if escaped => escaped = false,
            (Some('"'), '\\') => escaped = true,
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '=') => return Some((&arg[..i], &arg[i + 1..])),
            (None, _) => {}
        }
    }
    None
}

/// The segments of a dotted TOML key, quoted segments included.
fn key_segments(key: &str) -> Result<Vec<String>> {
    let mut table: toml::Table = toml::from_str(&format!("{key} = 0"))?;
    let mut path = Vec::new();
    loop {
        let mut entries = std::mem::take(&mut table).into_iter();
        let (Some((seg, value)), None) = (entries.next(), entries.next()) else {
            bail!("not one key");
        };
        path.push(seg);
        match value {
            toml::Value::Table(inner) => table = inner,
            _ => return Ok(path),
        }
    }
}

/// Set `path` in `table` to `value`, replacing whatever a non-table segment
/// held.
fn insert(table: &mut toml::Table, path: &[String], value: toml::Value) {
    let (last, parents) = path.split_last().expect("a key has a segment");
    let mut at = table;
    for seg in parents {
        let slot = at
            .entry(seg.clone())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !slot.is_table() {
            *slot = toml::Value::Table(toml::Table::new());
        }
        at = slot.as_table_mut().expect("made a table above");
    }
    at.insert(last.clone(), value);
}

/// Whether the schema `node` admits the key `path` below it. A struct admits
/// only its own fields, a map admits any key with a value its value schema
/// admits, and a free-form table admits any key.
fn knows(root: &Value, node: &Value, path: &[String]) -> bool {
    let node = resolve_ref(root, node);
    let Some((seg, rest)) = path.split_first() else {
        return true;
    };
    let branches: Vec<&Value> = ["anyOf", "oneOf", "allOf"]
        .iter()
        .filter_map(|k| node.get(k).and_then(Value::as_array))
        .flatten()
        .collect();
    if !branches.is_empty() {
        return branches.iter().any(|b| knows(root, b, path));
    }
    if let Some(field) = node.get("properties").and_then(|p| p.get(seg)) {
        return knows(root, field, rest);
    }
    match node.get("additionalProperties") {
        Some(Value::Bool(open)) => *open,
        Some(values) => knows(root, values, rest),
        None => node.get("properties").is_none() && admits_object(node),
    }
}

fn admits_object(node: &Value) -> bool {
    match node.get("type") {
        None => node.is_object() || node == &Value::Bool(true),
        Some(Value::String(t)) => t == "object",
        Some(Value::Array(ts)) => ts.iter().any(|t| t == "object"),
        Some(_) => false,
    }
}

/// `node` with a local `$ref` followed to its definition.
fn resolve_ref<'a>(root: &'a Value, node: &'a Value) -> &'a Value {
    let target = node
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| r.strip_prefix('#'))
        .and_then(|pointer| root.pointer(pointer));
    match target {
        Some(def) => resolve_ref(root, def),
        None => node,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> toml::Table {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn a_later_value_for_a_key_replaces_an_earlier_one() {
        let opts = Options::parse(&["tracker.kind=\"github\"", "tracker.kind=\"none\""]).unwrap();
        assert_eq!(opts.0, table("tracker.kind = 'none'"));
    }

    #[test]
    fn values_parse_as_toml() {
        let opts = Options::parse(&[
            "apps.api.base_port=9100",
            "defaults.worktree_include=[\".env\"]",
            "apps.\"my.app\".base_port = 7",
        ])
        .unwrap();
        assert_eq!(
            opts.0,
            table(
                "apps.api.base_port = 9100\napps.'my.app'.base_port = 7\n\
                 defaults.worktree_include = ['.env']"
            )
        );
    }

    #[test]
    fn a_bare_word_value_is_refused_naming_the_key() {
        let err = Options::parse(&["tracker.kind=none"])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("tracker.kind") && err.contains("quotes"),
            "{err}"
        );
    }

    #[test]
    fn a_value_that_smuggles_a_second_key_is_refused() {
        let err = Options::parse(&["tracker.kind=\"none\"\ndefaults.branch_prefix=\"x\""])
            .unwrap_err()
            .to_string();
        assert!(err.contains("tracker.kind"), "{err}");
    }

    #[test]
    fn a_key_the_schema_does_not_know_is_refused_naming_it() {
        for key in [
            "trackr.kind",
            "tracker.knd",
            "tracker.kind.deeper",
            "apps.api.nope",
        ] {
            let err = Options::parse(&[format!("{key}=1")])
                .unwrap_err()
                .to_string();
            assert!(err.contains(&format!("`{key}`")), "{key}: {err}");
        }
    }

    #[test]
    fn map_entries_and_optional_tables_are_known() {
        Options::parse(&[
            "apps.web.base_port=1",
            "people.ann.github=\"ann\"",
            "todo.postgres.root=\"devkit\"",
            "templates.variables.team.default=\"x\"",
        ])
        .unwrap();
    }

    #[test]
    fn an_argument_without_a_value_is_refused() {
        let err = Options::parse(&["tracker.kind"]).unwrap_err().to_string();
        assert!(err.contains("KEY=VALUE"), "{err}");
    }
}
