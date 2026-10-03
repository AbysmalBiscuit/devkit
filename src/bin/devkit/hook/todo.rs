//! The todo store's side of the hooks: who a payload acts as, and the
//! harness its session belongs to.

use devkit_todo::{Edit, Holder, TodoStore, node::Harness};
use pabal::AnyHarness;

use super::payload;

/// The harnesses whose sessions name a todo node.
pub(crate) fn harness_of(harness: AnyHarness) -> Option<Harness> {
    match harness {
        AnyHarness::ClaudeCode => Some(Harness::Claude),
        AnyHarness::Codex => Some(Harness::Codex),
        AnyHarness::Cursor | AnyHarness::Antigravity => None,
    }
}

pub(crate) fn to_todo_holder(holder: &payload::Holder) -> Holder {
    Holder::new(&**holder)
}

/// Returns every todo `holder` covers from in progress to pending, so a
/// crashed agent's todos do not show as in progress forever, and forgets the
/// lists last injected for it. Silent: a store failure leaves the claims for
/// a person to reset.
pub(crate) fn release(holder: Option<payload::Holder>) {
    if let Some(holder) = holder {
        let holder = to_todo_holder(&holder);
        let _ = std::fs::remove_file(crate::todo::digest_path(&holder));
        let _ = crate::todo::store().apply(&Edit::ReleaseAll { holder });
    }
}
