//! The todo store's side of the hooks: who a payload acts as, and the
//! harness its session belongs to.

use devkit_todo::{Holder, node::Harness};
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
