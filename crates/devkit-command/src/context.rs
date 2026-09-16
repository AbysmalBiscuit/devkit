//! What the caller establishes about where and how the source runs.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Bash,
    PowerShell,
    Fish,
}

impl Dialect {
    /// The name a record stores. Stable across renames of the variant.
    pub fn name(self) -> &'static str {
        match self {
            Dialect::Bash => "bash",
            Dialect::PowerShell => "powershell",
            Dialect::Fish => "fish",
        }
    }

    pub fn from_name(name: &str) -> Option<Dialect> {
        [Dialect::Bash, Dialect::PowerShell, Dialect::Fish]
            .into_iter()
            .find(|d| d.name() == name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathStyle {
    Unix,
    Windows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub outer_source: usize,
    pub cumulative_source: usize,
    pub depth: usize,
    pub nodes: usize,
    pub value: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            outer_source: 256 * 1024,
            cumulative_source: 1024 * 1024,
            depth: 8,
            nodes: 100_000,
            value: 64 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    pub dialect: Dialect,
    pub cwd: Option<String>,
    pub path_style: PathStyle,
    pub limits: Limits,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dialect_name_parses_back() {
        for d in [Dialect::Bash, Dialect::PowerShell, Dialect::Fish] {
            assert_eq!(Dialect::from_name(d.name()), Some(d));
        }
        assert_eq!(Dialect::from_name("zsh"), None);
    }
}
