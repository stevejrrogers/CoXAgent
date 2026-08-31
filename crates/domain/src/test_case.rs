//! The ticket's test cases — the executable form of its acceptance criteria,
//! each carrying its own verdict and evidence. Split from `ticket.rs` along
//! that seam: the aggregate owns the checklist, this file is everything about
//! how a criterion is demonstrated and verified.

use serde::{Deserialize, Serialize};

/// The status of a single test case as verified by the TEST agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestCaseStatus {
    /// Not yet verified.
    Pending,
    /// The TEST agent marked this case as passing its acceptance criterion.
    Passed,
    /// The TEST agent found the case failing (this is usually a bug in the
    /// ticket's own DoD, distinct from a separately-filed bug ticket).
    Failed,
}

fn default_pending() -> TestCaseStatus {
    TestCaseStatus::Pending
}

/// How far back a case's verdict history reaches (CXA-F251): past this many
/// records the oldest trim first, so the newest cycles — the ones a reviewer
/// decides on — always survive. Bounded, the same way the governance ledger
/// is bounded, so a churning ticket cannot grow state.json without end.
pub const MAX_VERDICT_HISTORY: usize = 12;

/// What one verdict-history record represents: a pass/fail verdict rendered
/// by the TEST agent, or an evidence-only refresh (the deterministic
/// screenshot pass attaching proof with no new verdict). The distinction is
/// the churn view's honesty rule — a screenshot arriving must never read as
/// a fresh verdict transition at the verify surface.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    /// A pass/fail verdict: counts as a step in the criterion's trajectory.
    /// Also the reading when a record arrives without a kind (hand-edited
    /// state): the missing marker defaults to the plain verdict it describes.
    #[default]
    Verdict,
    /// Evidence refreshed without a new verdict: carried for completeness,
    /// never rendered as a trajectory step.
    EvidenceRefresh,
}

/// One immutable entry in a test case's verdict history (CXA-F251): a
/// snapshot of the case's verdict state at `at`. Records are appended in
/// call order — never edited or reordered — so the criterion's ordered
/// pass/fail trajectory across send-back cycles is reconstructable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictRecord {
    #[serde(default)]
    pub kind: RecordKind,
    pub status: TestCaseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// RFC3339 capture time — the trajectory's ordering key.
    #[serde(default)]
    pub at: String,
}

/// Keep a case's history bounded: past [`MAX_VERDICT_HISTORY`] records the
/// oldest trim first. The newest records — the current cycle's — are the ones
/// a reviewer decides on, so they are the ones that survive the cap.
fn trim_verdict_history(tc: &mut TestCase) {
    let overflow = tc.history.len().saturating_sub(MAX_VERDICT_HISTORY);
    if overflow > 0 {
        tc.history.drain(0..overflow);
    }
}

/// Optional per-test-case proof attached when the TEST agent verifies a case.
/// A real captured image (project media URL) and/or a short reproducible note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseEvidence {
    /// Project media URL of a captured screenshot (e.g. `/api/projects/../media/...`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Who/what verified it, or how to reproduce.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The resolved live-reproduction URL for this criterion (CXA-F248): an
    /// opaque, already-resolved address on the deployed app that a reviewer
    /// can open to see the criterion demonstrated. `None` — never a blank or
    /// guessed placeholder — when no route resolved onto a known live base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repro: Option<String>,
    /// RFC3339 timestamp the evidence was captured.
    #[serde(default)]
    pub at: String,
}

impl CaseEvidence {
    /// Bare evidence for a case that has provenance but no proof yet — the
    /// repro setter hangs the resolved link off this without inventing an
    /// image, note or timestamp.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            image: None,
            note: None,
            repro: None,
            at: String::new(),
        }
    }
}

/// One test case on a ticket — the executable form of an acceptance
/// criterion — carrying its own verdict and optional evidence. Kept aligned
/// with `acceptance_criteria` by the agents; each case is marked pass/fail by
/// the TEST agent when it verifies the ticket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestCase {
    pub description: String,
    #[serde(default = "default_pending")]
    pub status: TestCaseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<CaseEvidence>,
    /// What demonstrates this criterion: relative test-file paths, or an API
    /// request/response line for non-UI tickets (CXA-F024). Written by the
    /// application layer's traceability matcher; the coverage matrix surfaces
    /// it. `serde(default)` so pre-matrix tickets load clean.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    /// The case's verdict history (CXA-F251): one append-only record per
    /// verdict or evidence refresh, oldest first, capped at
    /// [`MAX_VERDICT_HISTORY`]. `serde(default)` keeps pre-history snapshots
    /// loading untouched under the same schema version; omitted on the wire
    /// when empty, so old readers and old tickets see no change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<VerdictRecord>,
}

use crate::ticket::Ticket;

impl Ticket {
    /// The ticket's test cases — one per acceptance criterion, each carrying
    /// its own verdict and optional per-case evidence.
    #[must_use]
    pub fn test_cases(&self) -> &[TestCase] {
        self.test_case_list()
    }

    /// Reconcile `test_cases` against the current `acceptance_criteria`:
    /// drop stale cases, add newly-appeared criteria as `Pending`, and keep
    /// the verdict/evidence of cases that still exist. Call before persisting
    /// after criteria change, or at load time.
    pub fn sync_test_cases_from_acceptance(&mut self) {
        let criteria = self.acceptance_criteria().to_vec();
        let cases = self.test_case_list_mut();
        let mut next: Vec<TestCase> = Vec::with_capacity(criteria.len());
        for ac in &criteria {
            match cases.iter().find(|t| t.description == *ac) {
                // A criterion that survives reconcile keeps its whole history:
                // the trajectory must survive the criteria being re-synced.
                Some(existing) => next.push(existing.clone()),
                None => next.push(TestCase {
                    description: ac.clone(),
                    status: TestCaseStatus::Pending,
                    evidence: None,
                    sources: Vec::new(),
                    history: Vec::new(),
                }),
            }
        }
        *cases = next;
    }

    /// Seed `test_cases` from the acceptance criteria, preserving any existing
    /// verdict/evidence for criteria that already have a case. No-op when the
    /// ticket already has as many cases as criteria.
    pub fn ensure_test_cases_from_acceptance(&mut self) {
        // In sync only when every case matches its criterion in order. A length
        // match alone is NOT enough — if criteria were edited to new text (same
        // count) the old descriptions would otherwise shadow the new ones and
        // `set_test_case_result` would silently fail to match.
        let in_sync = self.test_cases().len() == self.acceptance_criteria().len()
            && self
                .test_cases()
                .iter()
                .zip(self.acceptance_criteria())
                .all(|(tc, ac)| tc.description == *ac);
        if in_sync {
            return;
        }
        self.sync_test_cases_from_acceptance();
    }

    /// Mark one test case pass (or fail, `passed=false`) by its description,
    /// attaching optional per-case evidence (an image URL + reproducible note)
    /// and a capture timestamp. A `None` image keeps any image already attached
    /// so a verdict-only pass never erases an existing screenshot. Returns
    /// `false` when no case matched.
    pub fn set_test_case_result(
        &mut self,
        description: &str,
        passed: bool,
        note: Option<String>,
        image: Option<String>,
        at: String,
    ) -> bool {
        let Some(tc) = self
            .test_case_list_mut()
            .iter_mut()
            .find(|t| t.description == description)
        else {
            return false;
        };
        tc.status = if passed {
            TestCaseStatus::Passed
        } else {
            TestCaseStatus::Failed
        };
        let keep_image = image.or_else(|| tc.evidence.as_ref().and_then(|e| e.image.clone()));
        let keep_repro = tc.evidence.as_ref().and_then(|e| e.repro.clone());
        tc.evidence = Some(CaseEvidence {
            image: keep_image,
            note,
            repro: keep_repro,
            at: at.clone(),
        });
        // History is the churn view's raw data (CXA-F251): EVERY call appends
        // one verdict record — a re-run with the same verdict included — so a
        // criterion that flips pass->fail->pass shows all three steps keyed
        // by their `at` order, not just the latest state.
        tc.history.push(VerdictRecord {
            kind: RecordKind::Verdict,
            status: tc.status,
            note: tc.evidence.as_ref().and_then(|e| e.note.clone()),
            image: tc.evidence.as_ref().and_then(|e| e.image.clone()),
            at,
        });
        trim_verdict_history(tc);
        true
    }

    /// Attach a captured screenshot URL to a test case's evidence without
    /// changing its verdict (the TEST agent already marked pass/fail; this is
    /// the cycle's deterministic screenshot pass filling in the image). Returns
    /// `false` when no case matched.
    pub fn set_test_case_image(&mut self, description: &str, image: String, at: String) -> bool {
        let Some(tc) = self
            .test_case_list_mut()
            .iter_mut()
            .find(|t| t.description == description)
        else {
            return false;
        };
        let note = tc.evidence.as_ref().and_then(|e| e.note.clone());
        let repro = tc.evidence.as_ref().and_then(|e| e.repro.clone());
        tc.evidence = Some(CaseEvidence {
            image: Some(image),
            note,
            repro,
            at: at.clone(),
        });
        // Appends as an EVIDENCE-REFRESH record, not a verdict (CXA-F251):
        // the deterministic screenshot pass must fill in proof without
        // fabricating a verdict transition in the churn trajectory.
        tc.history.push(VerdictRecord {
            kind: RecordKind::EvidenceRefresh,
            status: tc.status,
            note: tc.evidence.as_ref().and_then(|e| e.note.clone()),
            image: tc.evidence.as_ref().and_then(|e| e.image.clone()),
            at,
        });
        trim_verdict_history(tc);
        true
    }

    /// Attach the resolved live-reproduction URL to a test case's evidence
    /// without changing its verdict or the rest of the evidence (CXA-F248).
    /// The caller resolves the verdict's route onto the known live base; a
    /// blank url is refused so an unresolved route stays ABSENT rather than
    /// becoming a fabricated link. Returns `false` when no case matched or
    /// the url is blank.
    pub fn set_test_case_repro(&mut self, description: &str, repro: String) -> bool {
        if repro.trim().is_empty() {
            return false;
        }
        let Some(tc) = self
            .test_case_list_mut()
            .iter_mut()
            .find(|t| t.description == description)
        else {
            return false;
        };
        let evidence = tc.evidence.get_or_insert_with(CaseEvidence::empty);
        evidence.repro = Some(repro);
        true
    }

    /// Record WHAT demonstrates a test case — relative test-file paths, or an
    /// API request/response line when the evidence is not file-based
    /// (CXA-F024). Provenance only: it never changes the verdict. Returns
    /// `false` when no case matched.
    pub fn set_test_case_sources(&mut self, description: &str, sources: Vec<String>) -> bool {
        let Some(tc) = self
            .test_case_list_mut()
            .iter_mut()
            .find(|t| t.description == description)
        else {
            return false;
        };
        tc.sources = sources;
        true
    }

    /// The test-to-AC traceability matrix (CXA-F024): one row per acceptance
    /// criterion with its coverage status and the evidence addressing it.
    /// Computed fresh on every read — always consistent with current state.
    #[must_use]
    pub fn coverage_matrix(&self) -> Vec<crate::coverage::CoverageEntry> {
        crate::coverage::matrix_for(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TicketId;
    use crate::kinds::{Complexity, Priority, TicketType};

    fn feature() -> Ticket {
        Ticket::new(
            TicketId::new("FEAT-001").expect("id"),
            TicketType::Feature,
            "A feature",
            "desc",
            Priority::Medium,
            Complexity::Medium,
            false,
        )
        .expect("ticket")
    }

    #[test]
    fn ensure_syncs_when_criteria_content_changes_same_count() {
        let mut t = feature();
        t.set_acceptance_criteria(vec!["old ac".to_owned()]);
        t.ensure_test_cases_from_acceptance();
        assert_eq!(t.test_cases().len(), 1);
        assert_eq!(t.test_cases()[0].description, "old ac");
        // Criteria edited to NEW text but still one entry: length alone is not
        // enough — ensure must resync so the case tracks the new criterion.
        t.set_acceptance_criteria(vec!["new ac".to_owned()]);
        t.ensure_test_cases_from_acceptance();
        assert_eq!(t.test_cases().len(), 1);
        assert_eq!(t.test_cases()[0].description, "new ac");
        assert_eq!(t.test_cases()[0].status, TestCaseStatus::Pending);
    }

    #[test]
    fn set_result_keeps_attached_image_when_none_supplied() {
        let mut t = feature();
        t.set_acceptance_criteria(vec!["ac one".to_owned()]);
        t.ensure_test_cases_from_acceptance();
        assert!(
            t.set_test_case_result(
                "ac one",
                true,
                Some("verified".to_owned()),
                None,
                "t1".into(),
            ),
            "verdict without image still matches"
        );
        // Later the cycle attaches a screenshot, then a re-run's verdict
        // (image=None) must NOT wipe it.
        assert!(t.set_test_case_image("ac one", "/api/.../shot.png".into(), "t2".into()));
        assert_eq!(
            t.test_cases()[0]
                .evidence
                .as_ref()
                .unwrap()
                .image
                .as_deref(),
            Some("/api/.../shot.png")
        );
        assert!(t.set_test_case_result(
            "ac one",
            false,
            Some("now failing".to_owned()),
            None,
            "t3".into()
        ));
        let ev = t.test_cases()[0].evidence.as_ref().unwrap();
        assert_eq!(
            ev.image.as_deref(),
            Some("/api/.../shot.png"),
            "image survives a verdict-only re-run"
        );
        assert_eq!(ev.note.as_deref(), Some("now failing"));
        assert_eq!(t.test_cases()[0].status, TestCaseStatus::Failed);
    }

    #[test]
    fn set_result_overwrites_image_when_one_supplied() {
        let mut t = feature();
        t.set_acceptance_criteria(vec!["ac".to_owned()]);
        t.ensure_test_cases_from_acceptance();
        assert!(t.set_test_case_result("ac", true, None, Some("/old.png".into()), "t1".into()));
        assert!(t.set_test_case_result("ac", true, None, Some("/new.png".into()), "t2".into()));
        assert_eq!(
            t.test_cases()[0]
                .evidence
                .as_ref()
                .unwrap()
                .image
                .as_deref(),
            Some("/new.png")
        );
    }

    #[test]
    fn set_result_false_when_no_case_matches() {
        let mut t = feature();
        t.set_acceptance_criteria(vec!["ac".to_owned()]);
        t.ensure_test_cases_from_acceptance();
        assert!(!t.set_test_case_result("does not exist", true, None, None, "t1".into()));
    }

    // --- CXA-F248: per-criterion reproduction routes on CaseEvidence ----------

    /// A ticket whose one criterion already carries note evidence — the state
    /// every repro write must preserve.
    fn case_with_note(note: &str) -> Ticket {
        let mut t = feature();
        t.set_acceptance_criteria(vec!["settings persist".to_owned()]);
        t.ensure_test_cases_from_acceptance();
        assert!(t.set_test_case_result(
            "settings persist",
            true,
            Some(note.to_owned()),
            None,
            "t1".into(),
        ));
        t
    }

    /// Old snapshots deserialize unchanged: a record written before CXA-F248
    /// carries no `repro` key, loads as `None`, and re-serializes without the
    /// key — backward and forward compatible, no migration.
    #[test]
    fn repro_is_none_and_omitted_on_the_wire_for_pre_f248_records() {
        let t = case_with_note("GET /settings 200");
        let ev = t.test_cases()[0].evidence.as_ref().expect("evidence");
        assert_eq!(ev.repro, None, "no route resolved yet — repro is None");
        let v = serde_json::to_value(ev).expect("serialize");
        assert!(
            v.get("repro").is_none(),
            "an unresolved route is OMITTED, never serialized as a guessed link: {v}"
        );
        let round: CaseEvidence = serde_json::from_value(v).expect("deserialize pre-F248 record");
        assert_eq!(
            round, *ev,
            "old==new comparison holds through the round trip"
        );
    }

    /// The full recording story: a resolved repro attaches beside existing
    /// evidence without touching the verdict, survives a verdict-only re-run
    /// and a screenshot pass, and rides persistence.
    #[test]
    fn set_repro_preserves_note_image_and_status() {
        let mut t = case_with_note("GET /settings 200");
        assert!(t.set_test_case_repro("settings persist", "http://127.0.0.1:8101/settings".into(),));
        let ev = t.test_cases()[0].evidence.as_ref().expect("evidence");
        assert_eq!(
            ev.repro.as_deref(),
            Some("http://127.0.0.1:8101/settings"),
            "the resolved route is recorded verbatim — the domain parses nothing"
        );
        assert_eq!(ev.note.as_deref(), Some("GET /settings 200"));
        assert_eq!(t.test_cases()[0].status, TestCaseStatus::Passed);

        // A screenshot pass attaches the image; the repro must survive it.
        assert!(t.set_test_case_image(
            "settings persist",
            "/api/projects/cxa/media/s.png".into(),
            "t2".into()
        ));
        // A re-run's verdict (no image) must not wipe either.
        assert!(t.set_test_case_result(
            "settings persist",
            false,
            Some("now failing".into()),
            None,
            "t3".into()
        ));
        let ev = t.test_cases()[0].evidence.as_ref().expect("evidence");
        assert_eq!(ev.repro.as_deref(), Some("http://127.0.0.1:8101/settings"));
        assert_eq!(ev.image.as_deref(), Some("/api/projects/cxa/media/s.png"));
        assert_eq!(ev.note.as_deref(), Some("now failing"));
        assert_eq!(t.test_cases()[0].status, TestCaseStatus::Failed);

        // Persisted through the aggregate's serde and back.
        let v = serde_json::to_value(&t).expect("serialize ticket");
        let back: Ticket = serde_json::from_value(v).expect("deserialize ticket");
        assert_eq!(
            back, t,
            "the recorded repro round-trips through the store payload"
        );
    }

    /// An unresolved route never becomes a fabricated link: blank (and
    /// whitespace-only) urls are refused, and an unknown case matches nothing.
    #[test]
    fn set_repro_refuses_blank_and_unknown_case() {
        let mut t = case_with_note("GET /settings 200");
        assert!(
            !t.set_test_case_repro("settings persist", String::new()),
            "empty is not a reproduction"
        );
        assert!(!t.set_test_case_repro("settings persist", "   ".into()));
        assert!(!t.set_test_case_repro("no such criterion", "http://x/".into()));
        let ev = t.test_cases()[0].evidence.as_ref().expect("evidence");
        assert_eq!(ev.repro, None, "refusals leave the field absent");
        assert_eq!(
            ev.note.as_deref(),
            Some("GET /settings 200"),
            "note untouched"
        );
    }

    /// A re-run's freshly resolved route REPLACES the recorded one — the
    /// freshest verification is what a reviewer should open.
    #[test]
    fn set_repro_overwrites_with_the_fresher_route() {
        let mut t = case_with_note("GET /settings 200");
        assert!(t.set_test_case_repro("settings persist", "http://127.0.0.1:8101/settings".into()));
        assert!(t.set_test_case_repro(
            "settings persist",
            "http://127.0.0.1:8101/settings#v2".into()
        ));
        assert_eq!(
            t.test_cases()[0]
                .evidence
                .as_ref()
                .expect("evidence")
                .repro
                .as_deref(),
            Some("http://127.0.0.1:8101/settings#v2"),
            "the latest resolved route wins"
        );
    }

    /// Repro attaches even when the case has no evidence yet (no note/image):
    /// the evidence record materializes with just the link.
    #[test]
    fn set_repro_creates_evidence_when_absent() {
        let mut t = feature();
        t.set_acceptance_criteria(vec!["bare criterion".to_owned()]);
        t.ensure_test_cases_from_acceptance();
        assert!(t.test_cases()[0].evidence.is_none());
        assert!(t.set_test_case_repro("bare criterion", "http://127.0.0.1:8101/x".into()));
        let ev = t.test_cases()[0]
            .evidence
            .as_ref()
            .expect("evidence created");
        assert_eq!(ev.repro.as_deref(), Some("http://127.0.0.1:8101/x"));
        assert_eq!(ev.image, None);
        assert_eq!(ev.note, None);
        assert_eq!(ev.at, "");
    }

    // --- Verdict-history churn data (CXA-F251) ---

    #[test]
    fn every_result_call_appends_one_verdict_record_in_order() {
        let mut t = feature();
        t.set_acceptance_criteria(vec!["ac".to_owned()]);
        t.ensure_test_cases_from_acceptance();
        assert!(t.set_test_case_result("ac", true, Some("v1".to_owned()), None, "t1".into()));
        assert!(t.set_test_case_result("ac", false, Some("v2".to_owned()), None, "t2".into()));
        assert!(t.set_test_case_result("ac", true, None, None, "t3".into()));
        let h = &t.test_cases()[0].history;
        assert_eq!(h.len(), 3, "one record per call, transitions included");
        assert!(h.iter().all(|r| r.kind == RecordKind::Verdict));
        // Oldest-first, keyed by the `at` the caller stamped.
        assert_eq!(
            h.iter().map(|r| r.at.as_str()).collect::<Vec<_>>(),
            vec!["t1", "t2", "t3"],
            "append order is chronological by construction"
        );
        assert_eq!(
            h.iter().map(|r| r.status).collect::<Vec<_>>(),
            vec![
                TestCaseStatus::Passed,
                TestCaseStatus::Failed,
                TestCaseStatus::Passed
            ],
            "the full PASS->FAIL->PASS trajectory is on record"
        );
        // The current state still reads as today (unchanged contract).
        assert_eq!(t.test_cases()[0].status, TestCaseStatus::Passed);
    }

    #[test]
    fn an_identical_status_rerun_still_appends_so_rework_is_visible() {
        let mut t = feature();
        t.set_acceptance_criteria(vec!["ac".to_owned()]);
        t.ensure_test_cases_from_acceptance();
        assert!(t.set_test_case_result("ac", true, None, None, "t1".into()));
        assert!(t.set_test_case_result("ac", true, None, None, "t2".into()));
        assert_eq!(t.test_cases()[0].history.len(), 2);
    }

    #[test]
    fn an_image_refresh_is_recorded_but_never_as_a_verdict() {
        let mut t = feature();
        t.set_acceptance_criteria(vec!["ac".to_owned()]);
        t.ensure_test_cases_from_acceptance();
        assert!(t.set_test_case_result("ac", true, None, None, "t1".into()));
        assert!(t.set_test_case_image("ac", "/api/.../shot.png".into(), "t2".into()));
        let h = &t.test_cases()[0].history;
        assert_eq!(h.len(), 2, "the screenshot pass appends too");
        assert_eq!(h[1].kind, RecordKind::EvidenceRefresh);
        assert_eq!(
            h[1].status,
            TestCaseStatus::Passed,
            "verdict state unchanged"
        );
        // The trajectory a reviewer reads filters to verdicts only: the
        // screenshot pass did not fabricate a second PASS transition.
        let verdicts: Vec<_> = h.iter().filter(|r| r.kind == RecordKind::Verdict).collect();
        assert_eq!(verdicts.len(), 1);
    }

    #[test]
    fn history_is_capped_keeping_the_newest_records() {
        let mut t = feature();
        t.set_acceptance_criteria(vec!["ac".to_owned()]);
        t.ensure_test_cases_from_acceptance();
        for i in 0..(MAX_VERDICT_HISTORY + 2) {
            assert!(t.set_test_case_result("ac", i % 2 == 0, None, None, format!("t{i}")));
        }
        let h = &t.test_cases()[0].history;
        assert_eq!(h.len(), MAX_VERDICT_HISTORY, "bounded at the cap");
        assert_eq!(h[0].at, "t2", "the two oldest trimmed, newest kept");
        assert_eq!(h.last().expect("records").at, "t13");
    }

    #[test]
    fn sync_preserves_history_for_surviving_criteria_and_starts_fresh_for_new() {
        let mut t = feature();
        t.set_acceptance_criteria(vec!["ac one".to_owned(), "ac two".to_owned()]);
        t.ensure_test_cases_from_acceptance();
        assert!(t.set_test_case_result("ac one", false, None, None, "t1".into()));
        assert!(t.set_test_case_result("ac two", true, None, None, "t1".into()));
        // Criteria re-synced: "ac one" survives (edited text replaced "ac two").
        t.set_acceptance_criteria(vec!["ac one".to_owned(), "ac three".to_owned()]);
        t.sync_test_cases_from_acceptance();
        let cases = t.test_cases();
        let one = cases
            .iter()
            .find(|c| c.description == "ac one")
            .expect("kept");
        assert_eq!(
            one.history.len(),
            1,
            "surviving criterion keeps its history"
        );
        let three = cases
            .iter()
            .find(|c| c.description == "ac three")
            .expect("new");
        assert!(
            three.history.is_empty(),
            "a new criterion starts with no history"
        );
        assert_eq!(three.status, TestCaseStatus::Pending);
    }

    #[test]
    fn a_pre_history_snapshot_round_trips_without_migration() {
        // The serde contract old projects rely on: a ticket persisted before
        // history existed carries no `history` key, loads clean under the
        // same schema version, and stays byte-stable when nothing re-runs.
        let legacy = r#"{
            "description": "ac",
            "status": "passed",
            "evidence": {"image": "/a.png", "note": "ok", "at": "t0"}
        }"#;
        let case: TestCase = serde_json::from_str(legacy).expect("legacy case loads");
        assert!(case.history.is_empty(), "no fabricated history");
        let wire = serde_json::to_value(&case).expect("serialize");
        assert!(
            wire.get("history").is_none(),
            "empty history is omitted, so old state files are not rewritten"
        );
        // ...and the first re-run appends onto the empty history normally.
        let mut t = feature();
        t.set_acceptance_criteria(vec!["ac".to_owned()]);
        t.ensure_test_cases_from_acceptance();
        t.test_case_list_mut()[0] = case;
        assert!(t.set_test_case_result("ac", true, None, None, "t1".into()));
        assert_eq!(t.test_cases()[0].history.len(), 1);
        assert_eq!(t.test_cases()[0].history[0].at, "t1");
    }

    #[test]
    fn a_history_record_without_a_kind_reads_as_a_plain_verdict() {
        // The `kind` marker must never be load-bearing for the whole state
        // document: a hand-written record omitting it loads as the verdict it
        // describes, the same way the churn view reads it.
        let record: VerdictRecord =
            serde_json::from_str(r#"{"status":"failed","at":"t1"}"#).expect("loads");
        assert_eq!(record.kind, RecordKind::Verdict);
        assert_eq!(record.status, TestCaseStatus::Failed);
    }
}
