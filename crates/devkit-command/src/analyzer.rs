//! The invocation pipeline every adapter feeds.

use crate::{budget::Budget, context::Context, model::Analysis};

pub(crate) struct Analyzer<'c> {
    pub(crate) ctx: &'c Context,
    pub(crate) budget: Budget,
    pub(crate) out: Analysis,
}

impl<'c> Analyzer<'c> {
    pub(crate) fn new(ctx: &'c Context) -> Self {
        let _ = (
            crate::ts::parse,
            crate::ts::text,
            crate::ts::is_broken,
            crate::ts::named_children,
        );
        Self {
            ctx,
            budget: Budget::new(ctx.limits),
            out: Analysis::default(),
        }
    }

    pub(crate) fn run_source(mut self, source: &str) -> Analysis {
        let _ = self.ctx.dialect;
        let _ = self.budget.admit(source.len(), 0);
        self.out
    }

    pub(crate) fn run_argv(mut self, _argv: &[String]) -> Analysis {
        let _ = self.budget.limits();
        let _ = self.budget.visit();
        let _ = self.budget.value_fits(0);
        self.out
    }
}
