//! Live-log files: where a running agent streams its work log so the dashboard's
//! `agent-log` endpoint can tail it in real time. Shared by every streaming
//! engine (opencode, copilot, …) so they all land in the same
//! `<workspace>/logs/live/<role>__<label>__<operator>.log` layout the reader
//! expects — an engine that buffers instead of streaming here shows a blank live
//! view even while it is plainly working.

use std::path::{Path, PathBuf};

/// The live-log file for a run: `<workspace>/logs/live/<role>.log`, derived from
/// the codebase work-dir (`<workspace>/codebase`). A per-run
/// [`AgentRequest::label`](crate::AgentRequest) lands between role and operator
/// so runs are chaseable per ticket: `<role>__<label>__<operator>.log`.
pub(crate) fn live_path(work_dir: &Path, role: &str, label: Option<&str>) -> Option<PathBuf> {
    let dir = work_dir.parent()?.join("logs").join("live");
    std::fs::create_dir_all(&dir).ok()?;
    let label_part = label
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map_or_else(String::new, |l| format!("__{l}"));
    let suffix = std::env::var("COXAGENT_OPERATOR")
        .ok()
        .map(|o| {
            o.chars()
                .filter(char::is_ascii_alphanumeric)
                .collect::<String>()
        })
        .filter(|s| !s.is_empty())
        .map_or_else(String::new, |s| format!("__{s}"));
    Some(dir.join(format!("{role}{label_part}{suffix}.log")))
}

/// Append one work-log line to a live file, best-effort (a live log is a nicety,
/// never a reason to fail a run).
pub(crate) fn append_live(path: &Path, line: &str) {
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{}", line.trim_end());
    }
}
