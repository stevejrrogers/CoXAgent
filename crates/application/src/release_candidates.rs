//! Release-candidate assembly (CXA-F231) — the pure read that groups
//! Verified-complete work into named, shippable bundles, and the pure filter
//! that decides which commit subjects may enter a release cut.
//!
//! Same discipline as `selection.rs`: every answer is computed
//! deterministically from persisted state (and subject strings) alone, so it
//! is testable without any engine, server, harness or port.
//!
//! "Verified-complete" is the type-aware closed set the SA confirmed
//! (matching `deps_satisfied` in `selection.rs` and the terminal statuses in
//! `transitions.rs`): bugs finish at `Verified`, features/chores at
//! `Done`/`Documented`. `Rejected` never qualifies — it is scope a PO
//! removed, not work awaiting verification.

use crate::state::ProjectState;
use coxagent_domain::{Status, Ticket, TicketId, TicketType};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Why a proposed bundle cannot ship yet — an explicit, surfaced reason
/// (CXA-F231), never a silent broken partial scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedReason {
    /// The unverified prerequisite ticket the bundle depends on.
    pub prerequisite: TicketId,
    /// The reason the prerequisite blocks this bundle.
    pub reason: String,
}

/// One proposed release candidate: a NAMED BUNDLE of Verified-complete
/// tickets that ship together, with any block surfaced on the bundle itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseCandidate {
    /// The bundle's name, derived from the state it groups.
    pub name: String,
    /// The Verified-complete tickets in the bundle.
    pub tickets: Vec<TicketId>,
    /// Set when the bundle cannot ship as-is: an unverified prerequisite
    /// blocks it with an explicit reason. `None` = shippable as proposed.
    pub blocked: Option<BlockedReason>,
}

/// A commit subject kept OUT of a release candidate, with the explicit
/// reason (CXA-F231: unverified work never enters an RC silently).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExcludedSubject {
    pub subject: String,
    /// Ticket refs found on the subject (empty when it named none).
    pub refs: Vec<String>,
    pub reason: String,
}

/// The pure manifest decision for one release-cut range: which subjects ship
/// and which are kept out, with reasons. Empty `included` = nothing
/// releasable this range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ManifestOutcome {
    /// Subjects destined for the RC, in range order.
    pub included: Vec<String>,
    /// Subjects kept out, each with an explicit surfaced reason.
    pub excluded: Vec<ExcludedSubject>,
}

/// The type-aware terminal set (CXA-F231, SA-confirmed): a bug is complete at
/// `Verified`; a feature/chore at `Done`/`Documented`. `Rejected` is removed
/// scope, never shippable.
#[must_use]
pub fn is_verified_complete(ticket: &Ticket) -> bool {
    match ticket.ticket_type() {
        TicketType::Bug => ticket.status() == Status::Verified,
        TicketType::Feature | TicketType::Chore => {
            matches!(ticket.status(), Status::Done | Status::Documented)
        }
    }
}

/// The RC-eligible ticket ids in state — every Verified-complete ticket.
#[must_use]
pub fn verified_complete_ids(state: &ProjectState) -> BTreeSet<String> {
    state
        .tickets
        .iter()
        .filter(|t| is_verified_complete(t))
        .map(|t| t.id().to_string())
        .collect()
}

/// Assemble the release candidates from persisted state: only
/// Verified-complete tickets, grouped into named bundles whose consistency is
/// re-evaluated from current state on every call. Pure — a function of
/// `state` alone; no server, harness or port involved.
///
/// Grouping follows the goal lines: a goal whose declared tickets are ALL
/// Verified-complete proposes one bundle named after the goal (a
/// partially-supported goal line proposes nothing — no half-applied feature
/// survives). Verified tickets with no resolvable goal form one explicit
/// "Unattributed" bundle instead of vanishing (the CXA-F228 rule: verified
/// work is never silently dropped). Cross-ticket `depends_on` edges to
/// anything outside the verified set block the bundle with a surfaced reason.
#[must_use]
pub fn release_candidates(state: &ProjectState) -> Vec<ReleaseCandidate> {
    let mut board = Vec::new();
    for goal in &state.goals {
        let declared: Vec<&Ticket> = state
            .tickets
            .iter()
            .filter(|t| t.goal_id() == Some(&goal.id))
            .collect();
        // A rejected ticket is scope the PO removed: it neither joins the
        // bundle nor keeps its goal line permanently incomplete.
        let shippable: Vec<&Ticket> = declared
            .into_iter()
            .filter(|t| t.status() != Status::Rejected)
            .collect();
        if shippable.is_empty() || !shippable.iter().all(|t| is_verified_complete(t)) {
            continue; // partially supported goal line — proposes nothing
        }
        let tickets = shippable.iter().map(|t| t.id().clone()).collect();
        board.push(ReleaseCandidate {
            name: goal.title.clone(),
            tickets,
            blocked: first_unverified_dependency(state, &ticket_ids(&shippable)),
        });
    }
    // Goal-less (or dangling-goal) verified work: its own bundle, never a
    // silent drop (CXA-F228's unattributed rule carried onto the RC surface).
    let unattributed: Vec<TicketId> = state
        .tickets
        .iter()
        .filter(|t| is_verified_complete(t) && !goal_resolves(t.goal_id(), state))
        .map(|t| t.id().clone())
        .collect();
    if !unattributed.is_empty() {
        let blocked = first_unverified_dependency(state, &unattributed);
        board.push(ReleaseCandidate {
            name: "Unattributed".to_owned(),
            tickets: unattributed,
            blocked,
        });
    }
    board
}

/// Extract ticket refs (`CXA-F228`, `cxa-b004`, …) embedded in a commit
/// subject. `None` when the subject names no ticket — verification then
/// cannot be established from the commit alone.
///
/// A ref is a word of 2+ letters, a dash, then 1+ alphanumerics that carry a
/// digit or are 2+ uppercase letters (`CXA-F228`, `CXA-NEW` — but not
/// `well-known` or `size-capped`). Refs are uppercased to the project-alias
/// convention so `cxa-f228` in a subject matches `CXA-F228` in state.
#[must_use]
pub fn extract_ticket_refs(subject: &str) -> Option<Vec<String>> {
    let bytes = subject.as_bytes();
    let mut refs = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let boundary = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        if boundary && bytes[i].is_ascii_alphabetic() {
            let mut j = i;
            while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
                j += 1;
            }
            let prefix_len = j - i;
            if prefix_len >= 2 && j < bytes.len() && bytes[j] == b'-' {
                let mut k = j + 1;
                while k < bytes.len() && bytes[k].is_ascii_alphanumeric() {
                    k += 1;
                }
                let suffix = &subject[j + 1..k];
                let id_like = suffix.bytes().any(|b| b.is_ascii_digit())
                    || (suffix.len() >= 2 && suffix.bytes().all(|b| b.is_ascii_uppercase()));
                if !suffix.is_empty() && id_like {
                    refs.push(subject[i..k].to_ascii_uppercase());
                    i = k;
                    continue;
                }
            }
        }
        i += 1;
    }
    (!refs.is_empty()).then_some(refs)
}

/// The manifest decision for a cut range: subjects correlate to tickets via
/// [`extract_ticket_refs`]; a subject ships only when EVERY ref it names is
/// in the verified set. Merge markers and `release:` subjects prove nothing
/// releasable (the same rule `classify_bump` applies) and are skipped
/// entirely. A subject with no ref is NOT auto-included — honest-by-default
/// exclusion rather than guessing verification.
#[must_use]
pub fn filter_included(subjects: &[String], verified: &BTreeSet<String>) -> ManifestOutcome {
    let mut outcome = ManifestOutcome::default();
    for subject in subjects {
        let s = subject.trim();
        if s.is_empty() || s.starts_with("Merge ") || s.starts_with("release:") {
            continue;
        }
        let Some(refs) = extract_ticket_refs(s) else {
            outcome.excluded.push(ExcludedSubject {
                subject: s.to_owned(),
                refs: Vec::new(),
                reason: "no ticket reference — verification cannot be established".to_owned(),
            });
            continue;
        };
        let unverified: Vec<String> = refs
            .iter()
            .filter(|r| !verified.contains(r.as_str()))
            .cloned()
            .collect();
        if unverified.is_empty() {
            outcome.included.push(s.to_owned());
        } else {
            outcome.excluded.push(ExcludedSubject {
                subject: s.to_owned(),
                refs,
                reason: format!("{} not Verified-complete", unverified.join(", ")),
            });
        }
    }
    outcome
}

/// The one-line SM activity summary of a manifest: the ticket ids that ship
/// and the ids kept out (CXA-F231 audit line).
#[must_use]
pub fn manifest_summary(outcome: &ManifestOutcome) -> String {
    let mut included_ids: Vec<String> = Vec::new();
    for subject in &outcome.included {
        if let Some(refs) = extract_ticket_refs(subject) {
            for r in refs {
                if !included_ids.contains(&r) {
                    included_ids.push(r);
                }
            }
        }
    }
    let mut line = if included_ids.is_empty() {
        "RC manifest: in —".to_owned()
    } else {
        format!("RC manifest: in {}", included_ids.join(", "))
    };
    if !outcome.excluded.is_empty() {
        let out: Vec<String> = outcome
            .excluded
            .iter()
            .map(|e| {
                let tag = if e.refs.is_empty() {
                    "no ref"
                } else {
                    "unverified"
                };
                let id = e.refs.first().map_or("—", String::as_str);
                format!("{id} ({tag})")
            })
            .collect();
        line.push_str(" · out ");
        line.push_str(&out.join(", "));
    }
    line
}

fn ticket_ids(tickets: &[&Ticket]) -> Vec<TicketId> {
    tickets.iter().map(|t| t.id().clone()).collect()
}

fn goal_resolves(goal_id: Option<&coxagent_domain::GoalId>, state: &ProjectState) -> bool {
    goal_id.is_some_and(|g| state.goals.iter().any(|goal| &goal.id == g))
}

/// The first `depends_on` edge from any bundle member that leaves the
/// verified set (or names a ticket missing from state) — the surfaced block.
fn first_unverified_dependency(
    state: &ProjectState,
    members: &[TicketId],
) -> Option<BlockedReason> {
    for id in members {
        let Some(t) = state.ticket(id) else { continue };
        for dep in t.depends_on() {
            let Some(d) = state.ticket(dep) else {
                return Some(BlockedReason {
                    prerequisite: dep.clone(),
                    reason: format!(
                        "{dep} is not in project state — it must exist and be Verified-complete \
                         before this bundle ships"
                    ),
                });
            };
            if !is_verified_complete(d) {
                return Some(BlockedReason {
                    prerequisite: dep.clone(),
                    reason: format!(
                        "{dep} is not Verified-complete (status: {:?}) — it must ship first",
                        d.status()
                    ),
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_ticket_refs_parses_conventional_subjects_and_rejects_non_refs() {
        assert_eq!(
            extract_ticket_refs("feat(cxa): wire REST store #CXA-F228"),
            Some(vec!["CXA-F228".to_owned()])
        );
        assert_eq!(
            extract_ticket_refs("fix(cxa-f228): lowercase refs match too"),
            Some(vec!["CXA-F228".to_owned()])
        );
        assert_eq!(
            extract_ticket_refs("feat(CXA-F176): CXA-NEW batching (#395)"),
            Some(vec!["CXA-F176".to_owned(), "CXA-NEW".to_owned()])
        );
        // Prose hyphens are not ticket refs.
        assert_eq!(extract_ticket_refs("Merge branch 'x'"), None);
        assert_eq!(extract_ticket_refs("release: v2.27.0"), None);
        assert_eq!(
            extract_ticket_refs("feat(app): size-capped and stale-trimmed worktree targets"),
            None
        );
    }

    #[test]
    fn filter_included_admits_only_ref_bearing_verified_subjects() {
        let subjects: Vec<String> = [
            "feat(CXA-F228): outcome ledger",
            "fix(CXA-F009): scanner",
            "feat(app): disk discipline",
            "Merge pull request #394",
            "release: v2.26.0",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        let verified: BTreeSet<String> = ["CXA-F228".to_owned()].into();

        let outcome = filter_included(&subjects, &verified);

        assert_eq!(
            outcome.included,
            vec!["feat(CXA-F228): outcome ledger".to_owned()]
        );
        assert_eq!(outcome.excluded.len(), 2);
        assert_eq!(outcome.excluded[0].refs, vec!["CXA-F009".to_owned()]);
        assert!(outcome.excluded[0].reason.contains("CXA-F009"));
        assert!(outcome.excluded[1].refs.is_empty());
        assert!(!outcome.excluded[1].reason.is_empty());
    }

    #[test]
    fn a_subject_with_no_ticket_ref_is_not_auto_included() {
        let subjects: Vec<String> = ["feat(app): disk discipline".to_owned()].into();
        let verified: BTreeSet<String> = BTreeSet::new();

        let outcome = filter_included(&subjects, &verified);

        assert!(outcome.included.is_empty(), "{outcome:?}");
        assert_eq!(outcome.excluded.len(), 1);
    }

    #[test]
    fn summary_lists_included_and_excluded_ticket_ids() {
        let outcome = filter_included(
            &[
                "feat(CXA-F228): outcome ledger".to_owned(),
                "fix(CXA-F009): scanner".to_owned(),
                "feat(app): disk discipline".to_owned(),
            ],
            &["CXA-F228".to_owned()].into(),
        );
        let line = manifest_summary(&outcome);
        assert!(line.contains("in CXA-F228"), "{line}");
        assert!(line.contains("out CXA-F009 (unverified)"), "{line}");
        assert!(line.contains("— (no ref)"), "{line}");
    }
}
