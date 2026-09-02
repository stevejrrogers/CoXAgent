//! Lesson efficacy ledger (CXA-F306): the per-lesson record that turns the
//! write-only lesson stores into a measurable loop.
//!
//! Until this module, lessons were bare strings: recorded, deduped, capped,
//! fed into prompts — and nothing ever measured whether a lesson actually
//! prevented its failure class from recurring. The deploy-secret saga
//! (CXA-B027 → B028 → B030 → B031 → B036 → B037) is one failure class
//! recurring across five-plus tickets, each closed with a lesson, with no
//! signal that the lessons kept failing.
//!
//! Owned here: [`LessonRecord`] (recorded-at, re-recordings, recurrences
//! anchored to their incident, escalation), [`DismissedMatch`] (a reviewer's
//! persisted "this incident is NOT that lesson" verdict), and the mutation
//! methods on [`ProjectState`]. Pure state mechanics — the matching that
//! FEEDS it lives in `crate::lesson_efficacy`, the forcing sweep in
//! `use_cases::cycle::lesson_sweep`.

use serde::{Deserialize, Serialize};

use super::now_rfc3339;
use super::ProjectState;

/// The efficacy ledger stays prompt-adjacent sized: 24 records (vs the
/// 12-entry prompt list) so a lesson evicted from the prompts can still
/// carry its recurrence history when it is re-learned.
pub const MAX_LESSON_RECORDS: usize = 24;

/// Bounded dismissal log — a reviewer verdict per suggested match, kept so
/// the same incident can never re-increment a lesson it was dismissed for.
pub const MAX_DISMISSED_MATCHES: usize = 100;

/// One recurrence of a lesson's failure class, anchored to the incident that
/// evidenced it: `incident_at` is the once-per-incident dedupe identity AND
/// the link the Hub lessons page renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LessonRecurrence {
    /// RFC3339 when this recurrence was recorded.
    pub at: String,
    /// `IncidentRecord.at` of the incident that matched this lesson.
    pub incident_at: String,
    /// What triggered the incident (`deploy failed` / `tests failed` / …).
    pub incident_reason: String,
}

/// The structural-fix escalation of a repeating lesson (CXA-F306): the
/// prevention ticket filed for it and how far up the chore→bug ladder it
/// has climbed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LessonEscalation {
    /// Id of the filed prevention ticket.
    pub ticket: String,
    /// RFC3339 when the escalation was filed.
    pub at: String,
    /// `"chore"` on the first rung, `"bug"` once a closed chore failed to
    /// stop the recurrence.
    pub stage: String,
}

/// One lesson's efficacy record: when it was learned, how often it had to be
/// re-learned, which incidents matched it since, and whether structural work
/// was escalated for it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LessonRecord {
    /// The lesson text, exactly as recorded (the dedupe key).
    pub text: String,
    /// RFC3339 when the lesson was first recorded.
    pub at: String,
    /// The project cycle the lesson was recorded in.
    pub cycle: u64,
    /// How many times the SAME lesson was re-recorded (a re-learned lesson is
    /// itself an efficacy failure — the team had to learn it twice).
    pub re_recordings: u64,
    /// Incidents that matched this lesson after it was recorded, oldest
    /// first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recurrences: Vec<LessonRecurrence>,
    /// The structural-fix escalation, once filed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub escalated: Option<LessonEscalation>,
}

impl LessonRecord {
    /// A lesson with 2+ recurrences — or one that had to be re-learned 2+
    /// times — is repeating and must be forced into structural work.
    #[must_use]
    pub fn is_repeating(&self) -> bool {
        self.recurrences.len() >= 2 || self.re_recordings >= 2
    }
}

/// A reviewer's persisted dismissal of one suggested incident→lesson match
/// (CXA-F306 AC3): the incident stays unlinked, the lesson's recurrence
/// count is retracted for it, and the pair can never increment again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DismissedMatch {
    /// The lesson text the match suggested.
    pub lesson: String,
    /// `IncidentRecord.at` of the dismissed incident.
    pub incident_at: String,
    /// What triggered the dismissed incident.
    pub incident_reason: String,
    /// RFC3339 when the reviewer dismissed it.
    pub dismissed_at: String,
    /// Who dismissed it (authenticated principal or role label).
    pub dismissed_by: String,
}

impl ProjectState {
    /// Record a retro lesson (deduped, newest last, capped at 12) — the
    /// pre-F306 entry point, now delegating to [`ProjectState::record_lesson`]
    /// so every lesson also lands in the efficacy ledger.
    pub fn add_lesson(&mut self, lesson: &str) {
        self.record_lesson(lesson);
    }

    /// Record a retro/PR/post-mortem lesson (deduped, newest last, capped at
    /// 12 — the prompt-facing list is unchanged). A duplicate of an
    /// already-recorded lesson is an efficacy signal, not a no-op: it bumps
    /// `re_recordings` on the [`LessonRecord`] ledger instead of being
    /// silently dropped (CXA-F306). Returns whether the text was NEW.
    pub fn record_lesson(&mut self, lesson: &str) -> bool {
        let lesson = lesson.trim();
        if lesson.is_empty() {
            return false;
        }
        let mut is_new = false;
        if let Some(record) = self.lesson_records.iter_mut().find(|r| r.text == lesson) {
            record.re_recordings = record.re_recordings.saturating_add(1);
        } else {
            self.lesson_records.push(LessonRecord {
                text: lesson.to_owned(),
                at: now_rfc3339(),
                cycle: self.cycle,
                re_recordings: 0,
                recurrences: Vec::new(),
                escalated: None,
            });
            let overflow = self.lesson_records.len().saturating_sub(MAX_LESSON_RECORDS);
            if overflow > 0 {
                self.lesson_records.drain(0..overflow);
            }
            is_new = true;
        }
        // The prompt-facing list keeps its own 12-entry dedupe+cap. A lesson
        // evicted from it that gets re-recorded (or re-learned) climbs back
        // in — the team clearly still needs it in every brief.
        if !self.lessons.iter().any(|l| l == lesson) {
            self.lessons.push(lesson.to_owned());
            let overflow = self.lessons.len().saturating_sub(12);
            if overflow > 0 {
                self.lessons.drain(0..overflow);
            }
        }
        is_new
    }

    /// Record one recurrence of `lesson`'s failure class, anchored to the
    /// incident that evidenced it. Increments EXACTLY ONCE per incident
    /// (dedupe on `incident_at`), never for a match the reviewer dismissed,
    /// and carries `prior` history along when the lesson is being
    /// re-surfaced from eviction (AC4) so nothing is silently lost.
    /// Returns whether a recurrence was recorded.
    pub fn record_recurrence(
        &mut self,
        lesson: &str,
        incident_at: &str,
        incident_reason: &str,
        prior: &[LessonRecurrence],
    ) -> bool {
        if lesson.trim().is_empty()
            || incident_at.is_empty()
            || self.match_is_dismissed(lesson, incident_at)
        {
            return false;
        }
        if self
            .lesson_records
            .iter_mut()
            .find(|r| r.text == lesson)
            .is_none()
        {
            // A recurrence for a lesson we hold no record of (hub-origin,
            // or re-surfaced from eviction): create the record so the
            // history has a home instead of starting from zero.
            self.lesson_records.push(LessonRecord {
                text: lesson.to_owned(),
                at: now_rfc3339(),
                cycle: self.cycle,
                re_recordings: 0,
                recurrences: Vec::new(),
                escalated: None,
            });
            let overflow = self.lesson_records.len().saturating_sub(MAX_LESSON_RECORDS);
            if overflow > 0 {
                self.lesson_records.drain(0..overflow);
            }
        }
        let Some(record) = self.lesson_records.iter_mut().find(|r| r.text == lesson) else {
            // Unreachable: the record for this text was just ensured present.
            return false;
        };
        if record
            .recurrences
            .iter()
            .chain(prior.iter())
            .any(|e| e.incident_at == incident_at)
        {
            return false;
        }
        for event in prior {
            if !record
                .recurrences
                .iter()
                .any(|e| e.incident_at == event.incident_at)
            {
                record.recurrences.push(event.clone());
            }
        }
        record.recurrences.push(LessonRecurrence {
            at: now_rfc3339(),
            incident_at: incident_at.to_owned(),
            incident_reason: incident_reason.to_owned(),
        });
        true
    }

    /// Whether the reviewer already dismissed the suggested match between
    /// `lesson` and the incident stamped `incident_at` (AC3: a dismissed
    /// match never increments again).
    #[must_use]
    pub fn match_is_dismissed(&self, lesson: &str, incident_at: &str) -> bool {
        self.dismissed_matches
            .iter()
            .any(|d| d.lesson == lesson && d.incident_at == incident_at)
    }

    /// Persist the reviewer's dismissal of one suggested incident→lesson
    /// match: retract any recurrence already recorded for that pair and keep
    /// the verdict so the same match can never increment again (AC3).
    /// Returns whether a recurrence was retracted.
    pub fn dismiss_match(
        &mut self,
        lesson: &str,
        incident_at: &str,
        incident_reason: &str,
        dismissed_by: &str,
    ) -> bool {
        if lesson.trim().is_empty() || incident_at.is_empty() {
            return false;
        }
        if !self.match_is_dismissed(lesson, incident_at) {
            self.dismissed_matches.push(DismissedMatch {
                lesson: lesson.to_owned(),
                incident_at: incident_at.to_owned(),
                incident_reason: incident_reason.to_owned(),
                dismissed_at: now_rfc3339(),
                dismissed_by: dismissed_by.to_owned(),
            });
            let overflow = self
                .dismissed_matches
                .len()
                .saturating_sub(MAX_DISMISSED_MATCHES);
            if overflow > 0 {
                self.dismissed_matches.drain(0..overflow);
            }
        }
        let before = self
            .lesson_records
            .iter()
            .map(|r| r.recurrences.len())
            .sum::<usize>();
        if let Some(record) = self.lesson_records.iter_mut().find(|r| r.text == lesson) {
            record.recurrences.retain(|e| e.incident_at != incident_at);
        }
        let after = self
            .lesson_records
            .iter()
            .map(|r| r.recurrences.len())
            .sum::<usize>();
        before != after
    }

    /// Mark a repeating lesson structurally escalated: the prevention ticket
    /// filed for it and the ladder stage reached. Idempotent — an existing
    /// escalation at the same or higher rung is left alone. Returns whether
    /// the escalation was written.
    pub fn mark_escalated(&mut self, lesson: &str, ticket: &str, stage: &str) -> bool {
        let Some(record) = self.lesson_records.iter_mut().find(|r| r.text == lesson) else {
            return false;
        };
        if record
            .escalated
            .as_ref()
            .is_some_and(|e| e.stage == "bug" || e.ticket == ticket)
        {
            return false;
        }
        record.escalated = Some(LessonEscalation {
            ticket: ticket.to_owned(),
            at: now_rfc3339(),
            stage: stage.to_owned(),
        });
        true
    }
}

#[cfg(test)]
mod lessons_tests {
    use super::*;

    fn record(text: &str) -> LessonRecord {
        LessonRecord {
            text: text.to_owned(),
            at: "2026-09-01T10:00:00Z".to_owned(),
            cycle: 1,
            re_recordings: 0,
            recurrences: Vec::new(),
            escalated: None,
        }
    }

    #[test]
    fn a_duplicate_add_bumps_re_recordings_instead_of_dropping() {
        let mut s = ProjectState::default();
        assert!(s.record_lesson("pin the base image version"));
        assert!(!s.record_lesson("pin the base image version"));
        assert_eq!(s.lessons.len(), 1, "the prompt list stays deduped");
        assert_eq!(s.lesson_records.len(), 1);
        assert_eq!(
            s.lesson_records[0].re_recordings, 1,
            "a re-learned lesson is an efficacy signal, not a dropped write"
        );
    }

    #[test]
    fn the_record_cap_evicts_the_oldest_at_24() {
        let mut s = ProjectState::default();
        for i in 0..MAX_LESSON_RECORDS + 3 {
            s.record_lesson(&format!("distinct lesson {i}: pin the dependency"));
        }
        assert_eq!(s.lesson_records.len(), MAX_LESSON_RECORDS);
        assert!(
            !s.lesson_records
                .iter()
                .any(|r| r.text == "distinct lesson 0: pin the dependency"),
            "the oldest record is evicted first"
        );
        assert_eq!(s.lessons.len(), 12, "the prompt list keeps its own cap");
    }

    #[test]
    fn recurrence_increments_once_per_incident_and_never_after_dismissal() {
        let mut s = ProjectState::default();
        s.record_lesson("pin the base image version");
        let incident = "2026-09-02T10:00:00Z";
        assert!(s.record_recurrence("pin the base image version", incident, "deploy failed", &[]));
        assert!(
            !s.record_recurrence("pin the base image version", incident, "deploy failed", &[]),
            "the same incident must never double-count"
        );
        assert_eq!(s.lesson_records[0].recurrences.len(), 1);
        assert_eq!(s.lesson_records[0].recurrences[0].incident_at, incident);

        assert!(s.dismiss_match(
            "pin the base image version",
            incident,
            "deploy failed",
            "op"
        ));
        assert_eq!(
            s.lesson_records[0].recurrences.len(),
            0,
            "dismissal retracts the recurrence it dismissed"
        );
        assert!(
            !s.record_recurrence("pin the base image version", incident, "deploy failed", &[]),
            "a dismissed match never increments again"
        );
        assert!(s.match_is_dismissed("pin the base image version", incident));
    }

    #[test]
    fn a_recurrence_for_an_unknown_lesson_creates_its_record_with_prior_history() {
        let mut s = ProjectState::default();
        let prior = vec![super::LessonRecurrence {
            at: "2026-08-01T10:00:00Z".to_owned(),
            incident_at: "2026-08-01T09:00:00Z".to_owned(),
            incident_reason: "tests failed".to_owned(),
        }];
        assert!(s.record_recurrence(
            "re-surfaced from eviction",
            "2026-09-02T10:00:00Z",
            "deploy failed",
            &prior
        ));
        assert_eq!(s.lesson_records.len(), 1);
        assert_eq!(
            s.lesson_records[0].recurrences.len(),
            2,
            "prior history carried"
        );
    }

    #[test]
    fn escalation_is_idempotent_and_climbs_the_ladder() {
        let mut s = ProjectState::default();
        s.record_lesson("pin the base image version");
        assert!(s.mark_escalated("pin the base image version", "CXC-C001", "chore"));
        assert!(
            !s.mark_escalated("pin the base image version", "CXC-C001", "chore"),
            "re-marking the same ticket is a no-op"
        );
        assert!(s.mark_escalated("pin the base image version", "CXC-B002", "bug"));
        assert_eq!(
            s.lesson_records[0]
                .escalated
                .as_ref()
                .map(|e| e.stage.as_str()),
            Some("bug")
        );
        assert!(!s.mark_escalated("unknown lesson", "CXC-C003", "chore"));
    }

    #[test]
    fn a_state_written_before_this_field_existed_still_loads() {
        // The additive-field convention: a snapshot from before CXA-F306 has
        // no lesson_records / dismissed_matches / sweep day and must load
        // unchanged. Built from a REAL state with the new keys stripped —
        // exactly the shape a pre-F306 writer left on disk.
        let mut current = ProjectState::default();
        current.record_lesson("pin the base image version");
        let mut doc = serde_json::to_value(&current).expect("state serializes");
        let obj = doc.as_object_mut().expect("state is an object");
        assert!(
            obj.remove("lesson_records").is_some(),
            "lesson_records is part of the state today"
        );
        assert!(
            obj.remove("lesson_sweep_day").is_some(),
            "lesson_sweep_day is part of the state today"
        );
        obj.remove("dismissed_matches"); // skip-serialized when empty
        let back: ProjectState = serde_json::from_value(doc).expect("the legacy snapshot loads");
        assert_eq!(
            back.lessons,
            vec!["pin the base image version".to_owned()],
            "the prompt-facing lessons survive"
        );
        assert!(back.lesson_records.is_empty());
        assert!(back.dismissed_matches.is_empty());

        // And the ledger itself round-trips serde-stable.
        let record = LessonRecord {
            text: "x".to_owned(),
            at: "2026-09-01T10:00:00Z".to_owned(),
            cycle: 1,
            re_recordings: 2,
            recurrences: vec![LessonRecurrence {
                at: "2026-09-02T10:00:00Z".to_owned(),
                incident_at: "2026-09-02T09:00:00Z".to_owned(),
                incident_reason: "deploy failed".to_owned(),
            }],
            escalated: Some(LessonEscalation {
                ticket: "CXC-C001".to_owned(),
                at: "2026-09-02T11:00:00Z".to_owned(),
                stage: "chore".to_owned(),
            }),
        };
        let doc = serde_json::to_string(&record).expect("record serializes");
        let back: LessonRecord = serde_json::from_str(&doc).expect("record deserializes");
        assert_eq!(back, record);
    }

    #[test]
    fn repeating_requires_two_or_more_signals() {
        let mut one = record("a");
        one.recurrences.push(super::LessonRecurrence {
            at: "2026-09-01T10:00:00Z".to_owned(),
            incident_at: "2026-09-01T09:00:00Z".to_owned(),
            incident_reason: "deploy failed".to_owned(),
        });
        assert!(
            !one.is_repeating(),
            "one recurrence is watched, not repeating"
        );
        let mut two = one.clone();
        two.recurrences.push(super::LessonRecurrence {
            at: "2026-09-02T10:00:00Z".to_owned(),
            incident_at: "2026-09-02T09:00:00Z".to_owned(),
            incident_reason: "deploy failed".to_owned(),
        });
        assert!(two.is_repeating());
        let mut relearned = record("b");
        relearned.re_recordings = 2;
        assert!(
            relearned.is_repeating(),
            "re-learning twice is the same failure"
        );
    }
}
