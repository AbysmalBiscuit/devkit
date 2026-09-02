//! The two help views: clap's own rendering, and the full command tree.

/// Longest `about` the full-tree view can print without truncating, given the
/// hundred-column line cap and the longest command path in the tree.
pub const ABOUT_MAX: usize = 70;
