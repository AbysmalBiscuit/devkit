//! What the caller establishes about where and how the source runs.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Bash,
    PowerShell,
    Fish,
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
