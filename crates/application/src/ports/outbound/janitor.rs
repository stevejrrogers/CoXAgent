//! `ProcessJanitorPort` — kill orphaned test-driver processes leaked by DEV
//! agents. Pure OS surgery (pgrep/kill), so it lives behind a port: the cycle
//! decides WHEN to sweep, the adapter knows HOW.

use std::path::Path;

pub trait ProcessJanitorPort: Send + Sync {
    /// Kill long-running test drivers whose command line references
    /// `work_dir`. Scoped and age-gated by the adapter; best-effort.
    fn kill_orphaned_drivers(&self, work_dir: &Path);
}
