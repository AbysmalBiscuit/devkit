//! Whether a caller must supply a given `--arg`. The single door: every
//! `--arg` surface asks here, and nothing re-derives the answer.

use std::collections::{BTreeMap, BTreeSet};

use devkit_config::{Config, Required};

use crate::caller::Caller;

/// A required arg the caller did not supply. `reason` is the marking that
/// bound it, or `Never` when the derived rule did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    pub name: String,
    pub reason: Required,
    pub description: Option<String>,
}

impl Missing {
    /// The fragment an error joins. A caller-specific requirement names its
    /// audience, so an agent's report explains why the same command succeeds
    /// for the human reading it.
    pub fn hint(&self) -> String {
        let name = &self.name;
        match self.reason {
            Required::Never | Required::Always => format!("--arg {name}=..."),
            Required::Agents => format!("--arg {name}=... (required for agents)"),
            Required::Humans => format!("--arg {name}=... (required for humans)"),
        }
    }
}

/// Refuse the run when anything is missing, naming each arg as `what needs
/// --arg a=... --arg b=...`, then one [`description_line`] per described arg.
pub fn ensure_supplied(what: &str, missing: &[Missing]) -> anyhow::Result<()> {
    anyhow::ensure!(
        missing.is_empty(),
        "{what} needs {}{}",
        missing
            .iter()
            .map(Missing::hint)
            .collect::<Vec<_>>()
            .join(" "),
        missing
            .iter()
            .filter_map(|m| Some(description_line(&m.name, m.description.as_deref()?)))
            .collect::<String>()
    );
    Ok(())
}

/// An arg's `[templates.variables]` description as a line appended to a
/// message that names the arg.
pub fn description_line(label: &str, description: &str) -> String {
    format!("\n  {label}: {description}")
}

/// What `[templates.variables]` says to pass for `name`.
pub fn description(cfg: &Config, name: &str) -> Option<String> {
    cfg.templates
        .variables
        .get(name)
        .and_then(|d| d.description())
        .map(str::to_string)
}

/// Whether a marking binds this caller.
pub fn binds(r: Required, caller: Caller) -> bool {
    match r {
        Required::Never => false,
        Required::Always => true,
        Required::Agents => caller == Caller::Agent,
        Required::Humans => caller == Caller::Human,
    }
}

/// Every marking that applies to `name` under `task`, in precedence order.
///
/// The task's own entry wins alone when it has one, `never` included, so a
/// sequence can opt out of a marking its own steps carry. Failing that, the
/// steps' entries all apply: a command task's marking holds however it is
/// reached, and running it as one step of a sequence is a way of reaching it.
/// Failing that, the variable's own marking.
fn applicable(cfg: &Config, task: Option<&str>, name: &str) -> Vec<Required> {
    if let Some(t) = task.and_then(|t| cfg.tasks.get(t)) {
        if let Some(r) = t.required_args.get(name) {
            return vec![*r];
        }
        let from_steps: Vec<Required> = t
            .steps
            .iter()
            .filter_map(|s| match s {
                devkit_config::Step::Task(r) => cfg.tasks.get(r),
                devkit_config::Step::Up(_) => None,
            })
            .filter_map(|sub| sub.required_args.get(name).copied())
            .collect();
        if !from_steps.is_empty() {
            return from_steps;
        }
    }
    cfg.templates
        .variables
        .get(name)
        .map(|d| d.required())
        .into_iter()
        .collect()
}

/// The marking in force: the task's entry, else a step-task's, else the
/// variable's, else none. Where several steps mark one name, the first in
/// step order stands in for the set; `is_required` consults them all.
pub fn declared_required(cfg: &Config, task: Option<&str>, name: &str) -> Required {
    applicable(cfg, task, name)
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// Whether `name` carries a default anywhere in `[templates.variables]`.
fn has_default(cfg: &Config, name: &str) -> bool {
    cfg.templates
        .variables
        .get(name)
        .and_then(|d| d.default_value())
        .is_some()
}

/// The derived rule ORed with the marking. The derived rule is a floor, so a
/// marking can only add a requirement, never remove one.
pub fn is_required(cfg: &Config, task: Option<&str>, name: &str, caller: Caller) -> bool {
    !has_default(cfg, name)
        || applicable(cfg, task, name)
            .into_iter()
            .any(|r| binds(r, caller))
}

/// Which callers [`is_required`] binds for `name` under `task`, as one
/// marking: `Always` when it binds both, `Never` when neither.
pub fn required_of(cfg: &Config, task: Option<&str>, name: &str) -> Required {
    match (
        is_required(cfg, task, name, Caller::Agent),
        is_required(cfg, task, name, Caller::Human),
    ) {
        (true, true) => Required::Always,
        (true, false) => Required::Agents,
        (false, true) => Required::Humans,
        (false, false) => Required::Never,
    }
}

/// The required names this run's templates read and the caller did not supply.
/// `reads` is what the surface will actually render; a name no template reads
/// is never asked for.
pub fn missing_args(
    cfg: &Config,
    task: Option<&str>,
    reads: &BTreeSet<String>,
    given: &BTreeMap<String, String>,
    caller: Caller,
) -> Vec<Missing> {
    reads
        .iter()
        .filter(|n| !given.contains_key(n.as_str()))
        .filter(|n| is_required(cfg, task, n, caller))
        .map(|n| Missing {
            name: n.clone(),
            reason: binding_reason(cfg, task, n, caller),
            description: description(cfg, n),
        })
        .collect()
}

/// The marking that bound this name for this caller, `Never` when the derived
/// rule did. A marking names its audience only when it is what made the arg
/// required, which means only when a default exists for it to override. An
/// arg with no default is required of everyone whatever it is marked, so
/// naming an audience there would tell one caller the requirement is theirs
/// alone while the other is refused just the same.
fn binding_reason(cfg: &Config, task: Option<&str>, name: &str, caller: Caller) -> Required {
    if !has_default(cfg, name) {
        return Required::Never;
    }
    applicable(cfg, task, name)
        .into_iter()
        .find(|r| binds(*r, caller))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use devkit_config::Config;

    use super::*;

    fn parse(body: &str) -> Config {
        Config::parse(&format!(
            "[defaults]\nworktree_root='w'\nbranch_prefix='x/'\nbaseline_ref='m'\n{body}"
        ))
        .unwrap()
    }

    /// `msg` is defaulted, `ticket` is declared with no default, `plain` is a
    /// bare constant. The `commit` task reads all three.
    fn cfg(task_marking: &str) -> Config {
        parse(&format!(
            "[templates.variables]\n\
             plain = 'p'\n\
             msg = {{ default = 'wip', required = 'agents' }}\n\
             ticket = {{ required = 'always' }}\n\
             [tasks.commit]\n\
             run = ['git', 'commit', '-m', '{{{{ msg }}}}']\n\
             {task_marking}\n"
        ))
    }

    fn names(v: &[&str]) -> BTreeSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_marking_binds_only_the_caller_it_names() {
        let c = cfg("");
        assert!(is_required(&c, Some("commit"), "msg", Caller::Agent));
        assert!(!is_required(&c, Some("commit"), "msg", Caller::Human));
    }

    #[test]
    fn an_arg_with_no_default_is_required_of_everyone() {
        let c = cfg("");
        for caller in [Caller::Agent, Caller::Human] {
            assert!(is_required(&c, Some("commit"), "ticket", caller));
            assert!(is_required(&c, Some("commit"), "undeclared", caller));
        }
    }

    #[test]
    fn an_unmarked_defaulted_arg_is_required_of_nobody() {
        let c = cfg("");
        for caller in [Caller::Agent, Caller::Human] {
            assert!(!is_required(&c, Some("commit"), "plain", caller));
        }
    }

    #[test]
    fn required_of_names_the_callers_is_required_binds() {
        let c = cfg("");
        for (name, of) in [
            ("msg", Required::Agents),
            ("ticket", Required::Always),
            ("undeclared", Required::Always),
            ("plain", Required::Never),
        ] {
            assert_eq!(required_of(&c, Some("commit"), name), of, "{name}");
            for caller in [Caller::Agent, Caller::Human] {
                assert_eq!(
                    binds(of, caller),
                    is_required(&c, Some("commit"), name, caller),
                    "{name} {caller:?}"
                );
            }
        }
    }

    #[test]
    fn steps_marking_one_arg_for_each_caller_require_it_of_everyone() {
        let c = parse(
            "[templates.variables]\n\
             msg = 'wip'\n\
             [tasks.a]\n\
             run = ['x', '{{ msg }}']\n\
             required_args = { msg = 'agents' }\n\
             [tasks.h]\n\
             run = ['x', '{{ msg }}']\n\
             required_args = { msg = 'humans' }\n\
             [tasks.both]\n\
             steps = [{ task = 'a' }, { task = 'h' }]\n",
        );
        assert_eq!(required_of(&c, Some("both"), "msg"), Required::Always);
    }

    #[test]
    fn a_task_marking_beats_the_variable_marking() {
        let c = cfg("required_args = { msg = 'never' }");
        assert!(!is_required(&c, Some("commit"), "msg", Caller::Agent));
        // ... but only for that task.
        assert!(is_required(&c, None, "msg", Caller::Agent));
    }

    #[test]
    fn missing_args_reports_only_unsupplied_required_reads() {
        let c = cfg("");
        let given = BTreeMap::from([("ticket".to_string(), "T-1".to_string())]);
        let out = missing_args(
            &c,
            Some("commit"),
            &names(&["plain", "msg", "ticket"]),
            &given,
            Caller::Agent,
        );
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].name, "msg");
        assert_eq!(out[0].reason, Required::Agents);
        assert_eq!(out[0].hint(), "--arg msg=... (required for agents)");
    }

    #[test]
    fn a_derived_requirement_hints_without_an_audience() {
        let c = cfg("");
        let out = missing_args(
            &c,
            Some("commit"),
            &names(&["ticket"]),
            &BTreeMap::new(),
            Caller::Human,
        );
        assert_eq!(out[0].hint(), "--arg ticket=...");
    }

    #[test]
    fn a_marking_that_did_not_bind_this_caller_is_left_out_of_the_hint() {
        let c = parse(
            "[templates.variables]\n\
             humans_no_default = { required = 'humans' }\n\
             [tasks.t]\n\
             run = ['x', '{{ humans_no_default }}']\n",
        );
        let out = missing_args(
            &c,
            Some("t"),
            &names(&["humans_no_default"]),
            &BTreeMap::new(),
            Caller::Agent,
        );
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(
            out[0].hint(),
            "--arg humans_no_default=...",
            "the derived floor refused this agent, so naming the humans \
             marking would contradict the refusal it is attached to"
        );
        assert_eq!(out[0].reason, Required::Never);
    }

    #[test]
    fn a_marking_that_bound_this_caller_still_names_its_audience() {
        let c = cfg("");
        let out = missing_args(
            &c,
            Some("commit"),
            &names(&["msg"]),
            &BTreeMap::new(),
            Caller::Agent,
        );
        assert_eq!(out[0].hint(), "--arg msg=... (required for agents)");
    }

    #[test]
    fn a_step_tasks_marking_binds_the_sequence_that_runs_it() {
        let s = "[templates.variables]\n\
             scope = 'chore'\n\
             [tasks.commit]\n\
             run = ['git', 'commit', '-m', '{{ scope }}']\n\
             required_args = { scope = 'agents' }\n\
             [tasks.ship]\n\
             steps = [{ task = 'commit' }]\n";
        let c = parse(s);
        assert!(
            is_required(&c, Some("ship"), "scope", Caller::Agent),
            "running commit as a step must not launder away its marking"
        );
        assert!(!is_required(&c, Some("ship"), "scope", Caller::Human));
    }

    #[test]
    fn a_sequence_can_opt_out_of_a_marking_its_step_carries() {
        let s = "[templates.variables]\n\
             scope = 'chore'\n\
             [tasks.commit]\n\
             run = ['git', 'commit', '-m', '{{ scope }}']\n\
             required_args = { scope = 'agents' }\n\
             [tasks.ship]\n\
             steps = [{ task = 'commit' }]\n\
             required_args = { scope = 'never' }\n";
        let c = parse(s);
        assert!(
            !is_required(&c, Some("ship"), "scope", Caller::Agent),
            "the task the caller named wins, never included"
        );
    }

    #[test]
    fn the_floor_never_reports_an_audience() {
        let s = "[templates.variables]\n\
             agents_no_default = { required = 'agents' }\n\
             [tasks.t]\n\
             run = ['x', '{{ agents_no_default }}']\n";
        let c = parse(s);
        for caller in [Caller::Agent, Caller::Human] {
            let out = missing_args(
                &c,
                Some("t"),
                &names(&["agents_no_default"]),
                &BTreeMap::new(),
                caller,
            );
            assert_eq!(
                out[0].hint(),
                "--arg agents_no_default=...",
                "with no default both callers are refused, so neither is told \
                 the requirement belongs to the other"
            );
        }
    }

    /// Every marking crossed with default-or-not, for both callers. `never` on
    /// an arg with no default is absent: config load refuses it, so
    /// `is_required` never sees it.
    #[test]
    fn every_marking_and_default_combination_for_both_callers() {
        let s = "[templates.variables]\n\
             plain = 'p'\n\
             always_default = { default = 'd', required = 'always' }\n\
             agents_default = { default = 'd', required = 'agents' }\n\
             humans_default = { default = 'd', required = 'humans' }\n\
             overridden = { default = 'd', required = 'always' }\n\
             agents_no_default = { required = 'agents' }\n\
             [tasks.t]\n\
             run = ['x', '{{ plain }}']\n\
             required_args = { overridden = 'never' }\n";
        let c = parse(s);

        // (name, required for an agent, required for a human)
        let rows: &[(&str, bool, bool)] = &[
            // No marking and no default, including a wholly undeclared name.
            ("nowhere_declared", true, true),
            ("plain", false, false),
            ("always_default", true, true),
            ("agents_default", true, false),
            ("humans_default", false, true),
            // The variable says `always`, the task relaxes it to `never`.
            ("overridden", false, false),
            // With no default the floor holds whichever marking, so `agents`
            // still binds the human too.
            ("agents_no_default", true, true),
        ];
        for (name, agent, human) in rows {
            assert_eq!(
                is_required(&c, Some("t"), name, Caller::Agent),
                *agent,
                "{name} for Agent"
            );
            assert_eq!(
                is_required(&c, Some("t"), name, Caller::Human),
                *human,
                "{name} for Human"
            );
        }
    }
}
