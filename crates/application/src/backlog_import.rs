//! `backlog_import` — CXA-F258: merge a connected repo's forge issue backlog
//! into the project state as Pending tickets, so a team adopting CoXAgent on
//! an existing repo starts on the real backlog, not a hand-typed stand-in.
//!
//! The merge is a PURE function over `(state, drafts)` — no IO. The caller
//! fetches drafts through a port ([`crate::ports::outbound::ForgePort::
//! list_open_issues`]) and persists the merged state through the store in one
//! save (the `GitPort::working_tree` → pure decision → one save pattern).

use crate::ports::outbound::IssueDraft;
use crate::state::ProjectState;
use crate::use_cases::add_ticket::mint_id;
use coxagent_domain::{Complexity, Priority, Status, Ticket, TicketType};
use std::collections::HashSet;
use std::fmt::Write as _;

/// Cap on issues imported per run: a brownfield backlog of thousands must not
/// flood the Pending queue in one adoption. Anything beyond the cap is
/// reported as skipped and a re-run picks it up.
pub const IMPORT_CAP: usize = 50;

/// Outcome of one import run — AC3's report as data (not prose): the caller
/// surfaces `imported` vs `skipped` verbatim.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct ImportReport {
    /// Issues that became a new Pending ticket this run.
    pub imported: usize,
    /// Issues already tracked (same source issue or same title), duplicated
    /// within the batch, beyond the cap, or refused by construction.
    pub skipped: usize,
}

/// The ticket body an imported issue gets: the issue body verbatim (images
/// and attachments are NOT copied), then the back-link to the source issue
/// and its labels — so the ticket always leads home, and the labels (e.g.
/// `bug`) survive for triage even though the forge's taxonomy is not trusted
/// for status or priority.
fn imported_body(draft: &IssueDraft) -> String {
    let mut out = draft.body.trim().to_owned();
    if !draft.url.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        let _ = writeln!(out, "Imported from issue #{}: {}", draft.number, draft.url);
    }
    if !draft.labels.is_empty() {
        let _ = writeln!(out, "Labels: {}", draft.labels.join(", "));
    }
    out
}

/// Merge forge issue drafts into `state` as Pending tickets — the pure half
/// of the brownfield backlog import (CXA-F258).
///
/// Skipping rules, in order:
/// 1. the source issue is already tracked — any existing ticket's description
///    embeds the draft's source URL (status-independent: an issue whose
///    imported ticket later reached Done stays imported), or its title
///    matches an active or rejected ticket ([`crate::parsing::
///    duplicates_existing`], the ONE shared creation path's predicate — the
///    design's "skip duplicate titles");
/// 2. the per-run `cap` is reached;
/// 3. the same issue (URL) or title already appeared earlier in `drafts`.
///
/// Every imported ticket: `TicketType::Feature` (see the design note below),
/// `Priority::Medium` — the project default; no priority label is trusted
/// from the forge — `Complexity::Small`, empty acceptance criteria (a forge
/// issue has no checklist the agents can verify) and a stamped `created_at`.
///
/// DESIGN NOTE (deviation flagged to the SA): the design's `bug label →
/// TicketType::Bug` is NOT taken. A `Bug` starts `Open` (domain invariant),
/// the transition table has no `Open -> Pending` edge for ANY actor, and
/// `Open` bugs sit in `selection::open_bug_candidates` — the agent claim
/// queue. A bug-mapped import would therefore land `Open`, violating the
/// design's own "Status::Pending", AC2's "creates Pending tickets", and the
/// never-auto-enter-the-claim-pipeline edge case. The `bug` label is
/// preserved in the ticket body so triage keeps the signal.
#[must_use]
pub fn merge_pending(state: &mut ProjectState, drafts: &[IssueDraft], cap: usize) -> ImportReport {
    let mut report = ImportReport::default();

    // The shared creation path's duplicate lists (same predicate classes as
    // `AddTicketUseCase`): active themes block, rejected themes block — a
    // human "no" must not be re-imported behind their back.
    let active: Vec<String> = state
        .tickets
        .iter()
        .filter(|t| {
            !matches!(
                t.status(),
                Status::Done | Status::Documented | Status::Rejected | Status::Verified
            )
        })
        .map(|t| t.title().to_owned())
        .collect();
    let rejected: Vec<String> = state
        .tickets
        .iter()
        .filter(|t| t.status() == Status::Rejected)
        .map(|t| t.title().to_owned())
        .collect();

    let mut batch_urls: HashSet<String> = HashSet::new();
    let mut batch_titles: Vec<String> = Vec::new();

    for draft in drafts {
        if report.imported >= cap || draft.title.trim().is_empty() {
            report.skipped += 1;
            continue;
        }
        // AC3's teeth: dedupe on the SOURCE issue's identity (its URL in the
        // persisted backlog), not just on wording. The needle is the URL with
        // its trailing newline — `imported_body` always newline-terminates the
        // link line — because a bare substring match would read issue #71's
        // `.../issues/71` as already tracking #7 (`.../issues/7`).
        if !draft.url.is_empty()
            && (state
                .tickets
                .iter()
                .any(|t| t.description().contains(&format!("{}\n", draft.url)))
                || !batch_urls.insert(draft.url.clone()))
        {
            report.skipped += 1;
            continue;
        }
        if crate::parsing::duplicates_existing(&draft.title, &active)
            || crate::parsing::duplicates_existing(&draft.title, &rejected)
            || crate::parsing::duplicates_existing(&draft.title, &batch_titles)
        {
            report.skipped += 1;
            continue;
        }

        // Both constructors only refuse on inputs guarded above (a non-empty
        // id string; a non-blank title); a refusal here skips ONE draft
        // instead of failing the batch.
        let Ok(id) = mint_id(TicketType::Feature, state) else {
            report.skipped += 1;
            continue;
        };
        let Ok(mut ticket) = Ticket::new(
            id,
            TicketType::Feature,
            draft.title.trim(),
            imported_body(draft),
            Priority::Medium,
            Complexity::Small,
            false,
        ) else {
            report.skipped += 1;
            continue;
        };
        ticket.stamp_created_at(crate::state::now_rfc3339());
        state.tickets.push(ticket);
        batch_titles.push(draft.title.clone());
        report.imported += 1;
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use coxagent_domain::Role;

    fn draft(number: u64, title: &str, body: &str, labels: &[&str]) -> IssueDraft {
        IssueDraft {
            number,
            title: title.to_owned(),
            body: body.to_owned(),
            labels: labels.iter().map(|l| (*l).to_owned()).collect(),
            url: format!("https://github.com/owner/repo/issues/{number}"),
        }
    }

    #[test]
    fn imports_open_issues_as_pending_feature_tickets() {
        let mut state = ProjectState {
            alias: "CXA".to_owned(),
            ..ProjectState::default()
        };

        let report = merge_pending(
            &mut state,
            &[draft(
                7,
                "Support CSV export",
                "The reports page cannot export CSV.",
                &["enhancement"],
            )],
            IMPORT_CAP,
        );

        assert_eq!(report.imported, 1);
        assert_eq!(report.skipped, 0);
        let t = &state.tickets[0];
        assert_eq!(t.status(), Status::Pending, "AC2: imports land Pending");
        assert_eq!(t.ticket_type(), TicketType::Feature);
        assert_eq!(t.title(), "Support CSV export");
        assert!(
            t.description().contains("cannot export CSV"),
            "the issue body is carried"
        );
        assert!(
            t.description()
                .contains("https://github.com/owner/repo/issues/7"),
            "AC2: the ticket links back to the source issue"
        );
        assert_eq!(t.priority(), Priority::Medium);
        assert!(t.acceptance_criteria().is_empty());
        assert!(t.created_at().is_some(), "filed time is stamped");
    }

    #[test]
    fn rerun_is_idempotent_and_reports_imported_vs_skipped() {
        let mut state = ProjectState {
            alias: "CXA".to_owned(),
            ..ProjectState::default()
        };
        let drafts = vec![
            draft(7, "Support CSV export", "body seven", &[]),
            draft(8, "Fix flaky deploy check", "body eight", &[]),
        ];

        let first = merge_pending(&mut state, &drafts, IMPORT_CAP);
        assert_eq!((first.imported, first.skipped), (2, 0));

        // The same repo re-imported: nothing new, everything accounted for.
        let second = merge_pending(&mut state, &drafts, IMPORT_CAP);
        assert_eq!(
            (second.imported, second.skipped),
            (0, 2),
            "AC3: a re-run creates no duplicates and reports the counts"
        );
        assert_eq!(state.tickets.len(), 2);
    }

    #[test]
    fn rerun_after_terminal_status_still_skips() {
        // An imported issue whose ticket later reached Done must not be
        // re-imported — the source URL is the identity, not the title match.
        let mut state = ProjectState::default();
        let _ = merge_pending(
            &mut state,
            &[draft(7, "Ship dark mode", "b", &[])],
            IMPORT_CAP,
        );
        // Walk the imported ticket to Done the way the pipeline does: the SA
        // attaches the technical design (the DoR gate requires it), then the
        // feature path Ready -> InProgress -> Done.
        state.tickets[0]
            .set_technical_design(Role::Sa, coxagent_domain::TechnicalDesign::default())
            .expect("design");
        state.tickets[0]
            .transition_to(Role::Sa, Status::Ready)
            .expect("ready");
        state
            .tickets
            .first_mut()
            .expect("ticket")
            .transition_to(Role::DevFeature, Status::InProgress)
            .expect("claim");
        state
            .tickets
            .first_mut()
            .expect("ticket")
            .transition_to(Role::DevFeature, Status::Done)
            .expect("done");

        let report = merge_pending(
            &mut state,
            &[draft(7, "Ship dark mode", "b", &[])],
            IMPORT_CAP,
        );
        assert_eq!(report.imported, 0);
        assert_eq!(report.skipped, 1);
        assert_eq!(state.tickets.len(), 1);
    }

    #[test]
    fn skips_titles_matching_active_or_rejected_tickets() {
        let mut state = ProjectState {
            alias: "CXA".to_owned(),
            ..ProjectState::default()
        };
        let mut t = Ticket::new(
            crate::use_cases::add_ticket::mint_id(TicketType::Feature, &state).expect("mint"),
            TicketType::Feature,
            "Bug triage and burn-down cadence",
            "already tracked by the team",
            Priority::Medium,
            Complexity::Small,
            false,
        )
        .expect("ticket");
        t.stamp_created_at("2026-09-05T00:00:00Z");
        state.tickets.push(t);

        // Same theme (the shared predicate's normalised match), different URL.
        let report = merge_pending(
            &mut state,
            &[draft(
                9,
                "Backlog triage: assess bug risk and burn down",
                "b",
                &[],
            )],
            IMPORT_CAP,
        );
        assert_eq!(
            (report.imported, report.skipped),
            (0, 1),
            "the design's 'skip duplicate titles' uses the shared predicate"
        );
    }

    #[test]
    fn cap_limits_one_run_and_the_rest_is_reported_skipped() {
        // Distinct themes — the shared duplicate predicate would collapse
        // numbered look-alike titles before the cap ever applied.
        let mut state = ProjectState::default();
        let drafts = vec![
            draft(1, "Add dark mode to the dashboard", "b", &[]),
            draft(2, "Migrate the audit log to Postgres", "b", &[]),
            draft(3, "Fix flaky deploy health check", "b", &[]),
        ];

        let report = merge_pending(&mut state, &drafts, 2);
        assert_eq!((report.imported, report.skipped), (2, 1));
        assert_eq!(state.tickets.len(), 2);
    }

    #[test]
    fn bug_label_lands_pending_and_keeps_the_label_signal() {
        let mut state = ProjectState::default();
        let report = merge_pending(
            &mut state,
            &[draft(
                11,
                "Crash on empty payload",
                "POST /api/x 500s",
                &["bug"],
            )],
            IMPORT_CAP,
        );
        assert_eq!(report.imported, 1);
        let t = &state.tickets[0];
        assert_eq!(
            t.status(),
            Status::Pending,
            "a Bug type would start Open and enter the claim queue — imports stay Pending"
        );
        assert!(
            t.description().contains("Labels: bug"),
            "the bug signal survives in the body for triage"
        );
    }

    #[test]
    fn a_shorter_issue_number_is_not_shadowed_by_a_longer_one() {
        // Regression: `.../issues/7` is a substring of `.../issues/71`, so a
        // bare substring dedupe skips #7 the moment #71 is tracked — and the
        // skip is silent. Both must import; order must not matter.
        let mut state = ProjectState::default();
        let first = merge_pending(
            &mut state,
            &[draft(71, "Migrate the audit log to Postgres", "b", &[])],
            IMPORT_CAP,
        );
        assert_eq!(first.imported, 1);
        let second = merge_pending(
            &mut state,
            &[draft(7, "Add dark mode to the dashboard", "b", &[])],
            IMPORT_CAP,
        );
        assert_eq!(
            (second.imported, second.skipped),
            (1, 0),
            "issue #7 must not be swallowed by #71's URL substring"
        );
        assert_eq!(state.tickets.len(), 2);
    }

    #[test]
    fn batch_internal_duplicates_are_skipped_once_each() {
        let mut state = ProjectState::default();
        let report = merge_pending(
            &mut state,
            &[
                draft(7, "Same title", "b", &[]),
                draft(7, "Same title", "b", &[]), // same URL
                draft(8, "Same title", "b", &[]), // same title, new URL
            ],
            IMPORT_CAP,
        );
        assert_eq!(report.imported, 1);
        assert_eq!(report.skipped, 2);
        assert_eq!(state.tickets.len(), 1);
    }

    #[test]
    fn blank_titles_are_skipped_not_fatal() {
        let mut state = ProjectState::default();
        let report = merge_pending(
            &mut state,
            &[draft(7, "   ", "b", &[]), draft(8, "Real title", "b", &[])],
            IMPORT_CAP,
        );
        assert_eq!((report.imported, report.skipped), (1, 1));
    }
}
