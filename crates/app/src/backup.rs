//! `coxagent backup` / `coxagent restore` — the hub's own state, archived and
//! restorable in one command (CXA-F262).
//!
//! This is the composition root of the backup feature: it wires the real
//! adapters (`FsWorkspaceFiles`, `JsonHubArchive`), resolves the machine's
//! paths, prints the summary, and owns the scheduled-backup setting. The
//! WHAT-capture and WHETHER-safe-to-restore decisions live in the
//! application-layer use cases; this file only aims them at the hub dir.
//!
//! # What one archive covers
//!
//! Every file-backed store under the hub dir (the registry's parent): the
//! hub registry itself (`registry.json`), the hub-wide wiring
//! (`coordination.json`, `auth.json`, `system_chat.json`, `workspace.json`),
//! each registered project's `coxagent.json` and the whole `state/` tree —
//! whose `state.json` carries the tickets, evidence records and wiki pages —
//! and the local evidence blob bytes under `blobs/`. Restoring into an empty
//! dir and booting the hub shows the same projects and tickets.
//!
//! # Secret policy (stated in the backup output, never left to guess)
//!
//! Session tokens are EXCLUDED — `sessions.json` and the operator token are
//! live bearer material and are never archived; `auth.json` ships only its
//! salted credential hashes; `coordination.json` ships verbatim (prefer
//! `${VAR}` placeholders over inline DSN credentials). Deploy secrets are
//! EXCLUDED unless the operator passes `--include-secrets`; when included
//! they restore to the CURRENT machine's `deploy_secrets_root()` (the
//! CXA-B032 compose-name keying keeps them valid there), never to the source
//! machine's path.
//!
//! # Consistency while the hub serves
//!
//! The store publishes `state.json` by temp-file + rename (atomic), and every
//! JSON capture is parse-validated with one re-read on failure — a torn read
//! either heals on the second read or fails the backup loudly; a torn file
//! never enters an archive. The archive itself is published by atomic rename
//! (see `JsonHubArchive`), so a crash mid-write can never leave a partial
//! archive as the newest restorable one.
//!
//! # Scheduled backups
//!
//! `coordination.json` may carry a `backup` setting — `{"backup":
//! {"interval_secs": 86400, "retention": 7}}` — read each tick while the hub
//! runs: every interval the workspace is archived and the archive dir pruned
//! to the newest `retention` archives. Default when absent: every 24h, keep
//! 7. `interval_secs: 0` disables the schedule. Scheduled runs use the exact
//! code path of `coxagent backup`, so a live-serving capture is exercised
//! continuously. Externally-backed stores (Postgres/Redis/Mongo/S3 via DSN
//! env) are never in the archive — the summary names each configured backend
//! the operator must back up with its own tooling.

use std::fmt::Write as _;

use coxagent_application::use_cases::{
    path_stamp, BackupRequest, BackupWorkspaceUseCase, RestoreRequest, RestoreWorkspaceUseCase,
};
use coxagent_infrastructure::{deploy::deploy_secrets_root, FsWorkspaceFiles, JsonHubArchive};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// How often the scheduled loop re-reads the setting and checks whether a
/// backup is due — frequent enough to track settings edits, cheap enough to
/// ignore.
const SCHEDULE_TICK: Duration = Duration::from_secs(60);

/// Backup defaults: daily, keep a week of archives.
const DEFAULT_INTERVAL_SECS: u64 = 86_400;
const DEFAULT_RETENTION: usize = 7;

/// `coxagent backup` — archive the hub's own state and print the summary.
///
/// # Errors
/// Propagates the use case's failures (missing/corrupt registry, torn JSON,
/// unwritable archive).
pub(crate) async fn run_backup(
    hub_dir: &Path,
    out: Option<PathBuf>,
    include_secrets: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let out = out.unwrap_or_else(|| default_archive_path(hub_dir));
    let secrets_root = include_secrets.then(deploy_secrets_root);
    let uc = BackupWorkspaceUseCase::new(Arc::new(FsWorkspaceFiles::new()), JsonHubArchive::new());
    let outcome = uc
        .execute(&BackupRequest {
            hub_dir: hub_dir.to_path_buf(),
            out: out.clone(),
            secrets_root,
        })
        .await?;

    let mut summary = format!(
        "backed up {} file(s) ({} bytes) -> {}\nsha256: {}\n{}\n",
        outcome.manifest.file_count,
        outcome.manifest.total_bytes,
        outcome.archive_path.display(),
        outcome.manifest.sha256,
        outcome.secret_statement,
    );
    for w in &outcome.warnings {
        let _ = writeln!(summary, "warning: {w}");
    }
    // Retention applies to manual backups too, whenever a schedule is
    // configured: the archive dir never grows past the newest-N window.
    if let Some(setting) = scheduled_backup_setting(hub_dir) {
        let pruned = prune_archives(&backups_dir(hub_dir), setting.retention);
        if pruned > 0 {
            let _ = writeln!(
                summary,
                "pruned {pruned} old archive(s) (retention: newest {})",
                setting.retention
            );
        }
    }
    for w in external_backend_warnings() {
        let _ = writeln!(summary, "not captured (externally backed): {w}");
    }
    Ok(summary)
}

/// `coxagent restore` — rebuild a hub workspace from an archive.
///
/// # Errors
/// Propagates the use case's refusals: a non-empty target without `--force`,
/// a newer archive schema, a tampered archive, unsafe paths, or a hub that
/// still holds a project state lock.
pub(crate) async fn run_restore(
    archive: &Path,
    hub_dir: &Path,
    force: bool,
    dry_run: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let uc = RestoreWorkspaceUseCase::new(Arc::new(FsWorkspaceFiles::new()), JsonHubArchive::new());
    let outcome = uc
        .execute(&RestoreRequest {
            hub_dir: hub_dir.to_path_buf(),
            archive_path: archive.to_path_buf(),
            secrets_root: deploy_secrets_root(),
            force,
            dry_run,
        })
        .await?;

    let mut report = outcome.summary.clone();
    report.push('\n');
    for f in outcome
        .restored
        .iter()
        .chain(outcome.secrets_restored.iter())
    {
        let _ = writeln!(report, "  wrote {f}");
    }
    for h in &outcome.hints {
        let _ = writeln!(report, "hint: {h}");
    }
    Ok(report)
}

/// Arm the scheduled workspace backup for a running hub. Runs for as long as
/// the hub does; every tick re-reads the setting so an operator can tune the
/// interval or retention without a restart.
pub(crate) fn spawn_scheduled(hub_dir: PathBuf) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(SCHEDULE_TICK).await;
            let Some(setting) = scheduled_backup_setting(&hub_dir) else {
                continue; // the schedule is disabled
            };
            if !backup_due(&hub_dir, setting.interval).await {
                continue;
            }
            match run_backup(&hub_dir, None, false).await {
                Ok(summary) => {
                    tracing::info!(
                        "scheduled workspace backup complete: {}",
                        summary.lines().next().unwrap_or_default()
                    );
                }
                Err(e) => {
                    // A failed tick must be loud but never fatal to the hub;
                    // the next tick retries.
                    tracing::warn!("scheduled workspace backup failed: {e}");
                }
            }
        }
    });
}

/// The scheduled-backup setting: interval + retention count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScheduledBackupSetting {
    /// How often a backup runs.
    pub interval: Duration,
    /// How many archives to keep — the newest N; older ones are pruned.
    pub retention: usize,
}

/// Read the setting from `<hub_dir>/coordination.json`'s optional `backup`
/// object (`{"backup": {"interval_secs": 86400, "retention": 7}}`); fall back
/// to the defaults when absent or malformed. `interval_secs: 0` disables the
/// schedule (`None`), and retention below one clamps to one — keeping none
/// would make every backup pointless.
#[must_use]
pub(crate) fn scheduled_backup_setting(hub_dir: &Path) -> Option<ScheduledBackupSetting> {
    let mut setting = ScheduledBackupSetting {
        interval: Duration::from_secs(DEFAULT_INTERVAL_SECS),
        retention: DEFAULT_RETENTION,
    };
    let Ok(text) = std::fs::read_to_string(hub_dir.join("coordination.json")) else {
        return Some(setting);
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Some(setting);
    };
    // No `backup` section is the NORMAL case (a coordination.json carrying
    // only DSNs) — that must keep the defaults, not disable the schedule.
    let Some(backup) = v.get("backup") else {
        return Some(setting);
    };
    if backup
        .get("interval_secs")
        .and_then(serde_json::Value::as_u64)
        == Some(0)
    {
        return None;
    }
    if let Some(secs) = backup
        .get("interval_secs")
        .and_then(serde_json::Value::as_u64)
    {
        setting.interval = Duration::from_secs(secs.max(SCHEDULE_TICK.as_secs()));
    }
    if let Some(retention) = backup.get("retention").and_then(serde_json::Value::as_u64) {
        setting.retention = usize::try_from(retention.max(1)).unwrap_or(usize::MAX);
    }
    Some(setting)
}

/// Is a scheduled backup due? Pure over the newest archive's age: due when
/// no archive exists yet or the newest is at least `interval` old.
async fn backup_due(hub_dir: &Path, interval: Duration) -> bool {
    let Some(path) = newest_archive(&backups_dir(hub_dir)) else {
        return true;
    };
    let Ok(meta) = tokio::fs::metadata(&path).await else {
        return true;
    };
    let Ok(modified) = meta.modified() else {
        return true;
    };
    let Ok(age) = modified.elapsed() else {
        return false; // clock skew: a "future" archive is fresh enough
    };
    age >= interval
}

/// `<hub-dir>/backups/` — the archive directory itself sits on the capture
/// exclusion list so archives never nest into subsequent backups.
#[must_use]
pub(crate) fn backups_dir(hub_dir: &Path) -> PathBuf {
    hub_dir.join("backups")
}

/// The default archive path: `<hub-dir>/backups/coxagent-backup-<UTC
/// timestamp>.hubarchive.json`.
#[must_use]
fn default_archive_path(hub_dir: &Path) -> PathBuf {
    backups_dir(hub_dir).join(format!(
        "coxagent-backup-{}.hubarchive.json",
        path_stamp(&coxagent_application::state::now_rfc3339())
    ))
}

/// The archives in `dir` sorted oldest -> newest (names embed a UTC stamp,
/// so lexicographic order is chronological). Missing dir reads as empty.
fn archives_in(dir: &Path) -> Vec<PathBuf> {
    let mut archives: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.to_string_lossy().ends_with(".hubarchive.json"))
                .collect()
        })
        .unwrap_or_default();
    archives.sort();
    archives
}

/// The newest archive in `dir`, if any.
fn newest_archive(dir: &Path) -> Option<PathBuf> {
    archives_in(dir).pop()
}

/// Keep only the newest `keep` archives in `dir`; non-archives are untouched.
/// Returns how many were removed.
pub(crate) fn prune_archives(dir: &Path, keep: usize) -> usize {
    let archives = archives_in(dir);
    let excess = archives.len().saturating_sub(keep);
    for old in &archives[..excess] {
        if std::fs::remove_file(old).is_err() {
            tracing::warn!("could not prune old archive {}", old.display());
        }
    }
    excess
}

/// Every externally-backed store this machine has configured, which the
/// file-backed archive by definition cannot capture — printed with the
/// backup summary so the operator routes each to its own backup tooling.
/// Called AFTER `load_coordination`, so coordination.json's DSNs count too.
fn external_backend_warnings() -> Vec<String> {
    let configured = |key: &str| std::env::var(key).is_ok_and(|v| !v.trim().is_empty());
    let mut out = Vec::new();
    if configured("COXAGENT_DB_DSN") {
        out.push(
            "project state in Postgres (COXAGENT_DB_DSN) — back the database up with pg_dump"
                .to_owned(),
        );
    }
    if configured("COXAGENT_AUTH_DSN") {
        out.push(
            "accounts/tokens in Postgres (COXAGENT_AUTH_DSN) — back the database up with pg_dump"
                .to_owned(),
        );
    }
    if configured("COXAGENT_REDIS_URL") {
        out.push("ephemeral leases in Redis (COXAGENT_REDIS_URL) — TTL data, safe to lose, but it is not in this archive".to_owned());
    }
    if configured("COXAGENT_MONGO_URL") {
        out.push(
            "wiki pages in MongoDB (COXAGENT_MONGO_URL) — back the database up with mongodump"
                .to_owned(),
        );
    }
    if configured("COXAGENT_REMOTE_STORE_URL") {
        out.push("project state behind a REST gateway (COXAGENT_REMOTE_STORE_URL) — back the gateway's own store up".to_owned());
    }
    if configured("COXAGENT_S3_ENDPOINT") {
        out.push("evidence blobs in S3 (COXAGENT_S3_ENDPOINT) — back the bucket up with the provider's tooling".to_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scheduled setting defaults to daily / keep-7 when coordination.json
    /// is absent, and honours both halves when present.
    #[test]
    fn the_scheduled_setting_reads_interval_and_retention_from_coordination() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = scheduled_backup_setting(dir.path()).expect("defaults");
        assert_eq!(base.interval, Duration::from_secs(DEFAULT_INTERVAL_SECS));
        assert_eq!(base.retention, DEFAULT_RETENTION);

        std::fs::write(
            dir.path().join("coordination.json"),
            br#"{"db_dsn":"postgres://x","backup":{"interval_secs":3600,"retention":3}}"#,
        )
        .expect("write");
        let tuned = scheduled_backup_setting(dir.path()).expect("tuned");
        assert_eq!(tuned.interval, Duration::from_secs(3600));
        assert_eq!(tuned.retention, 3);
    }

    /// `interval_secs: 0` disables the schedule entirely.
    #[test]
    fn a_zero_interval_disables_the_schedule() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("coordination.json"),
            br#"{"backup":{"interval_secs":0,"retention":9}}"#,
        )
        .expect("write");
        assert!(scheduled_backup_setting(dir.path()).is_none());
    }

    /// Regression: the NORMAL coordination.json (DSNs only, no `backup`
    /// section) must keep the default schedule — a missing section is not a
    /// request to disable backups.
    #[test]
    fn a_coordination_file_without_a_backup_section_keeps_the_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("coordination.json"),
            br#"{"db_dsn":"postgres://x","redis_url":"redis://y"}"#,
        )
        .expect("write");
        let setting = scheduled_backup_setting(dir.path()).expect("defaults, not disabled");
        assert_eq!(setting.interval, Duration::from_secs(DEFAULT_INTERVAL_SECS));
        assert_eq!(setting.retention, DEFAULT_RETENTION);
    }

    /// Retention keeps the newest N archives and prunes older ones, by the
    /// chronological (name-embedded stamp) order.
    #[test]
    fn retention_prunes_to_the_newest_n_archives() {
        let dir = tempfile::tempdir().expect("tempdir");
        let backups = dir.path().join("backups");
        std::fs::create_dir_all(&backups).expect("backups dir");
        for stamp in ["20260901T000000Z", "20260902T000000Z", "20260903T000000Z"] {
            std::fs::write(
                backups.join(format!("coxagent-backup-{stamp}.hubarchive.json")),
                b"archive",
            )
            .expect("write archive");
        }
        std::fs::write(backups.join("unrelated.txt"), b"not an archive").expect("write other");

        let removed = prune_archives(&backups, 2);
        assert_eq!(removed, 1, "the oldest archive goes");
        assert!(!backups
            .join("coxagent-backup-20260901T000000Z.hubarchive.json")
            .is_file());
        assert!(backups
            .join("coxagent-backup-20260902T000000Z.hubarchive.json")
            .is_file());
        assert!(backups
            .join("coxagent-backup-20260903T000000Z.hubarchive.json")
            .is_file());
        assert!(
            backups.join("unrelated.txt").is_file(),
            "non-archives are untouched"
        );
    }

    /// A due check with no archive yet is due; a fresh archive is not; an
    /// archive older than the interval is due again.
    #[tokio::test]
    async fn a_backup_is_due_when_the_newest_archive_is_older_than_the_interval() {
        let dir = tempfile::tempdir().expect("tempdir");
        let backups = backups_dir(dir.path());
        std::fs::create_dir_all(&backups).expect("backups dir");
        assert!(
            backup_due(dir.path(), Duration::from_secs(3600)).await,
            "no archive yet"
        );

        let fresh = backups.join("coxagent-backup-20260901T000000Z.hubarchive.json");
        std::fs::write(&fresh, b"archive").expect("write");
        assert!(
            !backup_due(dir.path(), Duration::from_secs(3600)).await,
            "a just-written archive is fresh"
        );
        let old = std::time::SystemTime::now() - Duration::from_secs(7200);
        let f = std::fs::File::options()
            .write(true)
            .open(&fresh)
            .expect("open");
        f.set_modified(old).expect("set mtime");
        drop(f);
        assert!(
            backup_due(dir.path(), Duration::from_secs(3600)).await,
            "a two-hour-old archive is due on a one-hour interval"
        );
    }

    /// The default archive lands in backups/ with the house naming scheme.
    #[test]
    fn the_default_archive_path_is_timestamped_under_backups() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = default_archive_path(dir.path());
        let name = path
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        assert!(path.starts_with(backups_dir(dir.path())));
        assert!(name.starts_with("coxagent-backup-"), "{name}");
        assert!(name.ends_with(".hubarchive.json"), "{name}");
    }
}
