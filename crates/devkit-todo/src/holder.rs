//! Who acts on a todo: `S` for a session's main agent, `S/a` for its
//! sub-agent `a`, and `human` for a person at the CLI. The format is the one
//! the lock registry uses.

use std::{fmt, ops::Deref};

use serde::{Deserialize, Serialize};

const HUMAN: &str = "human";

/// The variable the pre-tool-use hook sets on a sub-agent's `devkit todo`
/// invocations, naming the sub-agent's holder.
pub const HOLDER_VAR: &str = "DEVKIT_TODO_HOLDER";

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Holder(String);

impl Holder {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn human() -> Self {
        Self(HUMAN.to_string())
    }

    pub fn is_human(&self) -> bool {
        self.0 == HUMAN
    }

    /// The session this holder belongs to: `S` for both `S` and `S/a`.
    pub fn session(&self) -> Holder {
        Holder::new(self.0.split('/').next().unwrap_or(&self.0))
    }

    /// The name a list shows for this holder: `a` for `S/a`, `S` for `S`.
    pub fn name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    /// Whether `self` may act on what `other` holds: its own claims, its
    /// sub-agents' claims, and every claim for a human.
    pub fn covers(&self, other: &Holder) -> bool {
        self.is_human()
            || self.0 == other.0
            || other
                .0
                .strip_prefix(self.0.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    }
}

impl Deref for Holder {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Holder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sub_agent_names_itself_and_its_session() {
        assert_eq!(Holder::new("S/a1").session(), Holder::new("S"));
        assert_eq!(Holder::new("S").session(), Holder::new("S"));
        assert_eq!(Holder::new("S/a1").name(), "a1");
        assert_eq!(Holder::new("S").name(), "S");
    }

    #[test]
    fn covers_itself_its_sub_agents_and_everything_as_human() {
        let covers = |a: &str, b: &str| Holder::new(a).covers(&Holder::new(b));
        assert!(covers("S", "S"));
        assert!(covers("S", "S/a1"));
        assert!(covers("human", "X"));
        assert!(!covers("S/a1", "S"));
        assert!(!covers("S", "S2"));
        assert!(!covers("S/a1", "S/a2"));
    }
}
