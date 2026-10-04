//! The activity log's side of the hooks: a subagent run starts and stops, a
//! session's end closes its runs, and every other hook a subagent fires marks
//! it seen for the backstop. Silent: a failed write changes nothing the hook
//! does.

use std::time::SystemTime;

use devkit_todo::activity::{ActivityLog, Event, What};

use super::{HookEvent, payload::Payload};

/// Records what `event` means for the payload's run. Keyed on the raw agent
/// id, so a fork, which has no agent type, still gets a run.
pub(crate) fn observe(payload: &Payload, event: HookEvent) {
    let Some(session) = payload.session_id() else {
        return;
    };
    let log = ActivityLog::open();
    let record = |what| {
        log.record(&Event {
            at: SystemTime::now().into(),
            what,
        })
    };
    let session = session.to_string();
    let agent = payload.agent_id().map(str::to_string);
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
        (_, None) => Ok(()),
    };
}
