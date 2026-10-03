//! Where a todo list lives. Nodes nest on `.`: `global`, `<repo>`,
//! `<repo>.<branch>` and `<repo>.<branch>.<harness>-<session>`. The format
//! matches alacritree's `alacritree_tasks::scope::node` byte for byte, so both
//! tools address the same lists.

use std::path::Path;

use devkit_common::vcs::Checkout;
use devkit_vcs::{DETACHED, Worktree};

pub const GLOBAL: &str = "global";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Harness {
    Claude,
    Codex,
}

impl Harness {
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// An agent conversation, keyed by the harness's own id so a resumed
/// conversation finds its todos again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRef {
    pub harness: Harness,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    Global,
    Project { repo: String },
    Workspace { repo: String, branch: String },
}

/// Nodes nest on `.`, so a dot inside one segment would invent a level.
pub fn sanitize(segment: &str) -> String {
    segment
        .chars()
        .map(|c| {
            if matches!(c, '.' | '/' | '\\') {
                '-'
            } else {
                c
            }
        })
        .collect()
}

/// A session only means something inside a workspace; above one there is no
/// conversation to key.
pub fn node(place: &Place, session: Option<&SessionRef>) -> String {
    match place {
        Place::Global => GLOBAL.to_string(),
        Place::Project { repo } => sanitize(repo),
        Place::Workspace { repo, branch } => {
            let workspace = format!("{}.{}", sanitize(repo), sanitize(branch));
            match session {
                Some(s) => format!("{workspace}.{}-{}", s.harness.prefix(), sanitize(&s.id)),
                None => workspace,
            }
        }
    }
}

/// The session a CLI call runs in. Codex wins when both variables are set, as
/// alacritree's `scope::session_from_env` decides, so both tools name the same
/// node.
pub fn session_from_env(get: impl Fn(&str) -> Option<String>) -> Option<SessionRef> {
    [
        ("CODEX_SESSION_ID", Harness::Codex),
        ("CLAUDE_CODE_SESSION_ID", Harness::Claude),
    ]
    .into_iter()
    .find_map(|(key, harness)| {
        let id = get(key)?.trim().to_string();
        (!id.is_empty()).then_some(SessionRef { harness, id })
    })
}

/// The nodes an agent at `place` sees, deepest first: its own and every node
/// above it, never a sibling session's.
pub fn visible_nodes(place: &Place, session: Option<&SessionRef>) -> Vec<String> {
    let mut out = Vec::new();
    if let Place::Workspace { .. } = place {
        if session.is_some() {
            out.push(node(place, session));
        }
        out.push(node(place, None));
    }
    if let Place::Workspace { repo, .. } | Place::Project { repo } = place {
        out.push(node(&Place::Project { repo: repo.clone() }, None));
    }
    out.push(GLOBAL.to_string());
    out
}

fn basename(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The main checkout names the repository for every worktree of it. A head
/// that names no branch is named by its directory.
pub fn place_from(main: Option<&Worktree>, here: Option<&Worktree>) -> Place {
    let Some(main) = main else {
        return Place::Global;
    };
    let name = basename(&main.path);
    // A bare repository's directory carries a `.git` suffix its name does not.
    let repo = match name.strip_suffix(".git") {
        Some(stripped) if main.bare => stripped.to_string(),
        _ => name,
    };
    let Some(here) = here else {
        return Place::Project { repo };
    };
    let branch = if here.branch == DETACHED {
        basename(&here.path)
    } else {
        here.branch.clone()
    };
    Place::Workspace { repo, branch }
}

pub fn place_of(checkout: &Checkout) -> Place {
    place_from(checkout.worktrees().first(), checkout.here())
}

/// Which nodes a listing covers. A todo matching none of them stays out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    pub nodes: Vec<NodeMatch>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeMatch {
    /// Todos on exactly this node.
    Exact(String),
    /// Todos on this node or any node below it. `r` covers `r.main` and
    /// `r.main.codex-1`, never a sibling repository named `r-web`.
    Subtree(String),
}

impl Filter {
    /// Every todo, whatever its node.
    pub fn all() -> Self {
        Self {
            nodes: vec![NodeMatch::Subtree(String::new())],
        }
    }

    pub fn exact(nodes: impl IntoIterator<Item = String>) -> Self {
        Self {
            nodes: nodes.into_iter().map(NodeMatch::Exact).collect(),
        }
    }

    /// `project` is a todo's node, `None` for the global list.
    pub fn matches(&self, project: Option<&str>) -> bool {
        let node = project.unwrap_or(GLOBAL);
        self.nodes.iter().any(|m| match m {
            NodeMatch::Exact(n) => node == n,
            NodeMatch::Subtree(n) if n.is_empty() => true,
            NodeMatch::Subtree(n) => node
                .strip_prefix(n.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('.')),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use devkit_vcs::{DETACHED, Worktree};

    use super::*;

    fn ws(repo: &str, branch: &str) -> Place {
        Place::Workspace {
            repo: repo.into(),
            branch: branch.into(),
        }
    }

    fn codex(id: &str) -> SessionRef {
        SessionRef {
            harness: Harness::Codex,
            id: id.into(),
        }
    }

    fn claude(id: &str) -> SessionRef {
        SessionRef {
            harness: Harness::Claude,
            id: id.into(),
        }
    }

    fn wt(path: &str, branch: &str, bare: bool) -> Worktree {
        Worktree {
            path: PathBuf::from(path),
            branch: branch.into(),
            bare,
            locked: false,
        }
    }

    #[test]
    fn each_place_names_its_node() {
        assert_eq!(node(&Place::Global, None), "global");
        assert_eq!(
            node(
                &Place::Project {
                    repo: "devkit".into()
                },
                None
            ),
            "devkit"
        );
        assert_eq!(node(&ws("devkit", "master"), None), "devkit.master");
        assert_eq!(
            node(&ws("alacritree", "master"), Some(&codex("0199a"))),
            "alacritree.master.codex-0199a"
        );
    }

    #[test]
    fn a_session_below_anything_but_a_workspace_is_dropped() {
        assert_eq!(node(&Place::Global, Some(&codex("x"))), "global");
        assert_eq!(
            node(&Place::Project { repo: "r".into() }, Some(&codex("x"))),
            "r"
        );
    }

    #[test]
    fn dots_and_slashes_never_add_levels() {
        let name = node(&ws("my.repo", "feat/v1.2"), Some(&claude("a.b/c")));
        assert_eq!(name, "my-repo.feat-v1-2.claude-a-b-c");
        assert_eq!(name.matches('.').count(), 2);
    }

    #[test]
    fn codex_session_id_wins_over_claude() {
        let env = |key: &str| match key {
            "CODEX_SESSION_ID" => Some("c1".to_string()),
            "CLAUDE_CODE_SESSION_ID" => Some("k1".to_string()),
            _ => None,
        };
        assert_eq!(session_from_env(env), Some(codex("c1")));
    }

    #[test]
    fn claude_session_id_is_read_when_codex_is_absent() {
        let env = |key: &str| (key == "CLAUDE_CODE_SESSION_ID").then(|| "k1".to_string());
        assert_eq!(session_from_env(env), Some(claude("k1")));
    }

    #[test]
    fn blank_ids_are_no_session() {
        let env = |key: &str| (key == "CODEX_SESSION_ID").then(|| "  ".to_string());
        assert_eq!(session_from_env(env), None);
    }

    #[test]
    fn a_subtree_keeps_other_repositories_out() {
        let filter = Filter {
            nodes: vec![NodeMatch::Subtree("r".into())],
        };
        assert!(filter.matches(Some("r")));
        assert!(filter.matches(Some("r.main.codex-1")));
        assert!(!filter.matches(Some("r-web")));
        assert!(!filter.matches(Some("rr.main")));
    }

    #[test]
    fn a_todo_without_a_project_is_global() {
        let filter = Filter {
            nodes: vec![NodeMatch::Exact(GLOBAL.into())],
        };
        assert!(filter.matches(None));
        assert!(!filter.matches(Some("r")));
    }

    #[test]
    fn a_session_sees_its_own_node_and_every_node_above_it() {
        assert_eq!(visible_nodes(&ws("r", "main"), Some(&claude("s"))), [
            "r.main.claude-s",
            "r.main",
            "r",
            "global"
        ]);
        assert_eq!(visible_nodes(&Place::Global, Some(&claude("s"))), [
            "global"
        ]);
    }

    #[test]
    fn the_main_checkout_names_the_repo_and_the_current_one_the_branch() {
        let main = wt("/x/devkit", "main", false);
        let here = wt("/x/devkit_worktrees/a", "feat/a", false);
        assert_eq!(place_from(Some(&main), Some(&here)), ws("devkit", "feat/a"));
    }

    #[test]
    fn a_bare_repo_drops_its_git_suffix() {
        let main = wt("/x/devkit.git", "main", true);
        assert_eq!(place_from(Some(&main), None), Place::Project {
            repo: "devkit".into()
        });
    }

    #[test]
    fn a_working_copy_keeps_a_git_suffix_in_its_name() {
        let main = wt("/x/tooling.git", "main", false);
        assert_eq!(
            place_from(Some(&main), Some(&main)),
            ws("tooling.git", "main")
        );
    }

    #[test]
    fn a_detached_head_is_named_by_its_directory() {
        let main = wt("/x/devkit", "main", false);
        let here = wt("/x/wt-3", DETACHED, false);
        assert_eq!(place_from(Some(&main), Some(&here)), ws("devkit", "wt-3"));
    }

    #[test]
    fn nothing_located_is_global() {
        assert_eq!(place_from(None, None), Place::Global);
    }

    #[test]
    fn a_repository_without_a_checkout_here_is_the_project() {
        let main = wt("/x/devkit", "main", false);
        assert_eq!(place_from(Some(&main), None), Place::Project {
            repo: "devkit".into()
        });
    }
}
