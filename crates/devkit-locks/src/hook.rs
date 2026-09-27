//! Hook glue on the lock side: holder derivation and the write-enforcement
//! gate. Reading the payload is the `devkit hook` verb's job; the registry
//! decision logic stays in `model`/`store`.

use std::path::Path;

use devkit_common::vcs::Checkout;

/// Whether write enforcement is active for a write originating at `cwd`.
pub fn enforcement_enabled_in(checkout: &Checkout, cwd: &Path) -> bool {
    devkit_common::harness::enforcement_enabled_in(
        checkout,
        cwd,
        "enforce_writes",
        "DEVKIT_ENFORCE_WRITES",
    )
}

/// Two-level holder id: top-level agents are `session_id`; sub-agents are
/// `session_id/agent_id`. The Claude Code payload exposes no deeper ancestry.
pub fn holder_from_fields(session_id: &str, agent_id: Option<&str>) -> String {
    match agent_id {
        Some(a) if !a.is_empty() => format!("{session_id}/{a}"),
        _ => session_id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holder_top_level_is_session() {
        assert_eq!(holder_from_fields("S", None), "S");
    }

    #[test]
    fn holder_subagent_is_session_slash_agent() {
        assert_eq!(holder_from_fields("S", Some("a1")), "S/a1");
    }
}
