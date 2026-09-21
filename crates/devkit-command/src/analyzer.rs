//! The invocation pipeline every adapter feeds.

#![allow(dead_code)]

use std::ops::Range;

use crate::{
    budget::Budget,
    catalog,
    context::{Context, Dialect},
    embed,
    model::{
        Analysis, FileEffect, FileOp, Invocation, Language, Location, ScriptFileInvocation,
        TreeEffect, TreeReach, Uncertainty, UncertaintyKind, Value,
    },
    normalize, paths,
};

#[derive(Debug, Clone)]
pub(crate) struct Word {
    pub(crate) value: Value,
    pub(crate) typed: String,
    pub(crate) span: Range<usize>,
}

#[derive(Debug, Clone)]
pub(crate) enum Stdin {
    None,
    Source { value: Value, span: Range<usize> },
}

#[derive(Debug, Clone)]
pub(crate) struct RawInvocation {
    pub(crate) words: Vec<Word>,
    pub(crate) stdin: Stdin,
    pub(crate) cwd: Option<String>,
    pub(crate) language: Language,
    pub(crate) location: Location,
}

#[derive(Debug, Clone)]
pub(crate) struct Frame {
    pub(crate) depth: usize,
    pub(crate) script_args: Vec<Value>,
    pub(crate) base: Option<Location>,
    pub(crate) cwd: Option<String>,
}

impl Frame {
    pub(crate) fn outer() -> Self {
        Self {
            depth: 0,
            script_args: Vec::new(),
            base: None,
            cwd: None,
        }
    }

    pub(crate) fn locate(&self, span: Range<usize>) -> Location {
        match &self.base {
            None => Location {
                outer: span,
                embedded: None,
            },
            Some(base) => Location {
                outer: base.outer.clone(),
                embedded: Some(span),
            },
        }
    }
}

pub(crate) struct Analyzer<'c> {
    pub(crate) ctx: &'c Context,
    pub(crate) budget: Budget,
    pub(crate) out: Analysis,
    fresh: Vec<FreshWrite>,
}

/// An effect on a path an API created fresh, and whether it put something
/// there from outside the fresh directory.
struct FreshWrite {
    location: Location,
    places: bool,
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
            fresh: Vec::new(),
        }
    }

    pub(crate) fn run_source(mut self, source: &str) -> Analysis {
        let language = match self.ctx.dialect {
            Dialect::Bash => Language::Bash,
            Dialect::PowerShell => Language::PowerShell,
            Dialect::Fish => Language::Fish,
        };
        let frame = Frame {
            cwd: self.ctx.cwd.clone(),
            ..Frame::outer()
        };
        self.source(language, source, frame);
        self.close_fresh();
        self.out
    }

    pub(crate) fn run_argv(mut self, argv: &[String]) -> Analysis {
        let _ = (
            self.budget.limits(),
            self.budget.visit(),
            self.budget.value_fits(0),
        );
        let words = argv
            .iter()
            .map(|w| Word {
                value: Value::Known(w.clone()),
                typed: w.clone(),
                span: 0..0,
            })
            .collect();
        let raw = RawInvocation {
            words,
            stdin: Stdin::None,
            cwd: self.ctx.cwd.clone(),
            language: Language::Bash,
            location: Location {
                outer: 0..0,
                embedded: None,
            },
        };
        let frame = Frame {
            cwd: self.ctx.cwd.clone(),
            ..Frame::outer()
        };
        self.invocation(raw, &frame);
        self.close_fresh();
        self.out
    }

    pub(crate) fn source(&mut self, language: Language, text: &str, frame: Frame) {
        let at = frame.locate(0..text.len());
        if let Err(limit) = self.budget.admit(text.len(), frame.depth) {
            self.uncertain(
                UncertaintyKind::LimitExhausted(limit),
                format!("{language:?} source was not analyzed"),
                at,
            );
            return;
        }
        match language {
            Language::Bash => crate::bash::walk(self, text, &frame),
            Language::Fish => crate::fish::walk(self, text, &frame),
            Language::PowerShell => crate::powershell::walk(self, text, &frame),
            Language::Python => crate::python::walk(self, text, &frame),
            Language::JavaScript | Language::TypeScript => {
                crate::js::walk(self, language, text, &frame)
            }
        }
    }

    pub(crate) fn uncertain(
        &mut self,
        kind: UncertaintyKind,
        detail: impl Into<String>,
        location: Location,
    ) {
        self.out.uncertainties.push(Uncertainty {
            kind,
            detail: detail.into(),
            location,
        });
    }

    /// Record a write to `path`. A copy's target is always its destination, so
    /// a copy onto a fresh path places something there.
    pub(crate) fn file_effect(
        &mut self,
        op: FileOp,
        path: &Value,
        cwd: Option<&str>,
        location: Location,
    ) {
        self.record_file(op, path, cwd, location, op == FileOp::Copy);
    }

    /// Record the destination of a rename or move, which places whatever was
    /// renamed there.
    pub(crate) fn rename_into(&mut self, dest: &Value, cwd: Option<&str>, location: Location) {
        self.record_file(FileOp::Rename, dest, cwd, location, true);
    }

    fn record_file(
        &mut self,
        op: FileOp,
        path: &Value,
        cwd: Option<&str>,
        location: Location,
        places: bool,
    ) {
        let target = paths::resolve(path, cwd, self.ctx.path_style);
        if matches!(target, crate::model::Target::Ephemeral { .. }) {
            self.fresh.push(FreshWrite {
                location: location.clone(),
                places,
            });
        }
        self.out.file_effects.push(FileEffect {
            op,
            target,
            location,
        });
    }

    /// Record a tree writer that fills its scope: a recursive copy, a move, an
    /// extraction, a formatter. Any of them can put a link there.
    pub(crate) fn tree_effect(
        &mut self,
        scope: &Value,
        whole_checkout: bool,
        cwd: Option<&str>,
        by: &str,
        location: Location,
    ) {
        self.record_tree(scope, whole_checkout, cwd, by, location, true);
    }

    /// Record a tree writer that only takes entries away: a recursive delete,
    /// or the source side of a directory move.
    pub(crate) fn tree_removal(
        &mut self,
        scope: &Value,
        cwd: Option<&str>,
        by: &str,
        location: Location,
    ) {
        self.record_tree(scope, false, cwd, by, location, false);
    }

    fn record_tree(
        &mut self,
        scope: &Value,
        whole_checkout: bool,
        cwd: Option<&str>,
        by: &str,
        location: Location,
        places: bool,
    ) {
        match paths::resolve(scope, cwd, self.ctx.path_style) {
            crate::model::Target::Path(scope) => self.out.tree_effects.push(TreeEffect {
                scope,
                reach: TreeReach::All,
                whole_checkout,
                by: by.to_string(),
                location,
            }),
            crate::model::Target::Unresolved => self.uncertain(
                UncertaintyKind::UnresolvedWrite,
                format!("`{by}` rewrites a directory that could not be determined"),
                location,
            ),
            // A tree writer rooted at a freshly created temp directory reaches
            // no path another session could name, but a claim on the directory
            // that one was made in covers the whole fresh subtree.
            crate::model::Target::Ephemeral { at } => {
                self.fresh.push(FreshWrite {
                    location: location.clone(),
                    places,
                });
                if let crate::model::TempLocation::In(scope) = at {
                    self.out.tree_effects.push(TreeEffect {
                        scope,
                        reach: TreeReach::FreshSubtree,
                        whole_checkout,
                        by: by.to_string(),
                        location,
                    });
                }
            }
        }
    }

    /// Once anything is moved, copied, or unpacked into a fresh directory, the
    /// other fresh writes stop being exempt: what was placed may be a link, and
    /// a write through it lands wherever it points. Fresh values carry no
    /// identity and a loop can run a later statement first, so this spans the
    /// whole analysis. A lone placement has nothing before it to lead it out.
    fn close_fresh(&mut self) {
        let placements = self.fresh.iter().filter(|f| f.places).count();
        if placements == 0 {
            return;
        }
        for f in std::mem::take(&mut self.fresh) {
            if placements == 1 && f.places {
                continue;
            }
            self.uncertain(
                UncertaintyKind::UnresolvedWrite,
                "something moved, copied, or unpacked into a fresh directory can be a link \
                 leading out of it",
                f.location,
            );
        }
    }

    pub(crate) fn invocation(&mut self, raw: RawInvocation, frame: &Frame) {
        let unwrapped = normalize::unwrap(self, &raw, frame);
        let Some(program) = unwrapped.argv.first() else {
            return;
        };
        let location = raw.location.clone();
        let cwd = unwrapped.cwd.apply(raw.cwd.clone());
        let program_value = program.value.clone();
        let args: Vec<Value> = unwrapped.argv[1..]
            .iter()
            .map(|w| w.value.clone())
            .collect();
        let git =
            normalize::program_options(&program_value, &args, cwd.as_deref(), self.ctx.path_style);

        self.out.invocations.push(Invocation {
            program: program_value.clone(),
            args: args.clone(),
            semantic_args: git.semantic_args.clone(),
            wrappers: unwrapped
                .wrappers
                .iter()
                .map(|ws| ws.iter().map(|w| w.value.clone()).collect())
                .collect(),
            typed: raw.words.iter().map(|w| w.typed.clone()).collect(),
            cwd: cwd.clone(),
            language: raw.language,
            depth: frame.depth,
            location: location.clone(),
        });

        let Some(name) = program_value
            .known()
            .map(normalize::basename)
            .map(str::to_string)
        else {
            self.uncertain(
                UncertaintyKind::UnresolvedInvocation,
                format!("the program `{}` could not be determined", program.typed),
                location,
            );
            return;
        };

        match embed::classify(&name, &args, &raw.stdin) {
            embed::Exec::Source {
                language,
                source,
                script_args,
            } => match source.known() {
                Some(text) => {
                    let child = Frame {
                        depth: frame.depth + 1,
                        script_args,
                        base: Some(location.clone()),
                        cwd: cwd.clone(),
                    };
                    self.source(language, text, child);
                }
                None => self.uncertain(
                    UncertaintyKind::UnresolvedWrite,
                    format!("`{name}` runs source that could not be determined"),
                    location.clone(),
                ),
            },
            embed::Exec::ScriptFile { script } => {
                self.out.script_files.push(ScriptFileInvocation {
                    interpreter: Some(name.clone()),
                    script,
                    location: location.clone(),
                })
            }
            embed::Exec::Unsupported { language } => self.uncertain(
                UncertaintyKind::UnsupportedLanguage(language.to_string()),
                format!("`{name}` runs {language} source, which devkit cannot analyze"),
                location.clone(),
            ),
            embed::Exec::Plain => {}
        }

        let effective_cwd = git.cwd.apply(cwd.clone());
        for hit in catalog::effects(&name, &git.semantic_args) {
            match hit {
                catalog::Hit::File(op, path) => {
                    self.file_effect(op, &path, effective_cwd.as_deref(), location.clone())
                }
                catalog::Hit::Tree {
                    scope,
                    whole_checkout,
                    by,
                } => self.tree_effect(
                    &scope,
                    whole_checkout,
                    effective_cwd.as_deref(),
                    &by,
                    location.clone(),
                ),
                catalog::Hit::RenameInto(path) => {
                    self.rename_into(&path, effective_cwd.as_deref(), location.clone())
                }
                catalog::Hit::TreeRemoval { scope, by } => {
                    self.tree_removal(&scope, effective_cwd.as_deref(), &by, location.clone())
                }
                catalog::Hit::Unresolved(detail) => {
                    self.uncertain(UncertaintyKind::UnresolvedWrite, detail, location.clone())
                }
                catalog::Hit::ScriptFile(script) => {
                    self.out.script_files.push(ScriptFileInvocation {
                        interpreter: None,
                        script,
                        location: location.clone(),
                    })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        context::{Context, Dialect, Limits, PathStyle},
        model::{FileOp, Language, Location, Target, UncertaintyKind, Value},
    };

    pub(crate) fn ctx() -> Context {
        Context {
            dialect: Dialect::Bash,
            cwd: Some("/repo".into()),
            path_style: PathStyle::Unix,
            limits: Limits::default(),
        }
    }

    fn word(s: &str) -> Word {
        Word {
            value: Value::Known(s.into()),
            typed: s.into(),
            span: 0..s.len(),
        }
    }
    fn at() -> Location {
        Location {
            outer: 0..1,
            embedded: None,
        }
    }

    #[test]
    fn an_invocation_is_recorded_with_its_cwd() {
        let c = ctx();
        let mut a = Analyzer::new(&c);
        a.invocation(
            RawInvocation {
                words: vec![word("ls"), word("-la")],
                stdin: Stdin::None,
                cwd: Some("/repo".into()),
                language: Language::Bash,
                location: at(),
            },
            &Frame::outer(),
        );
        let inv = &a.out.invocations[0];
        assert_eq!(inv.program, Value::Known("ls".into()));
        assert_eq!(inv.args, vec![Value::Known("-la".into())]);
        assert_eq!(inv.cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn an_unknown_program_word_is_an_unresolved_invocation() {
        let c = ctx();
        let mut a = Analyzer::new(&c);
        a.invocation(
            RawInvocation {
                words: vec![Word {
                    value: Value::Unknown,
                    typed: "$cmd".into(),
                    span: 0..4,
                }],
                stdin: Stdin::None,
                cwd: None,
                language: Language::Bash,
                location: at(),
            },
            &Frame::outer(),
        );
        assert_eq!(
            a.out.uncertainties[0].kind,
            UncertaintyKind::UnresolvedInvocation
        );
    }

    #[test]
    fn a_file_effect_resolves_against_the_given_cwd() {
        let c = ctx();
        let mut a = Analyzer::new(&c);
        a.file_effect(
            FileOp::Overwrite,
            &Value::Known("out.txt".into()),
            Some("/repo/sub"),
            at(),
        );
        assert_eq!(
            a.out.file_effects[0].target,
            Target::Path("/repo/sub/out.txt".into())
        );
    }
}
