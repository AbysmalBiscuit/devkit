//! Config values set on the command line with `-o/--option KEY=VALUE`, and
//! in the `DEVKIT_OPTIONS` environment variable.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::{Config, HarnessSection};

/// The name a command-line value carries as its source: in the layer list, in
/// `devkit config show --origin`, and in every error that names a layer.
pub const COMMAND_LINE: &str = "<command line>";

/// The environment variable that holds `--option` values, one `KEY=VALUE`
/// per line. Its values sit under the command line's.
pub const OPTIONS_VAR: &str = "DEVKIT_OPTIONS";

/// The name a value from [`OPTIONS_VAR`] carries as its source.
const ENVIRONMENT: &str = "$DEVKIT_OPTIONS";

/// Tables of the published `devkit.toml` schema that devkit reads from the
/// config files alone, outside the merged [`Config`], with the reason.
const FILE_ONLY_TABLES: [(&str, &str); 2] = [
    (
        "config",
        "it decides which config files exist, before any value merges",
    ),
    ("docs", "`docm` reads it from the config files"),
];

/// The `--option` values of one command, as two layers above every config
/// file: the values of [`OPTIONS_VAR`], then the command line's.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Options {
    environment: toml::Table,
    command_line: toml::Table,
}

impl Options {
    /// Parse each command-line `KEY=VALUE` in order, so a later value for a
    /// key replaces an earlier one. `KEY` is a dotted TOML key that the
    /// `devkit.toml` schema knows, and `VALUE` is a TOML value: a string needs
    /// its quotes.
    pub fn parse<S: AsRef<str>>(raw: &[S]) -> Result<Self> {
        Self::new(None, raw)
    }

    /// The values of `variable`, one `KEY=VALUE` per line, under the values
    /// of `command_line`. Each set parses as [`Options::parse`] parses.
    pub fn new<S: AsRef<str>>(variable: Option<&str>, command_line: &[S]) -> Result<Self> {
        let lines: Vec<&str> = variable
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect();
        Ok(Self {
            environment: parse_set(&lines, ENVIRONMENT)?,
            command_line: parse_set(command_line, "--option")?,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.environment.is_empty() && self.command_line.is_empty()
    }

    /// These values as config layers, lowest precedence first, each named by
    /// its source. A set that sets nothing adds no layer.
    pub fn layers(&self) -> Vec<(PathBuf, toml::Table)> {
        [
            (ENVIRONMENT, &self.environment),
            (COMMAND_LINE, &self.command_line),
        ]
        .into_iter()
        .filter(|(_, table)| !table.is_empty())
        .map(|(name, table)| (PathBuf::from(name), table.clone()))
        .collect()
    }

    /// Both layers merged into one table, or `None` when they set nothing.
    /// The `[harness]` readers, which read layer files outside the merged
    /// [`Config`], take it as their top layer.
    pub fn merged(&self) -> Option<toml::Table> {
        (!self.is_empty()).then(|| crate::merge_layers(&self.layers()).0)
    }

    /// The [`OPTIONS_VAR`] value that gives a child process the config these
    /// values give this process, or `None` when they set nothing. `start` and
    /// `checkout_root` are the anchors this process resolves its config at.
    /// A path leaves with `${VAR}` and `~` expanded, because the child's
    /// environment can differ. A relative host path leaves anchored to them,
    /// because the child anchors a relative path to its own directory. A
    /// relative repository path stays relative inside a checkout, so a child
    /// in another checkout resolves its own copy. Each line sets one leaf on
    /// one line, so a string that holds a newline keeps it as an escape.
    pub fn to_variable(
        &self,
        start: &Path,
        checkout_root: Option<&Path>,
    ) -> Result<Option<String>> {
        let Some(mut merged) = self.merged() else {
            return Ok(None);
        };
        for (key, kind, _) in crate::DEFAULTS_PATHS {
            if let Some(toml::Value::String(raw)) = lookup_mut(&mut merged, key) {
                let resolved = match (kind, checkout_root) {
                    // A repository file resolves against the checkout reading
                    // the config, so the child resolves its own copy.
                    (crate::PathKind::RepoRelative, Some(_)) => {
                        let expanded = crate::expand_vars(raw, key)?;
                        crate::expand_tilde(&expanded)
                            .to_string_lossy()
                            .into_owned()
                    }
                    _ => crate::resolve_path_key(
                        raw,
                        key,
                        kind,
                        &Default::default(),
                        checkout_root,
                        start,
                    )?,
                };
                *raw = escape_vars(&resolved);
            }
        }
        let log_dir = "harness.log.dir";
        if let Some(toml::Value::String(raw)) = lookup_mut(&mut merged, log_dir) {
            let resolved = crate::resolve_host_path(raw, log_dir, Some(start))?;
            *raw = escape_vars(&resolved.to_string_lossy());
        }
        let mut lines = Vec::new();
        leaf_lines(&merged, "", &mut lines);
        Ok(Some(lines.join("\n")))
    }
}

/// `path`, already expanded, spelled so that the child's expansion gives it
/// back unchanged.
fn escape_vars(path: &str) -> String {
    path.replace('$', "$$")
}

/// Whether `source`, a layer's name, is a set of `--option` values rather than
/// a file.
pub fn is_option_source(source: &Path) -> bool {
    source == Path::new(COMMAND_LINE) || source == Path::new(ENVIRONMENT)
}

/// Parse one set of `KEY=VALUE` values, with errors that start with `label`.
fn parse_set<S: AsRef<str>>(raw: &[S], label: &str) -> Result<toml::Table> {
    let schema = serde_json::to_value(schemars::schema_for!(Config))
        .context("generating the devkit.toml schema")?;
    let harness = serde_json::to_value(schemars::schema_for!(HarnessSection))
        .context("generating the [harness] schema")?;
    let mut table = toml::Table::new();
    let mut harness_keys = Vec::new();
    for arg in raw {
        let (path, value) = parse_one(arg.as_ref(), label)?;
        let key = path.join(".");
        if let Some((table, reason)) = FILE_ONLY_TABLES.iter().find(|(t, _)| path[0] == *t) {
            bail!(
                "{label} {key}: `{key}` is a `[{table}]` key, and `[{table}]` keys \
                 cannot be set with {label}: {reason}"
            );
        }
        let unknown = match path.split_first() {
            Some((table, rest)) if table == "harness" => {
                unknown_key(&harness, &harness, &nested(rest, value.clone()))
                    .map(|below| [vec![table.clone()], below].concat())
            }
            _ => unknown_key(&schema, &schema, &nested(&path, value.clone())),
        };
        if let Some(unknown) = unknown {
            let name = if unknown.len() <= path.len() {
                key.clone()
            } else {
                unknown.join(".")
            };
            bail!(
                "{label} {key}: devkit.toml has no key `{name}`; \
                 `devkit schema` lists the keys"
            );
        }
        if path[0] == "harness" {
            harness_keys.push(path.clone());
        }
        insert(&mut table, &path, value);
    }
    for path in &harness_keys {
        check_harness_value(&table, path).with_context(|| format!("{label} {}", path.join(".")))?;
    }
    Ok(table)
}

/// One `KEY=VALUE` line for each leaf of `table`: each value that is not a
/// table, and each empty table, which a merge still adds.
fn leaf_lines(table: &toml::Table, prefix: &str, lines: &mut Vec<String>) {
    for (key, value) in table {
        let path = format!("{prefix}{}", toml_key(key));
        match value {
            toml::Value::Table(inner) if !inner.is_empty() => {
                leaf_lines(inner, &format!("{path}."), lines);
            }
            _ => lines.push(format!("{path}={}", inline(value))),
        }
    }
}

/// `value` as TOML on one line.
fn inline(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => basic_string(s),
        toml::Value::Array(items) => {
            let items: Vec<String> = items.iter().map(inline).collect();
            format!("[{}]", items.join(", "))
        }
        toml::Value::Table(table) => {
            let entries: Vec<String> = table
                .iter()
                .map(|(k, v)| format!("{} = {}", toml_key(k), inline(v)))
                .collect();
            format!("{{ {} }}", entries.join(", "))
        }
        scalar => scalar.to_string(),
    }
}

/// `key` as one segment of a dotted TOML key: bare when TOML allows it,
/// otherwise quoted.
fn toml_key(key: &str) -> String {
    let bare = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if bare {
        key.to_owned()
    } else {
        basic_string(key)
    }
}

/// `s` as a TOML basic string, with every control character escaped.
fn basic_string(s: &str) -> String {
    let mut out = String::from('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Read the value at `path`, alone, as the `[harness]` readers read it. A
/// rendered-tool entry needs `title` and `body`, which an option that changes
/// one key of a file's entry leaves to the file, so they stand in here.
fn check_harness_value(table: &toml::Table, path: &[String]) -> Result<()> {
    let Some(value) = lookup(table, path) else {
        return Ok(());
    };
    let mut one = nested(&path[1..], value.clone());
    for tools in ["issue_tools", "pr_tools"] {
        let entries = one.get_mut(tools).and_then(toml::Value::as_table_mut);
        for entry in entries
            .into_iter()
            .flat_map(|e| e.iter_mut().map(|(_, v)| v))
        {
            if let Some(entry) = entry.as_table_mut() {
                for key in ["title", "body"] {
                    entry.entry(key).or_insert_with(|| String::new().into());
                }
            }
        }
    }
    one.try_into::<HarnessSection>()?;
    Ok(())
}

/// The value at the dotted `key` in `table`.
fn lookup_mut<'a>(table: &'a mut toml::Table, key: &str) -> Option<&'a mut toml::Value> {
    let mut segments = key.split('.');
    let first = table.get_mut(segments.next()?)?;
    segments.try_fold(first, |at, seg| at.as_table_mut()?.get_mut(seg))
}

/// The value at `path` in `table`, if a later option left it there.
fn lookup<'a>(table: &'a toml::Table, path: &[String]) -> Option<&'a toml::Value> {
    let (first, rest) = path.split_first()?;
    rest.iter()
        .try_fold(table.get(first)?, |at, seg| at.as_table()?.get(seg))
}

/// One `KEY=VALUE`, as the key's segments and the parsed value.
fn parse_one(arg: &str, label: &str) -> Result<(Vec<String>, toml::Value)> {
    let (key, value) =
        split_assignment(arg).ok_or_else(|| anyhow!("{label} {arg}: expected KEY=VALUE"))?;
    let key = key.trim();
    let path =
        key_segments(key).with_context(|| format!("{label} {arg}: `{key}` is not a TOML key"))?;
    let mut doc: toml::Table = toml::from_str(&format!("v = {value}")).map_err(|_| {
        anyhow!(
            "{label} {key}: `{value}` is not a TOML value; \
             a string needs quotes, as in {key}='\"{value}\"'"
        )
    })?;
    match (doc.remove("v"), doc.is_empty()) {
        (Some(v), true) => Ok((path, v)),
        _ => bail!("{label} {key}: `{value}` is more than one TOML value"),
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

/// `value` placed at `path` in tables of its own, or `value` alone when `path`
/// is empty.
fn nested(path: &[String], value: toml::Value) -> toml::Value {
    path.iter().rev().fold(value, |inner, seg| {
        toml::Value::Table(toml::Table::from_iter([(seg.clone(), inner)]))
    })
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

/// The schema that admits any value.
static ANY: Value = Value::Bool(true);

/// The first key in `value` that the schema `node` does not admit, as its
/// path below `node`. A struct admits only its own fields, a map admits any
/// key, and a free-form table admits any key. A value of the wrong type is
/// left to deserialization, which names the type it expected.
fn unknown_key(root: &Value, node: &Value, value: &toml::Value) -> Option<Vec<String>> {
    let entries: Vec<(String, &toml::Value)> = match value {
        toml::Value::Table(t) => t.iter().map(|(k, v)| (k.clone(), v)).collect(),
        toml::Value::Array(a) => {
            let items = schemas_of(root, node, &|n| match n.get("items") {
                Some(Value::Array(tuple)) => tuple.iter().collect(),
                Some(items) => vec![items],
                None if admits(n, "array") => vec![&ANY],
                None => Vec::new(),
            });
            if items.is_empty() {
                return None;
            }
            return a
                .iter()
                .enumerate()
                .find_map(|(i, v)| first_unknown(root, &items, i.to_string(), v));
        }
        _ => return None,
    };
    entries.into_iter().find_map(|(seg, v)| {
        let fields = schemas_of(root, node, &|n| match (
            n.get("properties").and_then(|p| p.get(&seg)),
            n.get("additionalProperties"),
        ) {
            (Some(field), _) => vec![field],
            (None, Some(Value::Bool(false))) => Vec::new(),
            (None, Some(Value::Bool(true))) => vec![&ANY],
            (None, Some(values)) => vec![values],
            (None, None) if n.get("properties").is_none() && admits(n, "object") => vec![&ANY],
            (None, None) => Vec::new(),
        });
        first_unknown(root, &fields, seg, v)
    })
}

/// `None` when one of `schemas` admits every key in `value`, otherwise the
/// unknown key's path, which starts at `seg`.
fn first_unknown(
    root: &Value,
    schemas: &[&Value],
    seg: String,
    value: &toml::Value,
) -> Option<Vec<String>> {
    let mut below = Vec::new();
    for schema in schemas {
        match unknown_key(root, schema, value) {
            None => return None,
            Some(path) if below.is_empty() => below = path,
            Some(_) => {}
        }
    }
    Some([vec![seg], below].concat())
}

/// What `pick` finds in `node` and in each `anyOf`, `oneOf` and `allOf`
/// branch below it, with each local `$ref` followed.
fn schemas_of<'a>(
    root: &'a Value,
    node: &'a Value,
    pick: &dyn Fn(&'a Value) -> Vec<&'a Value>,
) -> Vec<&'a Value> {
    let node = resolve_ref(root, node);
    let branches: Vec<&Value> = ["anyOf", "oneOf", "allOf"]
        .iter()
        .filter_map(|k| node.get(k).and_then(Value::as_array))
        .flatten()
        .collect();
    if branches.is_empty() {
        pick(node)
    } else {
        branches
            .into_iter()
            .flat_map(|b| schemas_of(root, b, pick))
            .collect()
    }
}

fn admits(node: &Value, kind: &str) -> bool {
    match node.get("type") {
        None => node.is_object() || node == &ANY,
        Some(Value::String(t)) => t == kind,
        Some(Value::Array(ts)) => ts.iter().any(|t| t == kind),
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
        assert_eq!(opts.command_line, table("tracker.kind = 'none'"));
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
            opts.command_line,
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
    fn each_key_inside_a_table_value_is_checked_against_the_schema() {
        for (option, key) in [
            ("defaults={branch_prefx=\"x/\"}", "defaults.branch_prefx"),
            ("apps={api={base_prt=1}}", "apps.api.base_prt"),
            (
                "apps.web.prep_files=[{path=\"a\",content=\"\",overwrit=true}]",
                "apps.web.prep_files.0.overwrit",
            ),
            (
                "harness={enforce_commandz=true}",
                "harness.enforce_commandz",
            ),
            ("harness.log={enabld=true}", "harness.log.enabld"),
        ] {
            let err = Options::parse(&[option]).unwrap_err().to_string();
            assert!(err.contains(&format!("`{key}`")), "{option}: {err}");
        }
        Options::parse(&[
            "defaults={branch_prefix=\"x/\"}",
            "apps={api={base_port=1,prep_files=[{path=\"a\",content=\"\"}]}}",
            "people={ann={github=\"ann\"}}",
            "harness={log={enabled=true},commands={x={programs=[\"node\"]}}}",
        ])
        .unwrap();
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
    fn harness_keys_are_checked_against_the_schema() {
        Options::parse(&[
            "harness.enforce_commands=true",
            "harness.log.enabled=true",
            "harness.commands.x.programs=[\"node\"]",
        ])
        .unwrap();
        let err = Options::parse(&["harness.nope=true"])
            .unwrap_err()
            .to_string();
        assert!(err.contains("`harness.nope`"), "{err}");
    }

    #[test]
    fn a_harness_value_is_read_as_the_harness_readers_read_it() {
        let err = format!(
            "{:#}",
            Options::parse(&["harness.log.max_age_days=0"]).unwrap_err()
        );
        assert!(err.contains("max_age_days = 0 is ambiguous"), "{err}");
        let err = format!(
            "{:#}",
            Options::parse(&["harness.enforce_commands=1"]).unwrap_err()
        );
        assert!(err.contains("bool"), "{err}");
        Options::parse(&["harness.pr_tools.x.enabled=false"]).unwrap();
        Options::parse(&[
            "harness.enforce_commands=1",
            "harness.enforce_commands=true",
        ])
        .unwrap();
    }

    #[test]
    fn schema_tables_the_merged_config_skips_are_named_as_such() {
        for (key, table) in [("config.root=true", "config"), ("docs.libs=[]", "docs")] {
            let err = Options::parse(&[key]).unwrap_err().to_string();
            assert!(
                err.contains(&format!("`[{table}]` keys cannot be set")),
                "{key}: {err}"
            );
            assert!(!err.contains("has no key"), "{key}: {err}");
        }
    }

    #[test]
    fn the_variable_gives_a_child_the_merged_values_one_per_line() {
        let opts = Options::new(
            Some("defaults.branch_prefix=\"env/\"\napps.web.base_port=1\ntracker.kind=\"linear\""),
            &[
                "tracker.kind=\"none\"",
                "apps.web={}",
                "people.\"a=b\".github=\"ann\"",
                "templates.variables.note.default='''two\nlines \"q\" \\ \t'''",
                "defaults.worktree_include=[\".env\"]",
                "apps.api.prep_files=[{ path = \"a b\", content = \"x\\ny\" }]",
            ],
        )
        .unwrap();
        let variable = opts
            .to_variable(Path::new("/start"), None)
            .unwrap()
            .unwrap();
        assert!(variable.lines().all(|l| l.contains('=')), "{variable}");
        let child = Options::new(Some(&variable), &[] as &[&str]).unwrap();
        assert_eq!(child.merged(), opts.merged(), "{variable}");
        assert_eq!(
            Options::default()
                .to_variable(Path::new("/start"), None)
                .unwrap(),
            None
        );
    }

    #[test]
    fn the_variable_anchors_relative_paths_as_this_process_does() {
        let start = std::env::temp_dir().join("start");
        let root = std::env::temp_dir().join("root");
        let opts = Options::parse(&[
            "defaults.worktree_root=\"wts\"",
            "defaults.doppler_yaml=\"doppler.yaml\"",
            "defaults.baseline_dir=\"a$$b\"",
            "harness.log.dir=\"logs\"",
        ])
        .unwrap();
        let variable = opts.to_variable(&start, Some(&root)).unwrap().unwrap();
        let child = Options::new(Some(&variable), &[] as &[&str])
            .unwrap()
            .merged()
            .unwrap();
        let at = |key: &str| {
            PathBuf::from(
                lookup(
                    &child,
                    &key.split('.').map(String::from).collect::<Vec<_>>(),
                )
                .unwrap()
                .as_str()
                .unwrap(),
            )
        };
        assert_eq!(at("defaults.worktree_root"), start.join("wts"));
        assert_eq!(
            at("defaults.doppler_yaml"),
            PathBuf::from("doppler.yaml"),
            "a repository path stays relative inside a checkout"
        );
        assert_eq!(at("harness.log.dir"), start.join("logs"));
        let baseline = at("defaults.baseline_dir");
        assert_eq!(
            crate::expand_vars(&baseline.to_string_lossy(), "defaults.baseline_dir").unwrap(),
            start.join("a$b").to_string_lossy(),
            "the child expands the value to what this process expanded"
        );

        let outside = opts.to_variable(&start, None).unwrap().unwrap();
        let child = Options::new(Some(&outside), &[] as &[&str])
            .unwrap()
            .merged()
            .unwrap();
        let doppler = lookup(&child, &["defaults".into(), "doppler_yaml".into()]).unwrap();
        assert_eq!(
            PathBuf::from(doppler.as_str().unwrap()),
            start.join("doppler.yaml"),
            "outside a checkout it anchors as this process anchors it"
        );
    }

    #[test]
    fn a_command_line_value_sits_above_the_variable() {
        let opts =
            Options::new(Some("tracker.kind=\"linear\""), &["tracker.kind=\"none\""]).unwrap();
        let names: Vec<_> = opts.layers().into_iter().map(|(p, _)| p).collect();
        assert_eq!(names, [
            PathBuf::from(ENVIRONMENT),
            PathBuf::from(COMMAND_LINE)
        ]);
        assert_eq!(opts.merged().unwrap(), table("tracker.kind = 'none'"));
    }

    #[test]
    fn an_argument_without_a_value_is_refused() {
        let err = Options::parse(&["tracker.kind"]).unwrap_err().to_string();
        assert!(err.contains("KEY=VALUE"), "{err}");
    }
}
