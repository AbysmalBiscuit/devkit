//! What `ticket create` and `ticket edit` share for ticket fields: refusing
//! before any write, writing through the GitHub or Linear writer, reading
//! back, and reporting what did not take.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use devkit_common::{
    fields::{Problem, Report, Resolved, Value, message},
    tracker::{
        Tracker,
        github_fields::{GithubFieldWriter, IssueFields, stale_hint},
        linear_fields::{LinearFieldWriter, LinearIssue, written_ids},
    },
};
use devkit_config::{FieldDecl, Place, TrackerKind};

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
) -> Vec<String> {
    let mut problems = Vec::new();
    let mut failed: BTreeSet<String> = BTreeSet::new();
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
                    if let Some(hint) = decls
                        .get(&field)
                        .and_then(|d| stale_hint(TrackerKind::Github, &field, d, &e))
                    {
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

/// The Linear API key, refusing before any request without one.
pub(crate) fn linear_key() -> Result<String> {
    devkit_common::secrets::resolve("LINEAR_API_KEY").context(
        "no LINEAR_API_KEY in the environment or ~/.config/devkit/secrets.toml; run `devkit auth \
         linear`",
    )
}

/// The ticket's URL, else its identifier.
pub(crate) fn linear_link(issue: &LinearIssue) -> &str {
    if issue.url.is_empty() {
        &issue.identifier
    } else {
        &issue.url
    }
}

/// Set the blocked-by fields of `resolved` on the Linear ticket that held
/// `before`, then compare every field of `resolved` with what the ticket
/// holds. `before` is read again first when `stale`, because writes followed
/// its read. Returns the ticket as last read, and one line per field that
/// did not take.
pub(crate) fn linear_finish(
    writer: &LinearFieldWriter,
    before: LinearIssue,
    decls: &BTreeMap<String, FieldDecl>,
    resolved: &Resolved,
    refs: &BTreeMap<String, String>,
    stale: bool,
) -> (LinearIssue, Vec<String>) {
    let mut problems = Vec::new();
    let mut failed: BTreeSet<String> = BTreeSet::new();
    let mut related = false;
    for name in resolved.fields.keys().chain(&resolved.nones) {
        if decls.get(name).and_then(|d| d.r#where.as_ref()) != Some(&Place::BlockedBy) {
            continue;
        }
        let values = resolved
            .fields
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or_default();
        related = true;
        if let Err(e) = writer.set_blocked_by(&before, &written_ids(values), refs) {
            problems.push(format!("`{name}`{}: {e:#}", given_text(resolved, name)));
            failed.insert(name.clone());
        }
    }
    let after = if stale || related {
        match writer.read_by_uuid(&before.uuid) {
            Ok(after) => after,
            Err(e) => {
                problems.push(format!(
                    "{} could not be read back to confirm its fields: {e:#}",
                    before.identifier
                ));
                return (before, problems);
            }
        }
    } else {
        before
    };
    for (field, reads) in LinearFieldWriter::verify(decls, resolved, &after) {
        if !failed.contains(&field) {
            problems.push(format!(
                "`{field}`{} reads back as {reads}",
                given_text(resolved, &field)
            ));
        }
    }
    (after, problems)
}

/// A refused Linear write, with the stale-id hint of each enum field it
/// named.
pub(crate) fn linear_refused(
    e: anyhow::Error,
    decls: &BTreeMap<String, FieldDecl>,
    resolved: &Resolved,
) -> anyhow::Error {
    let hints: Vec<String> = resolved
        .fields
        .keys()
        .filter_map(|f| stale_hint(TrackerKind::Linear, f, decls.get(f)?, &e))
        .collect();
    if hints.is_empty() {
        e
    } else {
        e.context(hints.join("; "))
    }
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
