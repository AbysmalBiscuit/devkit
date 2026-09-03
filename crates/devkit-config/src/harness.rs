//! The `[harness]` table: the coding-agent enforcement opt-ins.

use serde::Deserialize;

/// The `[harness]` table of a checkout's `devkit.toml`.
///
/// This is the shape `devkit schema` renders. Nothing at runtime deserializes
/// the table through it: the probe reads each key independently so one bad key
/// cannot take the others down with it.
#[derive(Deserialize, Default, Debug, Clone, PartialEq, schemars::JsonSchema)]
pub struct HarnessSection {
    /// Refuse writes to paths this checkout has not claimed with `lockm`.
    #[serde(default)]
    pub enforce_writes: bool,
}
