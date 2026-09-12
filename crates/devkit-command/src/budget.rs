//! Limit accounting shared across one analysis, nested sources included.

use crate::{context::Limits, model::Limit};

#[derive(Debug)]
pub(crate) struct Budget {
    limits: Limits,
    nodes: usize,
    source: usize,
}

impl Budget {
    pub(crate) fn new(limits: Limits) -> Self {
        Self {
            limits,
            nodes: 0,
            source: 0,
        }
    }

    pub(crate) fn limits(&self) -> Limits {
        self.limits
    }

    pub(crate) fn visit(&mut self) -> Result<(), Limit> {
        self.nodes += 1;
        if self.nodes > self.limits.nodes {
            Err(Limit::Nodes)
        } else {
            Ok(())
        }
    }

    pub(crate) fn admit(&mut self, len: usize, depth: usize) -> Result<(), Limit> {
        if depth == 0 && len > self.limits.outer_source {
            return Err(Limit::OuterSource);
        }
        if depth > self.limits.depth {
            return Err(Limit::Depth);
        }
        if self.source + len > self.limits.cumulative_source {
            return Err(Limit::CumulativeSource);
        }
        self.source += len;
        Ok(())
    }

    pub(crate) fn value_fits(&self, len: usize) -> bool {
        len <= self.limits.value
    }
}
