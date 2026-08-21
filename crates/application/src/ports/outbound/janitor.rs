//! `ProcessJanitorPort` — kill orphaned test-driver processes leaked by DEV
//! agents. Pure OS surgery (pgrep/kill), so it lives behind a port: the cycle
//! decides WHEN to sweep, the adapter knows HOW.

use std::path::Path;

pub trait ProcessJanitorPort: Send + Sync {
    /// Kill long-running test drivers whose command line references
    /// `work_dir`. Scoped and age-gated by the adapter; best-effort.
    fn kill_orphaned_drivers(&self, work_dir: &Path);

    /// Remove an idle runner's regenerable build cache (`work_dir/target`).
    /// The caller decides WHEN a runner is safe to evict its cache (end of an
    /// idle cycle); the adapter does the removal, scoped to exactly
    /// `work_dir/target` and best-effort. Never fatal on failure.
    fn purge_target_cache(&self, work_dir: &Path);
}
