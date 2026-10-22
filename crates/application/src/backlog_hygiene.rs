//! Backlog hygiene — the SM's own triage of the on-hold mountain.
//!
//! `sm_drive` resumes held BUGS and on-hold tickets inside the sprint's
//! committed set; everything else parked `on_hold` is invisible to every
//! lane forever (307 of 446 tickets by 2026-10-22). This pass is the
//! missing proactive half, as a pure decision function over one snapshot:
//!
//! - RESUME an on-hold non-bug with no live children (a split parent stays
//!   held — its children ARE the plan), beyond the committed set.
//! - REJECT exact-duplicate titles among non-terminal tickets, keeping the
//!   OLDEST (it carries the history); dupes re-minted by sweeps/intake are
//!   the bulk of the pile.
//! - REPORT (never auto-close) the rest of the long-held: exhausted-budget
//!   bugs and aged holds are a human/PO call, surfaced in one digest line
//!   instead of silently rotting.

use serde::Serialize;
use std::collections::HashMap;

/// What the pass needs to know about one ticket — struct-literal small.
#[derive(Debug, Default, Clone)]
pub struct HygieneTicket {
    pub id: String,
    pub title: String,
    /// serde-style status key: `on_hold`, `open`, `pending`, …
    pub status: String,
    pub is_bug: bool,
    /// This ticket has at least one non-terminal child (split parent).
    pub has_live_children: bool,
    /// RFC3339 creation stamp, "" when unknown.
    pub created_at: String,
}

/// The decided actions, in apply order.
#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
pub struct HygienePlan {
    /// On-hold non-bugs with no live children → Pending.
    pub resume: Vec<String>,
    /// Younger exact-title duplicates → Rejected, with the kept ticket id.
    pub reject_duplicates: Vec<(String, String)>,
    /// Still-parked ids the digest should name for a human (no action).
    pub report_held: Vec<String>,
}

fn terminal(status: &str) -> bool {
    matches!(status, "done" | "documented" | "verified" | "rejected")
}

/// Decide the hygiene actions for one snapshot. Conservative on purpose:
/// duplicates must match on the EXACT normalized title, resumes never touch
/// bugs (sm_drive owns those) or split parents, and everything else is only
/// reported.
#[must_use]
pub fn plan(tickets: &[HygieneTicket]) -> HygienePlan {
    let mut out = HygienePlan::default();
    // Duplicate rejection: group non-terminal tickets by normalized title.
    let mut by_title: HashMap<String, Vec<&HygieneTicket>> = HashMap::new();
    for t in tickets.iter().filter(|t| !terminal(&t.status)) {
        let key = t.title.split_whitespace().collect::<Vec<_>>().join(" ");
        if key.len() >= 20 {
            // Short titles ("fix build") collide by accident, not duplication.
            by_title.entry(key.to_lowercase()).or_default().push(t);
        }
    }
    let mut dup_rejected: Vec<String> = Vec::new();
    for group in by_title.values_mut() {
        if group.len() < 2 {
            continue;
        }
        // Keep the oldest (history lives there); "" timestamps sort first,
        // so unknown-age tickets are treated as old rather than culled.
        group.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let keep = group[0].id.clone();
        for dupe in &group[1..] {
            // An in-progress duplicate is someone's live work — report, don't kill.
            if dupe.status == "in_progress" {
                continue;
            }
            dup_rejected.push(dupe.id.clone());
            out.reject_duplicates.push((dupe.id.clone(), keep.clone()));
        }
    }
    for t in tickets {
        if t.status != "on_hold" || dup_rejected.contains(&t.id) {
            continue;
        }
        if t.has_live_children {
            continue; // the children ARE the plan
        }
        if t.is_bug {
            // sm_drive resumes held bugs while rescue budget lasts; the rest
            // are the human pile — name them.
            out.report_held.push(t.id.clone());
        } else {
            out.resume.push(t.id.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: &str, title: &str, status: &str, bug: bool, kids: bool, at: &str) -> HygieneTicket {
        HygieneTicket {
            id: id.into(),
            title: title.into(),
            status: status.into(),
            is_bug: bug,
            has_live_children: kids,
            created_at: at.into(),
        }
    }

    #[test]
    fn resumes_childless_nonbug_holds_and_reports_held_bugs() {
        let p = plan(&[
            t(
                "F-1",
                "a feature parked during the outage window",
                "on_hold",
                false,
                false,
                "2026-10-01",
            ),
            t(
                "F-2",
                "a split parent with living children here",
                "on_hold",
                false,
                true,
                "2026-10-01",
            ),
            t(
                "B-1",
                "a bug whose rescue budget ran out already",
                "on_hold",
                true,
                false,
                "2026-10-01",
            ),
        ]);
        assert_eq!(p.resume, vec!["F-1"]);
        assert_eq!(p.report_held, vec!["B-1"]);
        assert!(p.reject_duplicates.is_empty());
    }

    #[test]
    fn exact_title_duplicates_reject_the_younger_keep_the_oldest() {
        let p = plan(&[
            t(
                "A-1",
                "scaffold self-test acceptance criteria unmet",
                "on_hold",
                true,
                false,
                "2026-10-01",
            ),
            t(
                "A-2",
                "scaffold self-test acceptance criteria unmet",
                "open",
                true,
                false,
                "2026-10-05",
            ),
            t(
                "A-3",
                "Scaffold self-test acceptance criteria UNMET",
                "on_hold",
                true,
                false,
                "2026-10-07",
            ),
        ]);
        assert_eq!(
            p.reject_duplicates,
            vec![
                ("A-2".to_owned(), "A-1".to_owned()),
                ("A-3".to_owned(), "A-1".to_owned())
            ]
        );
        // The rejected dupes never also appear in resume/report.
        assert_eq!(p.report_held, vec!["A-1"]);
    }

    #[test]
    fn short_titles_in_progress_and_terminal_tickets_are_left_alone() {
        let p = plan(&[
            t("S-1", "fix build", "on_hold", false, false, "2026-10-01"),
            t("S-2", "fix build", "open", false, false, "2026-10-02"),
            t(
                "W-1",
                "a long enough duplicate working title",
                "in_progress",
                false,
                false,
                "2026-10-01",
            ),
            t(
                "W-2",
                "a long enough duplicate working title",
                "in_progress",
                false,
                false,
                "2026-10-02",
            ),
            t(
                "D-1",
                "a long enough duplicate finished title",
                "documented",
                false,
                false,
                "2026-10-01",
            ),
            t(
                "D-2",
                "a long enough duplicate finished title",
                "on_hold",
                false,
                false,
                "2026-10-02",
            ),
        ]);
        assert!(p.reject_duplicates.is_empty());
        // S-1 is a childless non-bug hold → still resumed; D-2 too (its twin
        // is terminal so the group has one live member).
        assert_eq!(p.resume, vec!["S-1", "D-2"]);
    }
}
