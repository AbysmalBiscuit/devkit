//! The caller's tracker as the todo name resolver reads it.

use std::path::Path;

use devkit_common::tracker::{self, TrackerKind};
use devkit_todo::names::TrackerKey;
pub(crate) use devkit_todo::names::Names;

/// The [`TrackerKey`] of the checkout at `cwd`, reading a GitHub issue's
/// short form or URL against the issues repository it names; `None` when its
/// tracker is not GitHub.
pub(crate) fn tracker_key(cwd: &Path) -> Option<Box<TrackerKey>> {
    let tracker = tracker::select(None, &cwd.to_string_lossy(), None)
        .tracker
        .tracker;
    (tracker.kind() == TrackerKind::Github)
        .then(|| Box::new(move |input: &str| tracker.ticket_key(input).ok()) as Box<TrackerKey>)
}
