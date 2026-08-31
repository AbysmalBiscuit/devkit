//! The workspace's shared worker pool.
//!
//! One bounded `rayon::ThreadPool` serves every parallel feature in devkit,
//! rather than each building its own. Several agent sessions run devkit at once
//! on a machine, so per-feature pools would multiply thread count by feature
//! count with nothing coordinating them.
//!
//! Go through [`install`] and [`jwalk_parallelism`] rather than `par_iter` or
//! jwalk's default parallelism directly. Both of those reach rayon's global
//! pool, which has its own width and is the collision this module prevents.

use std::num::NonZeroUsize;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Threads when neither the environment nor the config says otherwise. The
/// measured throughput knee for file copying.
const DEFAULT_THREADS: usize = 4;

/// How long jwalk waits for its first reader task to actually start running
/// on the pool before giving up and returning a busy error instead of
/// hanging.
///
/// Live only for a walk that calls [`jwalk_parallelism`] from outside
/// [`install`], which is the only way to get `RayonExistingPool`: `Serial`'s
/// jwalk timeout is unconditionally `None`. Such a walk queues onto the same
/// bounded pool as whatever else devkit runs concurrently, so this timeout is
/// its backstop. Ten seconds is generous next to how quickly one directory
/// read finishes, so ordinary contention between concurrent walks and copies
/// does not trip it.
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

static CONFIGURED: OnceLock<NonZeroUsize> = OnceLock::new();
static POOL: OnceLock<Option<Arc<rayon::ThreadPool>>> = OnceLock::new();

/// Record the pool's width from config. The first call wins, and a call made
/// once the pool has been built is dropped: the pool is sized once and cannot
/// be resized, so accepting a later width would make [`width`] report a number
/// the running pool does not have. This belongs beside the config load rather
/// than at a use site.
pub fn configure(threads: Option<NonZeroUsize>) {
    if POOL.get().is_some() {
        return;
    }
    if let Some(n) = threads {
        let _ = CONFIGURED.set(n);
    }
}

/// The pool's width: the running pool's own thread count once it is built, and
/// the width it will be built at until then. Where the pool could not be built
/// at all this reports the width that was asked for, while [`install`] runs the
/// work on the calling thread: the requested width is the honest answer to what
/// was configured, and there is no pool whose count could be read instead. `DEVKIT_THREADS` wins over
/// [`configure`], which wins over [`DEFAULT_THREADS`]. An unparseable or zero
/// env value is ignored rather than treated as a request, because
/// `ThreadPoolBuilder::num_threads(0)` means one thread per core: the opposite
/// of what someone capping threads intends.
pub fn width() -> usize {
    if let Some(Some(p)) = POOL.get() {
        return p.current_num_threads();
    }
    requested_width()
}

/// The width the pool will be built at. Read from inside the pool's own
/// initialiser, where [`width`] would find `POOL` not yet set anyway.
fn requested_width() -> usize {
    if let Ok(v) = std::env::var("DEVKIT_THREADS")
        && let Ok(n) = v.parse::<NonZeroUsize>()
    {
        return n.get();
    }
    CONFIGURED.get().map_or(DEFAULT_THREADS, |n| n.get())
}

/// The shared pool, or `None` when it could not be built. A build failure is
/// not fatal, but it is a degraded mode rather than a clean fallback:
/// [`install`] runs the closure on the calling thread and
/// [`jwalk_parallelism`] goes `Serial`, so a jwalk walk stays bounded, while a
/// rayon parallel iterator inside an `install` closure reaches rayon's global
/// pool at one thread per core. Building fails only where the process cannot
/// spawn threads at all, which is also where that width hurts most; there is
/// no narrower pool to fall back to at that point.
fn pool() -> Option<&'static Arc<rayon::ThreadPool>> {
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(requested_width())
            .thread_name(|i| format!("devkit-{i}"))
            .build()
            .ok()
            .map(Arc::new)
    })
    .as_ref()
}

/// Whether the calling thread is a rayon worker. Work dispatched from one must
/// stay on it: a bounded pool re-entered from its own worker can leave a nested
/// walk waiting for a thread that never frees.
fn inside_a_pool() -> bool {
    rayon::current_thread_index().is_some()
}

/// Run `f` on the shared pool, or on the calling thread when already inside a
/// pool or when the pool could not be built.
pub fn install<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    match pool() {
        Some(p) if !inside_a_pool() => p.install(f),
        _ => f(),
    }
}

/// jwalk's parallelism setting for the shared pool. `Serial` when already
/// inside a pool, for the reason [`install`] describes.
pub fn jwalk_parallelism() -> jwalk::Parallelism {
    match pool() {
        Some(p) if !inside_a_pool() => jwalk::Parallelism::RayonExistingPool {
            pool: Arc::clone(p),
            busy_timeout: Some(BUSY_TIMEOUT),
        },
        _ => jwalk::Parallelism::Serial,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pool exists and runs work. `width` is not asserted against a
    /// constant: `configure` and `DEVKIT_THREADS` are process-global and the
    /// test binary runs tests concurrently, so any test pinning a specific
    /// width would race the others.
    #[test]
    fn install_runs_the_closure_and_returns_its_value() {
        assert_eq!(install(|| 2 + 2), 4);
    }

    #[test]
    fn width_is_at_least_one() {
        assert!(width() >= 1);
    }

    /// The pool is sized once and cannot be resized, so a width recorded after
    /// it is running would be a number no thread of it has. The assertion is
    /// against the width observed just before the late call, not a constant,
    /// so it does not race whatever else configured the pool first.
    #[test]
    fn a_late_configure_cannot_change_the_reported_width() {
        install(|| ());
        let running = width();
        configure(NonZeroUsize::new(running + 7));
        assert_eq!(width(), running);
    }

    /// The guard that keeps a nested walk from waiting on threads its own
    /// caller is holding. Without it this test deadlocks rather than fails.
    #[test]
    fn install_nested_inside_the_pool_runs_on_the_calling_thread() {
        let inner = install(|| install(|| "ran"));
        assert_eq!(inner, "ran");
    }

    /// jwalk must never reach for rayon's global pool, and must go serial when
    /// it would otherwise be nested.
    #[test]
    fn jwalk_parallelism_is_serial_when_already_inside_the_pool() {
        let nested = install(|| matches!(jwalk_parallelism(), jwalk::Parallelism::Serial));
        assert!(nested);
    }

    #[test]
    fn jwalk_parallelism_uses_the_shared_pool_from_outside_it() {
        assert!(matches!(
            jwalk_parallelism(),
            jwalk::Parallelism::RayonExistingPool { .. }
        ));
    }
}
