//! CXA-F306 lesson efficacy: the pure half of the loop.
//!
//! Matching reuses the codebase's own similarity primitive — `parsing::jaccard`
//! over `parsing::title_tokens`, whose documented near-duplicate threshold in
//! this codebase is 0.6 (`parsing::duplicates_existing`). A post-mortem
//! summary that repeats an existing lesson's words above that threshold IS
//! that lesson's failure class recurring; anything below it creates no match
//! (AC5). Zero IO: the sweep and the post-mortem path feed it candidates they
//! gathered behind ports, and the read model is a pure function of state.

use serde::Serialize;

use crate::parsing::{jaccard, title_tokens};
use crate::state::{LessonRecurrence, ProjectState};

/// The house near-duplicate threshold, restated here on purpose: the matcher
/// must restate (or deliberately change, moving the guard with it) the
/// premise `parsing::duplicates_existing` already proves.
pub const SIMILARITY_THRESHOLD: f64 = 0.6;

/// The best-matching lesson for a post-mortem summary.
#[derive(Debug, Clone, PartialEq)]
pub struct LessonMatch {
    /// The matched lesson text (the dedupe key everywhere).
    pub text: String,
    /// Its word-overlap score with the summary, `>=` [`SIMILARITY_THRESHOLD`].
    pub score: f64,
}

/// Best lesson whose word-overlap with `summary` clears the threshold.
/// `None` when no candidate is plausible — the AC5 no-match shape. Ties
/// resolve to the FIRST candidate, so callers control determinism by the
/// order they pass.
#[must_use]
pub fn match_lessons(summary: &str, candidates: &[&str]) -> Option<LessonMatch> {
    let toks = title_tokens(summary);
    let mut best: Option<LessonMatch> = None;
    for candidate in candidates {
        let score = jaccard(&toks, &title_tokens(candidate));
        if score >= SIMILARITY_THRESHOLD && best.as_ref().map_or(true, |b| score > b.score) {
            best = Some(LessonMatch {
                text: (*candidate).to_owned(),
                score,
            });
        }
    }
    best
}

/// One incident linked to a lesson's recurrence, as the Hub lessons page
/// renders it.
#[derive(Debug, Serialize)]
pub struct RecurrenceView {
    /// RFC3339 when the recurrence was recorded.
    pub at: String,
    /// The incident's own stamp (the dismissal identity).
    pub incident_at: String,
    /// What triggered the incident (`deploy failed` / `tests failed` / …).
    pub incident_reason: String,
}

/// One lesson's efficacy row on the Hub lessons page (CXA-F306 AC2).
#[derive(Debug, Serialize)]
pub struct LessonEfficacyRow {
    pub text: String,
    /// RFC3339 when the lesson was first recorded (AC2 "when it was recorded").
    pub recorded_at: String,
    /// How often the same lesson had to be re-learned.
    pub re_recordings: u64,
    /// Recurrences since the lesson was recorded (AC2 "recurrence count").
    pub recurrence_count: u64,
    /// RFC3339 of the most recent recurrence (AC2), `None` before any.
    pub last_recurrence_at: Option<String>,
    /// The linked incidents (AC1 "the linked incident is visible").
    pub incidents: Vec<RecurrenceView>,
    /// Structural work has been escalated for this lesson.
    pub escalated: bool,
    /// How far up the chore→bug ladder the escalation climbed.
    pub stage: Option<String>,
    /// The prevention ticket filed for it.
    pub structural_ticket: Option<String>,
    /// 2+ recurrences (or 2+ re-learnings) — listed in the repeating section.
    pub repeating: bool,
    /// `Some("shipped")` when the lesson is shipped with the binary
    /// (CXA-F371) — the lessons UI badges it so shipped wisdom is
    /// distinguishable from locally learned lessons; `None` = learned locally.
    pub source: Option<String>,
}

/// The lesson-efficacy overlay served additively on
/// `GET /api/projects/:pid/metrics/summary` (the CXA-F230 `attention`
/// contract: old clients that never read the key are unaffected).
#[derive(Debug, Serialize)]
pub struct LessonEfficacy {
    /// How many lessons currently repeat (2+).
    pub repeaters: u64,
    /// Every tracked lesson, most-urgent first (deterministic).
    pub lessons: Vec<LessonEfficacyRow>,
}

/// Pure read model over the state's lesson ledger. Deterministic ordering:
/// recurrence count desc, then re-recordings desc, then text asc.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn lesson_efficacy(state: &ProjectState) -> LessonEfficacy {
    let mut rows: Vec<LessonEfficacyRow> = state
        .lesson_records
        .iter()
        .map(|record| LessonEfficacyRow {
            text: record.text.clone(),
            recorded_at: record.at.clone(),
            re_recordings: record.re_recordings,
            recurrence_count: record.recurrences.len() as u64,
            last_recurrence_at: record.recurrences.last().map(|e| e.at.clone()),
            incidents: record
                .recurrences
                .iter()
                .map(|e: &LessonRecurrence| RecurrenceView {
                    at: e.at.clone(),
                    incident_at: e.incident_at.clone(),
                    incident_reason: e.incident_reason.clone(),
                })
                .collect(),
            escalated: record.escalated.is_some(),
            stage: record.escalated.as_ref().map(|e| e.stage.clone()),
            structural_ticket: record.escalated.as_ref().map(|e| e.ticket.clone()),
            repeating: record.is_repeating(),
            source: record.source.clone(),
        })
        .collect();
    rows.sort_by(|a, b| {
        b.recurrence_count
            .cmp(&a.recurrence_count)
            .then(b.re_recordings.cmp(&a.re_recordings))
            .then(a.text.cmp(&b.text))
    });
    let repeaters = rows.iter().filter(|r| r.repeating).count() as u64;
    LessonEfficacy {
        repeaters,
        lessons: rows,
    }
}

#[cfg(test)]
mod efficacy_tests {
    use super::*;
    use crate::state::{LessonRecord, MAX_LESSON_RECORDS};

    #[test]
    fn a_summary_that_repeats_a_lesson_matches_it_above_the_threshold() {
        let hit = match_lessons(
            "deploy failed: docker build fails when the base image tag moves",
            &["docker build fails when the base image tag moves — pin the base image version"],
        )
        .expect("the repeat must match");
        assert!(hit.score >= SIMILARITY_THRESHOLD);
        assert!(hit.text.contains("pin the base image version"));
    }

    #[test]
    fn an_incident_with_no_plausible_matching_lesson_creates_no_match() {
        let candidates = [
            "always pin the docker base image version",
            "route PRs that touch gating files to their human approver at open",
        ];
        let summary =
            "tests failed: oauth token expired for engine claude — re-authenticate the runner";
        assert!(
            match_lessons(summary, &candidates).is_none(),
            "an unrelated failure must not fabricate a recurrence"
        );
    }

    #[test]
    fn empty_or_blank_summaries_never_match() {
        let candidates = ["pin the base image version"];
        assert!(match_lessons("", &candidates).is_none());
        assert!(match_lessons("   ", &candidates).is_none());
    }

    fn state_with(records: Vec<LessonRecord>) -> ProjectState {
        ProjectState {
            lesson_records: records,
            ..ProjectState::default()
        }
    }

    fn recurring(text: &str, count: usize) -> LessonRecord {
        let mut r = LessonRecord {
            text: text.to_owned(),
            at: "2026-09-01T10:00:00Z".to_owned(),
            cycle: 1,
            re_recordings: 0,
            recurrences: Vec::new(),
            escalated: None,
            id: None,
            source: None,
        };
        for i in 0..count {
            r.recurrences.push(crate::state::LessonRecurrence {
                at: format!("2026-09-0{}T10:00:00Z", (i % 9) + 1),
                incident_at: format!("2026-09-0{}T09:00:00Z", (i % 9) + 1),
                incident_reason: "deploy failed".to_owned(),
            });
        }
        r
    }

    #[test]
    fn one_recurrence_is_watched_but_only_two_is_repeating() {
        let eff = lesson_efficacy(&state_with(vec![
            recurring("watched once", 1),
            recurring("repeater", 2),
        ]));
        assert_eq!(eff.repeaters, 1);
        let watched = eff
            .lessons
            .iter()
            .find(|r| r.text == "watched once")
            .expect("row");
        assert!(
            !watched.repeating,
            "AC2: 2+ recurrences make the repeating section"
        );
        assert_eq!(
            watched.last_recurrence_at,
            Some("2026-09-01T10:00:00Z".to_owned()),
            "AC2: the most recent recurrence is on the row"
        );
        let rep = eff
            .lessons
            .iter()
            .find(|r| r.text == "repeater")
            .expect("row");
        assert!(rep.repeating);
    }

    #[test]
    fn a_re_recordings_only_lesson_is_a_repeater_too() {
        let mut r = recurring("had to re-learn this", 0);
        r.re_recordings = 2;
        let eff = lesson_efficacy(&state_with(vec![r]));
        assert_eq!(
            eff.repeaters, 1,
            "re-learning twice is the same failure recurring"
        );
    }

    #[test]
    fn escalation_fields_ride_the_row() {
        let mut r = recurring("structural case", 2);
        r.escalated = Some(crate::state::LessonEscalation {
            ticket: "CXC-C001".to_owned(),
            at: "2026-09-02T10:00:00Z".to_owned(),
            stage: "chore".to_owned(),
        });
        let eff = lesson_efficacy(&state_with(vec![r]));
        let row = &eff.lessons[0];
        assert!(row.escalated);
        assert_eq!(row.stage.as_deref(), Some("chore"));
        assert_eq!(row.structural_ticket.as_deref(), Some("CXC-C001"));
    }

    /// AC3's middle link (CXA-F371): the source tag rides the read model — a
    /// shipped record reaches the UI row marked, a locally learned record
    /// reaches it unmarked.
    #[test]
    fn the_source_tag_rides_the_read_model_row() {
        use crate::bootstrap_lessons::SHIPPED_SOURCE;
        let mut shipped = recurring("a shipped rule", 0);
        shipped.id = Some("CXA-F371-shipped-rule".to_owned());
        shipped.source = Some(SHIPPED_SOURCE.to_owned());
        let local = recurring("a locally learned rule", 0);
        let eff = lesson_efficacy(&state_with(vec![shipped, local]));
        let row = eff
            .lessons
            .iter()
            .find(|r| r.text == "a shipped rule")
            .expect("row");
        assert_eq!(
            row.source.as_deref(),
            Some(SHIPPED_SOURCE),
            "the shipped tag reaches the UI row"
        );
        let row = eff
            .lessons
            .iter()
            .find(|r| r.text == "a locally learned rule")
            .expect("row");
        assert!(row.source.is_none(), "a locally learned row stays unmarked");
    }

    #[test]
    fn ordering_is_deterministic_most_urgent_first() {
        let eff = lesson_efficacy(&state_with(vec![
            recurring("one hit", 1),
            recurring("three hits", 3),
            recurring("two hits", 2),
        ]));
        let texts: Vec<&str> = eff.lessons.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, ["three hits", "two hits", "one hit"]);
    }

    #[test]
    fn empty_state_yields_zero_repeaters_and_no_rows() {
        let eff = lesson_efficacy(&ProjectState::default());
        assert_eq!(eff.repeaters, 0);
        assert!(eff.lessons.is_empty());
    }

    #[test]
    fn the_ledger_cap_keeps_the_read_model_bounded() {
        let mut s = ProjectState::default();
        for i in 0..MAX_LESSON_RECORDS + 5 {
            s.record_lesson(&format!("lesson {i}"));
        }
        let eff = lesson_efficacy(&s);
        assert_eq!(eff.lessons.len(), MAX_LESSON_RECORDS);
    }
}
