//! What `ticket create` and `ticket edit` share for ticket fields: refusing
//! before any write, writing through the GitHub writer, reading back, and
//! reporting what did not take.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use devkit_common::{
    fields::{Problem, Report, Resolved, Value, message},
    tracker::{
        Tracker,
        github_fields::{GithubFieldWriter, IssueFields, stale_hint},
    },
};
use devkit_config::FieldDecl;

/// Turn each `issue` value into the issue number the tracker's own parser
/// reads from it, refusing one it rejects before anything is written.
pub(crate) fn number_issue_refs(
    tracker: &dyn Tracker,
    decls: &BTreeMap<String, FieldDecl>,
    resolved: &mut Resolved,
) -> Result<()> {
    let mut problems = Vec::new();
    for (name, values) in &mut resolved.fields {
        let is_issue = decls
            .get(name)
            .is_some_and(|d| d.r#type == Some(devkit_config::FieldType::Issue));
        if !is_issue {
            continue;
        }
        for v in values.iter_mut() {
            let Value::Issue(raw) = v else { continue };
            match tracker.issue_ref(raw.trim().trim_start_matches('#')) {
                Ok(r) => *raw = r.id,
                Err(e) => problems.push(Problem::Invalid {
                    field: name.clone(),
                    value: raw.clone(),
                    reason: format!("is not an issue reference here: {e:#}"),
                }),
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        bail!("{}", message(decls, &problems))
    }
}

/// Refuse the write when the check refused anything, naming every field.
pub(crate) fn refuse(
    surface: &str,
    decls: &BTreeMap<String, FieldDecl>,
    report: &Report,
) -> Result<()> {
    if report.refused.is_empty() {
        return Ok(());
    }
    bail!(
        "{surface} refused, nothing was written:\n{}",
        message(decls, &report.refused)
    )
}

/// Report each warned field after the write.
pub(crate) fn warn(decls: &BTreeMap<String, FieldDecl>, report: &Report) {
    if !report.warned.is_empty() {
        eprintln!("warning: {}", message(decls, &report.warned));
    }
}

/// Write the fields of `resolved` other than `skip` onto issue `number`,
/// which held `before` (or reads it first when `None`), then read it back.
/// Returns one line per field that did not take.
pub(crate) fn write_and_verify(
    writer: &GithubFieldWriter,
    number: u64,
    decls: &BTreeMap<String, FieldDecl>,
    resolved: &Resolved,
    before: Option<IssueFields>,
    skip: &BTreeSet<String>,
    include_nones: bool,
) -> Vec<String> {
    let mut problems = Vec::new();
    let mut failed: BTreeSet<String> = BTreeSet::new();
    let without_nones;
    let resolved = if include_nones {
        resolved
    } else {
        without_nones = Resolved {
            fields: resolved.fields.clone(),
            nones: BTreeSet::new(),
        };
        &without_nones
    };
    let to_write: Vec<&String> = resolved
        .fields
        .keys()
        .chain(&resolved.nones)
        .filter(|n| !skip.contains(*n))
        .collect();
    if !to_write.is_empty() {
        let before = match before {
            Some(b) => Ok(b),
            None => writer.read(number, decls, Some(resolved)),
        };
        match before {
            Ok(before) => {
                for (field, e) in writer.apply(number, decls, resolved, &before, skip) {
                    let mut line = format!("`{field}`{}: {e:#}", given_text(resolved, &field));
                    if let Some(hint) = decls.get(&field).and_then(|d| stale_hint(&field, d, &e)) {
                        line.push_str(&format!("; {hint}"));
                    }
                    problems.push(line);
                    failed.insert(field);
                }
            }
            Err(e) => {
                for field in to_write {
                    problems.push(format!(
                        "`{field}`{}: not written, reading issue #{number} failed: {e:#}",
                        given_text(resolved, field)
                    ));
                    failed.insert(field.clone());
                }
            }
        }
    }
    let written = resolved.fields.keys().chain(&resolved.nones);
    if written.clone().all(|f| failed.contains(f)) {
        return problems;
    }
    match writer.read(number, decls, None) {
        Ok(after) => {
            for (field, reads) in GithubFieldWriter::verify(decls, resolved, &after) {
                if !failed.contains(&field) {
                    problems.push(format!(
                        "`{field}`{} reads back as {reads}",
                        given_text(resolved, &field)
                    ));
                }
            }
        }
        Err(e) => problems.push(format!(
            "issue #{number} could not be read back to confirm its fields: {e:#}"
        )),
    }
    problems
}

/// ` = <values>` for a field given values, ` (--none)` for a clear.
fn given_text(resolved: &Resolved, field: &str) -> String {
    match resolved.fields.get(field) {
        Some(vs) => format!(
            " = `{}`",
            vs.iter().map(Value::name).collect::<Vec<_>>().join(", ")
        ),
        None => " (--none)".to_string(),
    }
}

/// The issue number at the end of a GitHub issue URL.
pub(crate) fn number_of(url: &str) -> Result<u64> {
    url.trim()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .and_then(|n| n.parse().ok())
        .with_context(|| format!("no issue number in `{url}`"))
}
