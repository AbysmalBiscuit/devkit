//! Whether a caller must supply a given `--arg`. The single door: every
//! `--arg` surface asks here, and nothing re-derives the answer. An earlier
//! attempt at this feature re-derived it inline for the task listing, and the
//! listing then disagreed with the check that refused the run.

use std::collections::{BTreeMap, BTreeSet};

use devkit_config::{Config, Required};

pub use crate::caller::Caller;

/// A required arg the caller did not supply. `reason` is the marking that
/// bound it, or `Never` when the derived rule did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    pub name: String,
    pub reason: Required,
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

/// Whether a marking binds this caller.
pub fn binds(r: Required, caller: Caller) -> bool {
    match r {
        Required::Never => false,
        Required::Always => true,
        Required::Agents => caller == Caller::Agent,
        Required::Humans => caller == Caller::Human,
    }
}

/// The marking in force: the task's entry, else the variable's, else none.
pub fn declared_required(cfg: &Config, task: Option<&str>, name: &str) -> Required {
    if let Some(t) = task.and_then(|t| cfg.tasks.get(t))
        && let Some(r) = t.required_args.get(name)
    {
        return *r;
    }
    cfg.templates
        .variables
        .get(name)
        .map(|d| d.required())
        .unwrap_or_default()
}

/// The derived rule ORed with the marking. The derived rule is a floor, so a
/// marking can only add a requirement, never remove one.
pub fn is_required(cfg: &Config, task: Option<&str>, name: &str, caller: Caller) -> bool {
    let has_default = cfg
        .templates
        .variables
        .get(name)
        .and_then(|d| d.default_value())
        .is_some();
    !has_default || binds(declared_required(cfg, task, name), caller)
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
        })
        .collect()
}

/// The marking that bound this name for this caller, `Never` when the derived
/// rule did. A marking names its audience only when it is what made the arg
/// required. An arg with no default is required of everyone, so a `humans`
/// marking on one would otherwise tell an agent the arg is "required for
/// humans" while refusing the agent's own run.
fn binding_reason(cfg: &Config, task: Option<&str>, name: &str, caller: Caller) -> Required {
    let declared = declared_required(cfg, task, name);
    if binds(declared, caller) {
        declared
    } else {
        Required::Never
    }
}

#[cfg(test)]
mod tests {
    use devkit_config::Config;

    use super::*;

    /// `msg` is defaulted, `ticket` is declared with no default, `plain` is a
    /// bare constant. The `commit` task reads all three.
    fn cfg(task_marking: &str) -> Config {
        let s = format!(
            "[defaults]\nworktree_root='w'\nbranch_prefix='x/'\nbaseline_ref='m'\n\
             [templates.variables]\n\
             plain = 'p'\n\
             msg = {{ default = 'wip', required = 'agents' }}\n\
             ticket = {{ required = 'always' }}\n\
             [tasks.commit]\n\
             run = ['git', 'commit', '-m', '{{{{ msg }}}}']\n\
             {task_marking}\n"
        );
        Config::parse(&s).unwrap()
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
    fn a_task_marking_beats_the_variable_marking() {
        let c = cfg("required_args = { msg = 'never' }");
        assert!(!is_required(&c, Some("commit"), "msg", Caller::Agent));
        // ... but only for that task.
        assert!(is_required(&c, None, "msg", Caller::Agent));
    }

    #[test]
    fn a_task_marking_cannot_lower_the_derived_floor() {
        let c = cfg("required_args = { ticket = 'never' }");
        assert!(
            is_required(&c, Some("commit"), "ticket", Caller::Human),
            "never cannot relax an arg with no default"
        );
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
        let s = "[defaults]\nworktree_root='w'\nbranch_prefix='x/'\nbaseline_ref='m'\n\
             [templates.variables]\n\
             humans_no_default = { required = 'humans' }\n\
             [tasks.t]\n\
             run = ['x', '{{ humans_no_default }}']\n";
        let c = Config::parse(s).unwrap();
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

    /// The design spec's truth table, crossed with both callers. Row eight
    /// (`never` on a no-default variable) is excluded:
    /// `reject_never_without_default` refuses that combination at config
    /// load, so `is_required` never sees it.
    #[test]
    fn the_truth_table_holds_for_both_callers() {
        let s = "[defaults]\nworktree_root='w'\nbranch_prefix='x/'\nbaseline_ref='m'\n\
             [templates.variables]\n\
             plain = 'p'\n\
             always_default = { default = 'd', required = 'always' }\n\
             agents_default = { default = 'd', required = 'agents' }\n\
             humans_default = { default = 'd', required = 'humans' }\n\
             overridden = { default = 'd', required = 'always' }\n\
             agents_no_default = { required = 'agents' }\n\
             [tasks.t]\n\
             run = ['x', '{{ plain }}']\n\
             required_args = { overridden = 'never' }\n";
        let c = Config::parse(s).unwrap();

        // (name, required for an agent, required for a human)
        let rows: &[(&str, bool, bool)] = &[
            // row 1: no marking, no default (also covers a wholly undeclared name).
            ("nowhere_declared", true, true),
            // row 2: no marking, has default.
            ("plain", false, false),
            // row 3: `always`, has default.
            ("always_default", true, true),
            // row 4: `agents`, has default.
            ("agents_default", true, false),
            // row 5: `humans`, has default.
            ("humans_default", false, true),
            // row 6: variable says `always`, the task relaxes it to `never`.
            ("overridden", false, false),
            // row 7: a marking with no default; the floor holds regardless of
            // which marking, so `agents` still binds the human too.
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
