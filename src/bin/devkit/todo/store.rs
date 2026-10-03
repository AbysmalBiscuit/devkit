//! The one place that names every todo backend.

use std::path::Path;

use ambassador::Delegate;
use anyhow::Result;
use devkit_common::vcs::Checkout;
use devkit_config::{NoConfig, TodoBackend, TodoConfig};
use devkit_todo::{TodoStore, ambassador_impl_TodoStore};
use devkit_todo_builtin::BuiltinStore;
use devkit_todo_taskwarrior::TaskwarriorStore;

/// The store `[todo] backend` names.
#[derive(Delegate)]
#[delegate(TodoStore)]
pub(crate) enum Store {
    Builtin(BuiltinStore),
    Taskwarrior(TaskwarriorStore),
}

impl Store {
    pub(crate) fn from_config(config: &TodoConfig) -> Self {
        match config.backend {
            TodoBackend::Builtin => Self::Builtin(BuiltinStore::open()),
            TodoBackend::Taskwarrior => {
                Self::Taskwarrior(TaskwarriorStore::new(&config.taskwarrior.path))
            }
        }
    }

    /// The store a CLI call in `cwd` uses. No config anywhere, as in a cloud
    /// session, is the built-in store; a config that fails to load is an
    /// error.
    pub(crate) fn for_cli(cwd: &Path) -> Result<Self> {
        match devkit_common::config::resolve(None, cwd) {
            Ok((config, _)) => Ok(Self::from_config(&config.todo)),
            Err(e) if e.downcast_ref::<NoConfig>().is_some() => {
                Ok(Self::from_config(&TodoConfig::default()))
            }
            Err(e) => Err(e),
        }
    }

    /// The store a hook in `cwd` uses: the built-in store whenever the config
    /// fails to load, since a hook never fails on its account.
    pub(crate) fn for_hook(checkout: &Checkout, cwd: &Path) -> Self {
        let todo = devkit_common::config::resolve_in(checkout, None, cwd)
            .map(|(config, _)| config.todo)
            .unwrap_or_default();
        Self::from_config(&todo)
    }
}
