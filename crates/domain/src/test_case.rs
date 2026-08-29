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
    /// RFC3339 timestamp the evidence was captured.
    #[serde(default)]
    pub at: String,
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
                Some(existing) => next.push(existing.clone()),
                None => next.push(TestCase {
                    description: ac.clone(),
                    status: TestCaseStatus::Pending,
                    evidence: None,
                    sources: Vec::new(),
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
        tc.evidence = Some(CaseEvidence {
            image: keep_image,
            note,
            at,
        });
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
        tc.evidence = Some(CaseEvidence {
            image: Some(image),
            note,
            at,
        });
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
}
