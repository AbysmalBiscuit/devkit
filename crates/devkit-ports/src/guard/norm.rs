//! What the guard still needs after shared analysis has unwrapped a command:
//! a program's basename and the Doppler wrapper's identity.

use devkit_command::{Invocation, Value};

/// A Doppler wrapper's identity, normalized so `-c dev` and `--config dev`
/// compare equal.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Doppler {
    pub config: Option<String>,
    pub project: Option<String>,
}

/// The last path component of a program word, without a Windows `.exe`.
pub fn basename(prog: &str) -> &str {
    let base = prog.rsplit(['/', '\\']).next().unwrap_or(prog);
    base.strip_suffix(".exe").unwrap_or(base)
}

/// The Doppler wrapper the analysis removed to reach `inv`, if any.
pub fn doppler_of(inv: &Invocation) -> Option<Doppler> {
    inv.wrappers
        .iter()
        .find(|wrapper| wrapper.first().and_then(Value::known).map(basename) == Some("doppler"))
        .map(|wrapper| {
            let (config, project) = devkit_command::doppler_flags(wrapper);
            Doppler { config, project }
        })
}
