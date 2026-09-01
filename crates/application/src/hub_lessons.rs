//! CXA-F306 hub-lesson efficacy store: per-lesson metadata for the
//! cross-project `hub_lessons.md` bullet store.
//!
//! The md store stays exactly what it was — one `- lesson` bullet per lesson,
//! newest last, capped at 30 so the prompt block stays bounded (the cap and
//! eviction contract are pinned by the F306 gate). What it never had is
//! per-lesson metadata, so this module owns a JSON sidecar next to it:
//!
//! * `lessons` — metadata for lessons currently IN the 30-entry md store
//!   (recorded-at + recurrences anchored to incidents).
//! * `retained` — lessons EVICTED by the 30-entry cap, kept with their prior
//!   recurrence history instead of being silently lost (AC4). When a later
//!   incident matches a retained lesson, it is re-surfaced into the md store
//!   and its history moves back with it.
//!
//! All IO goes through the [`WorkspaceFilesPort`] — the same discipline as
//! the md store itself; a missing or corrupt sidecar reads as an empty store
//! (the md store is the source of truth for lesson text, this only adds the
//! efficacy history).

use serde::{Deserialize, Serialize};

use crate::ports::outbound::WorkspaceFilesPort;
use crate::state::LessonRecurrence;

/// The retained-eviction shelf stays bounded: 60 entries of history is far
/// beyond anything a hub accrues between resurfacing events, and unbounded
/// growth would defeat the cap the md store exists to enforce.
pub const MAX_RETAINED: usize = 60;

/// One recurrence of a hub lesson's failure class, anchored to the incident.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HubRecurrence {
    /// RFC3339 when this recurrence was recorded.
    pub at: String,
    /// `IncidentRecord.at` of the incident that matched — the once-per-incident
    /// dedupe identity, shared with the project-side `LessonRecurrence`.
    pub incident_at: String,
    /// What triggered the incident.
    pub incident_reason: String,
}

/// Per-lesson metadata for one hub lesson.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HubLessonMeta {
    /// The lesson text (matches its `- text` bullet in the md store).
    pub text: String,
    /// RFC3339 when the lesson was first recorded hub-wide.
    pub at: String,
    /// Incidents that matched this lesson, oldest first.
    pub recurrences: Vec<HubRecurrence>,
}

/// The sidecar document.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HubLessonStore {
    /// Lessons currently in the md store, in md order (oldest first).
    pub lessons: Vec<HubLessonMeta>,
    /// Evicted by the 30-entry cap, retained for resurfacing (AC4).
    pub retained: Vec<HubLessonMeta>,
}

/// The sidecar path for a given md store path: a sibling
/// (`hub_lessons.md.meta.json` next to `hub_lessons.md`), so it inherits the
/// md store's location wherever the caller resolved it from.
#[must_use]
pub fn meta_path_for(md: &std::path::Path) -> std::path::PathBuf {
    match md.file_name().and_then(|n| n.to_str()) {
        Some(name) => md.with_file_name(format!("{name}.meta.json")),
        None => md.to_path_buf(),
    }
}

/// Load the sidecar. Missing or unreadable reads as empty — the md store
/// stays the source of truth; this only carries the history.
pub async fn read_store(
    files: Option<&dyn WorkspaceFilesPort>,
    md_path: &std::path::Path,
) -> HubLessonStore {
    let Some(files) = files else {
        return HubLessonStore::default();
    };
    files
        .read(&meta_path_for(md_path))
        .await
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Persist the sidecar. Best-effort like every other hub-store write: a lost
/// history entry beats a crashed retro.
async fn write_store(
    files: &dyn WorkspaceFilesPort,
    md_path: &std::path::Path,
    store: &HubLessonStore,
) {
    if let Ok(text) = serde_json::to_string_pretty(store) {
        let _ = files.write(&meta_path_for(md_path), &text).await;
    }
}

/// Move lessons evicted from the md store's 30-entry cap onto the retention
/// shelf with whatever history they already carry (AC4: eviction must not
/// destroy the efficacy record).
pub async fn retain_evicted(
    files: Option<&dyn WorkspaceFilesPort>,
    md_path: &std::path::Path,
    evicted: &[String],
    at: &str,
) {
    let Some(files) = files else {
        return;
    };
    if evicted.is_empty() {
        return;
    }
    let mut store = read_store(Some(files), md_path).await;
    for text in evicted {
        let text = text.trim_start_matches("- ").trim();
        if text.is_empty() || store.retained.iter().any(|m| m.text == text) {
            continue;
        }
        let meta = match store.lessons.iter().position(|m| m.text == text) {
            Some(at) => store.lessons.remove(at),
            None => HubLessonMeta {
                text: text.to_owned(),
                at: at.to_owned(),
                recurrences: Vec::new(),
            },
        };
        store.retained.push(meta);
    }
    let overflow = store.retained.len().saturating_sub(MAX_RETAINED);
    if overflow > 0 {
        store.retained.drain(0..overflow);
    }
    write_store(files, md_path, &store).await;
}

/// Record one incident's recurrence against a hub lesson, deduped once per
/// incident. When the lesson was on the RETENTION shelf (evicted by the
/// 30-entry cap), it is re-surfaced into the md store first and its prior
/// recurrence history moves back with it (AC4).
///
/// Returns the lesson's FULL recurrence history after the update (prior +
/// new), so the caller can mirror it wherever the page reads from. Empty
/// when there was no store to write (no files port) — the caller's project
/// ledger still records its own view.
pub async fn record_recurrence(
    files: Option<&dyn WorkspaceFilesPort>,
    md_path: &std::path::Path,
    lesson: &str,
    incident_at: &str,
    incident_reason: &str,
) -> Vec<LessonRecurrence> {
    let Some(files) = files else {
        return Vec::new();
    };
    if lesson.trim().is_empty() || incident_at.is_empty() {
        return Vec::new();
    }
    let mut store = read_store(Some(files), md_path).await;
    // Find-or-create the meta across both shelves.
    let shelf = store
        .lessons
        .iter()
        .position(|m| m.text == lesson)
        .map(|at| (at, false))
        .or_else(|| {
            store
                .retained
                .iter()
                .position(|m| m.text == lesson)
                .map(|at| (at, true))
        });
    let resurfaced = matches!(shelf, Some((_, true)));
    let position = match shelf {
        Some((at, true)) => {
            // AC4 resurfacing: back into the live store, history intact.
            let mut meta = store.retained.remove(at);
            meta.recurrences.retain(|e| e.incident_at != incident_at);
            meta.recurrences.push(HubRecurrence {
                at: crate::state::now_rfc3339(),
                incident_at: incident_at.to_owned(),
                incident_reason: incident_reason.to_owned(),
            });
            store.lessons.push(meta);
            store.lessons.len() - 1
        }
        Some((at, false)) => {
            let meta = &mut store.lessons[at];
            if meta
                .recurrences
                .iter()
                .any(|e| e.incident_at == incident_at)
            {
                // Already counted for this incident — once per incident, exactly.
                return to_history(meta);
            }
            meta.recurrences.push(HubRecurrence {
                at: crate::state::now_rfc3339(),
                incident_at: incident_at.to_owned(),
                incident_reason: incident_reason.to_owned(),
            });
            at
        }
        None => {
            // A pre-F306 lesson with no sidecar yet: start its history now.
            store.lessons.push(HubLessonMeta {
                text: lesson.to_owned(),
                at: crate::state::now_rfc3339(),
                recurrences: vec![HubRecurrence {
                    at: crate::state::now_rfc3339(),
                    incident_at: incident_at.to_owned(),
                    incident_reason: incident_reason.to_owned(),
                }],
            });
            store.lessons.len() - 1
        }
    };
    write_store(files, md_path, &store).await;
    if resurfaced {
        // The md store IS the prompt feed and the wiki mirror's source:
        // re-adding the bullet puts the lesson back in front of every team.
        crate::prompts::record_hub_lesson(Some(files), lesson).await;
    }
    to_history(&store.lessons[position])
}

fn to_history(meta: &HubLessonMeta) -> Vec<LessonRecurrence> {
    meta.recurrences
        .iter()
        .map(|e| LessonRecurrence {
            at: e.at.clone(),
            incident_at: e.incident_at.clone(),
            incident_reason: e.incident_reason.clone(),
        })
        .collect()
}

/// Every lesson text the hub currently knows: live bullets' metadata plus
/// the retained shelf (AC4 — an evicted lesson must still be MATCHABLE).
#[must_use]
pub fn candidate_texts(store: &HubLessonStore) -> Vec<String> {
    let mut out: Vec<String> = store.lessons.iter().map(|m| m.text.clone()).collect();
    out.extend(store.retained.iter().map(|m| m.text.clone()));
    out
}

#[cfg(test)]
mod hub_store_tests {
    use super::*;
    use crate::ports::outbound::FileMeta;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    /// In-memory files double — the hub store is only ever touched through
    /// the port, so the store fixtures stay pure (no env, no disk).
    struct MemFiles(Mutex<HashMap<PathBuf, String>>);

    impl MemFiles {
        fn new() -> Arc<Self> {
            Arc::new(Self(Mutex::new(HashMap::new())))
        }
        #[allow(dead_code)]
        fn get(&self, path: &Path) -> String {
            self.0
                .lock()
                .expect("lock")
                .get(path)
                .cloned()
                .unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl WorkspaceFilesPort for MemFiles {
        async fn read(&self, path: &Path) -> Option<String> {
            self.0.lock().expect("lock").get(path).cloned()
        }
        async fn read_bytes(&self, path: &Path) -> Option<Vec<u8>> {
            self.0
                .lock()
                .expect("lock")
                .get(path)
                .map(String::as_bytes)
                .map(<[u8]>::to_vec)
        }
        async fn write(&self, p: &Path, c: &str) -> bool {
            self.0
                .lock()
                .expect("lock")
                .insert(p.to_path_buf(), c.to_owned());
            true
        }
        async fn write_bytes(&self, _p: &Path, _b: &[u8]) -> bool {
            false
        }
        async fn delete(&self, p: &Path) -> bool {
            self.0.lock().expect("lock").remove(p).is_some()
        }
        async fn list(&self, dir: &Path) -> Vec<FileMeta> {
            self.0
                .lock()
                .expect("lock")
                .keys()
                .filter(|p| p.parent().is_some_and(|parent| parent == dir))
                .map(|p| FileMeta {
                    path: p.clone(),
                    modified_epoch: 0,
                    size: 0,
                })
                .collect()
        }
        async fn stat(&self, path: &Path) -> Option<FileMeta> {
            self.0
                .lock()
                .expect("lock")
                .contains_key(path)
                .then(|| FileMeta {
                    path: path.to_path_buf(),
                    modified_epoch: 0,
                    size: 0,
                })
        }
        async fn list_recursive(&self, dir: &Path) -> Vec<PathBuf> {
            self.0
                .lock()
                .expect("lock")
                .keys()
                .filter(|p| p.starts_with(dir))
                .cloned()
                .collect()
        }
        async fn list_dirs(&self, _dir: &Path) -> Vec<PathBuf> {
            Vec::new()
        }
    }

    /// Any path works: the store functions take the md path explicitly, so
    /// the fixtures never touch the COXAGENT_HUB_LESSONS_PATH env (which
    /// other tests mutate concurrently).
    const MD: &str = "/mem-tests/hub_lessons.md";

    #[tokio::test]
    async fn a_missing_sidecar_reads_as_an_empty_store() {
        let fs = MemFiles::new();
        let store = read_store(Some(fs.as_ref()), Path::new(MD)).await;
        assert_eq!(store, HubLessonStore::default());
        assert!(candidate_texts(&store).is_empty());
    }

    #[tokio::test]
    async fn recurrences_dedupe_once_per_incident() {
        let fs = MemFiles::new();
        let lesson = "pin the base image version";
        assert!(!record_recurrence(
            Some(fs.as_ref()),
            Path::new(MD),
            lesson,
            "2026-09-02T10:00:00Z",
            "deploy failed"
        )
        .await
        .is_empty());
        let again = record_recurrence(
            Some(fs.as_ref()),
            Path::new(MD),
            lesson,
            "2026-09-02T10:00:00Z",
            "deploy failed",
        )
        .await;
        assert_eq!(again.len(), 1, "the same incident never double-counts");
        let third = record_recurrence(
            Some(fs.as_ref()),
            Path::new(MD),
            lesson,
            "2026-09-03T10:00:00Z",
            "deploy failed",
        )
        .await;
        assert_eq!(third.len(), 2, "a different incident counts");
    }

    #[tokio::test]
    async fn an_evicted_lesson_is_retained_and_resurfaces_with_its_prior_history() {
        let fs = MemFiles::new();
        let lesson = "pin the base image version";
        record_recurrence(
            Some(fs.as_ref()),
            Path::new(MD),
            lesson,
            "2026-09-01T10:00:00Z",
            "deploy failed",
        )
        .await;
        // The 30-entry cap evicts it from the md store; the write path calls
        // retain_evicted — simulate the eviction exactly as prompts.rs does.
        retain_evicted(
            Some(fs.as_ref()),
            Path::new(MD),
            &[lesson.to_owned()],
            "2026-09-01T11:00:00Z",
        )
        .await;
        let store = read_store(Some(fs.as_ref()), Path::new(MD)).await;
        assert!(store.lessons.is_empty());
        assert_eq!(store.retained.len(), 1, "eviction retains the lesson");
        assert_eq!(store.retained[0].recurrences.len(), 1, "with its history");

        // A later incident matches the evicted lesson: it resurfaces with the
        // prior recurrence history instead of starting from zero (AC4).
        let history = record_recurrence(
            Some(fs.as_ref()),
            Path::new(MD),
            lesson,
            "2026-09-02T10:00:00Z",
            "deploy failed",
        )
        .await;
        assert_eq!(
            history.len(),
            2,
            "prior history carried alongside the new hit"
        );
        assert!(
            history
                .iter()
                .any(|e| e.incident_at == "2026-09-01T10:00:00Z"),
            "the pre-eviction incident is in the resurfaced history"
        );
        let store = read_store(Some(fs.as_ref()), Path::new(MD)).await;
        assert!(store.retained.is_empty(), "the lesson left the shelf");
        assert_eq!(store.lessons.len(), 1);
    }

    #[tokio::test]
    async fn retention_stays_bounded() {
        let fs = MemFiles::new();
        let evicted: Vec<String> = (0..MAX_RETAINED + 10)
            .map(|i| format!("lesson {i}"))
            .collect();
        retain_evicted(
            Some(fs.as_ref()),
            Path::new(MD),
            &evicted,
            "2026-09-01T10:00:00Z",
        )
        .await;
        let store = read_store(Some(fs.as_ref()), Path::new(MD)).await;
        assert_eq!(store.retained.len(), MAX_RETAINED);
    }

    #[tokio::test]
    async fn no_files_port_is_a_no_op() {
        assert!(record_recurrence(
            None,
            Path::new(MD),
            "lesson",
            "2026-09-02T10:00:00Z",
            "deploy failed"
        )
        .await
        .is_empty());
        let store = read_store(None, Path::new(MD)).await;
        assert_eq!(store, HubLessonStore::default());
    }
}
