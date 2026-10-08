//! The activity log's side of the hooks: a subagent run starts and stops, a
//! session's end closes its runs, and every other hook marks the agent that
//! fired it seen, a subagent for the backstop on its run and a main agent for
//! its session's. Silent: a failed write changes nothing the hook does.

use std::{path::Path, time::Duration};

use devkit_common::vcs::Checkout;
use devkit_todo::activity::{ActivityStore, MAIN_AGENT, What};

use super::{HookEvent, gate, payload::Payload};
use crate::todo::store::Store;

/// How long a hook waits for its activity record once its own work is done:
/// well inside every harness's timeout for the hook, after the verdict.
const OBSERVE_WAIT: Duration = Duration::from_secs(1);

/// [`observe`] on a thread of its own, waited on for at most
/// [`OBSERVE_WAIT`] or what is left of the hook's overall deadline, so a
/// stuck database or CA file never holds the hook past its harness timeout.
/// With no time left nothing is recorded, and a record still unwritten when
/// the wait ends is lost as the hook exits.
pub(crate) fn observe_within(payload: &Payload, event: HookEvent, checkout: &Checkout, cwd: &Path) {
    let wait = super::within_deadline(OBSERVE_WAIT);
    if wait.is_zero() {
        return;
    }
    let (payload, checkout, cwd) = (payload.clone(), checkout.clone(), cwd.to_path_buf());
    let _ = gate::with_deadline(wait, move || {
        observe(&payload, event, &checkout, &cwd);
    });
}

/// Records what `event` means for the payload's run, or its session's main
/// agent, in the log that goes with the checkout's todo store. Keyed on the
/// raw agent id, so a fork, which has no agent type, still gets a run.
fn observe(payload: &Payload, event: HookEvent, checkout: &Checkout, cwd: &Path) {
    let Some(session) = payload.session_id() else {
        return;
    };
    let agent = payload.agent_id().map(str::to_string);
    let log = Store::activity_for_hook(checkout, cwd);
    let record = |what| log.record_now(&what);
    let session = session.to_string();
    let _ = match (event, agent) {
        (HookEvent::SessionEnd, _) => record(What::SessionEnd {
            session: session.clone(),
        })
        .and_then(|()| log.forget(&session, None)),
        (HookEvent::SubagentStart, Some(agent)) => record(What::SubagentStart {
            session: session.clone(),
            agent: agent.clone(),
            agent_type: payload.agent_type().map(str::to_string),
        })
        .and_then(|()| log.seen(&session, &agent)),
        (HookEvent::SubagentStop, Some(agent)) => record(What::SubagentStop {
            session: session.clone(),
            agent: agent.clone(),
        })
        .and_then(|()| log.forget(&session, Some(&agent))),
        (_, Some(agent)) => log.seen(&session, &agent),
        (_, None) => log.seen(&session, MAIN_AGENT),
    };
}

/// Marks the payload's agent seen, for the backstop, without ending its run,
/// in the log that goes with the checkout's todo store, waited on as
/// [`observe_within`] waits.
pub(crate) fn seen_within(payload: &Payload, checkout: &Checkout, cwd: &Path) {
    let wait = super::within_deadline(OBSERVE_WAIT);
    let (Some(session), Some(agent)) = (payload.session_id(), payload.agent_id()) else {
        return;
    };
    if wait.is_zero() {
        return;
    }
    let (session, agent) = (session.to_string(), agent.to_string());
    let (checkout, cwd) = (checkout.clone(), cwd.to_path_buf());
    let _ = gate::with_deadline(wait, move || {
        let _ = Store::activity_for_hook(&checkout, &cwd).seen(&session, &agent);
    });
}
