// Part of the use_cases split by bounded context — see mod.rs.
//! Atomic release-candidate assembly from verified work (CXA-F231).
//!
//! One pure decision step between "collect raw commit subjects" and
//! "bump + changelog" (the cut itself lives in [`super::cycle::release_cut`]).
//! It answers, purely from persisted state — no IO, no engine, no git:
//!
//! * WHICH tickets may ship: the type-aware Verified-complete set
//!   ([`is_verified_complete`], the same terminal statuses `deps_satisfied`
//!   accepts in `crate::selection`). The transition table makes each of
//!   `Done | Documented | Verified` unreachable for the wrong ticket type,
//!   so one match arms both lifecycles (bugs end at `Verified`; features and
//!   chores at `Done`/`Documented`) — the SA ruling on CXA-F231, which also
//!   rejected DoD-evidence as the predicate (`ticket_evidence` stores
//!   `waived` records, so non-empty ≠ verified).
//! * HOW they group: a bundle is one goal line (CXA-F228 attribution). A
//!   goal ships atomically — every non-`Rejected` ticket of the goal
//!   together, or nothing at all — so a partially-supported feature never
//!   survives assembly. Verified work with no resolvable goal lands in the
//!   explicit `Unattributed` bundle, never silently dropped; those tickets
//!   share no declared scope, so one of them being blocked never strands
//!   another.
//! * WHAT blocks: a Verified-complete ticket whose `depends_on`
//!   prerequisite is itself not Verified-complete (or missing) is reported
//!   in [`RcAssembly::blocked`] with the reason surfaced — it blocks its
//!   whole goal bundle rather than shipping as broken partial scope.
//!
//! The commit-subject half ([`extract_ticket_refs`],
//! [`filter_verified_subjects`]) correlates conventional-commit subjects to
//! ticket ids so a cut's version bump and changelog are computed ONLY over
//! Verified-complete work. Honest-by-default: a subject with no ticket ref
//! is never auto-included, and one unverified reference taints the whole
//! subject — guessing verification is how unverified work sneaks into an RC.

use std::collections::BTreeSet;

use crate::state::ProjectState;
use coxagent_domain::{Status, Ticket, TicketId};

/// One proposed bundle: a named group of Verified-complete tickets that ship
/// together, members in dependency order (prerequisites first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RcBundle {
    pub name: String,
    pub members: Vec<TicketId>,
}

/// A Verified-complete ticket that cannot ship yet, with the reason surfaced
/// (AC3: blocked-with-reason, never silently shrunk scope).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedCandidate {
    pub ticket: TicketId,
    pub reason: String,
}

/// The release-candidate surface computed purely from state. `bundles` lists
/// what may ship; `blocked` lists Verified-complete tickets held back and why.
/// Both empty is AC5's explicit empty state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RcAssembly {
    pub bundles: Vec<RcBundle>,
    pub blocked: Vec<BlockedCandidate>,
}

impl RcAssembly {
    /// True when nothing verified-complete is shippable and nothing is held
    /// back — the empty state renders no candidate.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bundles.is_empty() && self.blocked.is_empty()
    }
}

/// The single type-aware "shipped-complete" predicate. A ticket qualifies by
/// reaching the terminal status of its lifecycle — `Verified` for bugs,
/// `Done | Documented` for features/chores — exactly the statuses
/// `selection::deps_satisfied` accepts as satisfied dependencies. `Rejected`
/// never qualifies (rejected work is dead, not pending).
#[must_use]
pub fn is_verified_complete(t: &Ticket) -> bool {
    matches!(
        t.status(),
        Status::Done | Status::Documented | Status::Verified
    )
}

/// The Verified-complete id set of `state` — the verification gate the cut
/// applies to commit subjects.
#[must_use]
pub fn verified_complete_ids(state: &ProjectState) -> BTreeSet<String> {
    state
        .tickets
        .iter()
        .filter(|t| is_verified_complete(t))
        .map(|t| t.id().as_str().to_owned())
        .collect()
}

/// The flat ship-list: every member of a proposed bundle, bundle by bundle —
/// the surface the acceptance criteria phrase as "assembly lists only
/// Verified-complete tickets grouped into named bundles".
#[must_use]
pub fn rc_members(state: &ProjectState) -> Vec<TicketId> {
    assemble(state)
        .bundles
        .into_iter()
        .flat_map(|b| b.members)
        .collect()
}

/// Assemble the release-candidate surface from state.
///
/// Per goal line: the goal proposes a bundle only when EVERY non-`Rejected`
/// member is Verified-complete AND every member's `depends_on` prerequisite
/// resolves to a Verified-complete ticket (in or out of the bundle).
/// Otherwise the goal proposes nothing — atomic scope — and its
/// prerequisite-blocked members still surface in `blocked` with reasons.
#[must_use]
pub fn assemble(state: &ProjectState) -> RcAssembly {
    let resolves = |dep: &TicketId| state.ticket(dep).is_some_and(is_verified_complete);

    // AC3: every Verified-complete ticket with an unresolved prerequisite is
    // surfaced as blocked-with-reason, whatever goal it belongs to.
    let blocked: Vec<BlockedCandidate> = state
        .tickets
        .iter()
        .filter(|t| is_verified_complete(t) && t.depends_on().iter().any(|d| !resolves(d)))
        .map(|t| BlockedCandidate {
            ticket: t.id().clone(),
            reason: t
                .depends_on()
                .iter()
                .filter(|d| !resolves(d))
                .map(|d| block_reason(state, d))
                .collect::<Vec<_>>()
                .join("; "),
        })
        .collect();
    let blocked_ids: BTreeSet<&str> = blocked.iter().map(|b| b.ticket.as_str()).collect();

    let mut bundles = Vec::new();
    for goal in &state.goals {
        let members: Vec<&Ticket> = state
            .tickets
            .iter()
            .filter(|t| {
                t.status() != Status::Rejected && t.goal_id().is_some_and(|g| *g == goal.id)
            })
            .collect();
        // A goal with no (surviving) work proposes nothing — an empty bundle
        // would fake a candidate out of a declared-but-untouched goal line —
        // and a goal with ANY member short of Verified-complete (or
        // prerequisite-blocked) proposes nothing either: partial scope never
        // ships (AC2, and AC5's empty state stays explicit).
        if members.is_empty() {
            continue;
        }
        let complete = members
            .iter()
            .all(|t| is_verified_complete(t) && !blocked_ids.contains(t.id().as_str()));
        if complete {
            bundles.push(RcBundle {
                name: format!("{}: {}", goal.id, goal.title),
                members: dependency_order(&members),
            });
        }
    }

    // Goal-less Verified-complete work: the explicit Unattributed bundle
    // (F228's "never silently dropped"), minus prerequisite-blocked tickets.
    let unattributed: Vec<&Ticket> = state
        .tickets
        .iter()
        .filter(|t| {
            is_verified_complete(t)
                && !blocked_ids.contains(t.id().as_str())
                && !state
                    .goals
                    .iter()
                    .any(|g| t.goal_id().is_some_and(|gid| *gid == g.id))
        })
        .collect();
    if !unattributed.is_empty() {
        bundles.push(RcBundle {
            name: "Unattributed".to_owned(),
            members: dependency_order(&unattributed),
        });
    }

    RcAssembly { bundles, blocked }
}

/// Why `dep` cannot count as a resolved prerequisite today.
fn block_reason(state: &ProjectState, dep: &TicketId) -> String {
    match state.ticket(dep) {
        Some(t) => format!(
            "prerequisite {dep} is {:?} (not Verified-complete)",
            t.status()
        ),
        None => format!("prerequisite {dep} is not in the project state"),
    }
}

/// Order bundle members prerequisites-first over the `depends_on` edges that
/// stay inside the bundle; ties keep state order. State is external input, so
/// a (domain-illegal) cycle degrades to state order instead of dropping work.
fn dependency_order(members: &[&Ticket]) -> Vec<TicketId> {
    let in_bundle: BTreeSet<&str> = members.iter().map(|t| t.id().as_str()).collect();
    let mut placed: BTreeSet<&str> = BTreeSet::new();
    let mut ordered = Vec::with_capacity(members.len());
    while ordered.len() < members.len() {
        let Some(next) = members.iter().find(|t| {
            !placed.contains(t.id().as_str())
                && t.depends_on()
                    .iter()
                    .filter(|d| in_bundle.contains(d.as_str()))
                    .all(|d| placed.contains(d.as_str()))
        }) else {
            // Cycle: no member is dependency-free — append the rest as-is.
            ordered.extend(
                members
                    .iter()
                    .filter(|t| !placed.contains(t.id().as_str()))
                    .map(|t| t.id().clone()),
            );
            break;
        };
        placed.insert(next.id().as_str());
        ordered.push(next.id().clone());
    }
    ordered
}

/// Every ticket id a conventional-commit subject references — `CXA-F228`,
/// `#CXA-B084` and `cxa-c001` all match (case-normalised) — or `None` when
/// the subject references no ticket at all. Delimiter-bounded, so
/// `feat(cxa):` scopes, `v2.27.0` markers and bare `#401` numbers yield
/// nothing.
#[must_use]
pub fn extract_ticket_refs(subject: &str) -> Option<Vec<String>> {
    let bytes = subject.as_bytes();
    let mut refs = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        // A ref starts at an alphabetic character on a token boundary (not
        // glued to preceding alphanumerics), then `-`, up to three letters,
        // and at least one digit: CXA-F228, cxa-b084, CX-42.
        let on_boundary = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        if bytes[i].is_ascii_alphabetic() && on_boundary {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            let mut j = i;
            if j < bytes.len() && bytes[j] == b'-' {
                j += 1;
                let letters = j;
                while j < bytes.len() && bytes[j].is_ascii_alphabetic() && j - letters < 3 {
                    j += 1;
                }
                let digits = j;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                let delimited = j >= bytes.len() || !bytes[j].is_ascii_alphanumeric();
                if j > digits && delimited {
                    refs.push(subject[start..j].to_ascii_uppercase());
                }
            }
        } else {
            i += 1;
        }
    }
    (!refs.is_empty()).then_some(refs)
}

/// The commit-subject side of the manifest: which raw subjects may drive the
/// cut's bump + changelog, correlated against the Verified-complete id set.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubjectManifest {
    /// Subjects fully backed by Verified-complete tickets.
    pub included: Vec<String>,
    /// Ticket ids carried by included subjects (first-occurrence order).
    pub included_ids: Vec<String>,
    /// Subjects dropped: no ticket ref, or any ref not Verified-complete.
    pub excluded: Vec<String>,
    /// The unverified/unknown ids that caused exclusions.
    pub excluded_ids: Vec<String>,
}

/// Correlate raw commit subjects with the Verified-complete set. A subject
/// joins the manifest only when it references tickets and EVERY reference is
/// Verified-complete — honest-by-default: a ref-less subject is never
/// auto-included, and one unverified id taints the whole subject.
#[must_use]
pub fn filter_verified_subjects(
    subjects: &[String],
    verified: &BTreeSet<String>,
) -> SubjectManifest {
    let mut m = SubjectManifest::default();
    let mut seen_included: BTreeSet<String> = BTreeSet::new();
    let mut seen_excluded: BTreeSet<String> = BTreeSet::new();
    for s in subjects {
        match extract_ticket_refs(s) {
            None => m.excluded.push(s.clone()),
            Some(ids) if ids.iter().all(|id| verified.contains(id)) => {
                m.included.push(s.clone());
                push_unique(&mut m.included_ids, &mut seen_included, ids);
            }
            Some(ids) => {
                m.excluded.push(s.clone());
                let unverified: Vec<String> = ids
                    .into_iter()
                    .filter(|id| !verified.contains(id))
                    .collect();
                push_unique(&mut m.excluded_ids, &mut seen_excluded, unverified);
            }
        }
    }
    m
}

fn push_unique(
    into: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    ids: impl IntoIterator<Item = String>,
) {
    for id in ids {
        if seen.insert(id.clone()) {
            into.push(id);
        }
    }
}

/// The cut-facing manifest: the subjects whose ticket references are all
/// Verified-complete (what may drive the cut's bump + changelog), plus the SM
/// note that surfaces the proposed bundles and the included/excluded ticket
/// ids. No bundle or id, no entry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CutManifest {
    pub included: Vec<String>,
    pub note: String,
}

/// Build [`CutManifest`] for a cut over `state` + raw commit subjects.
#[must_use]
pub fn cut_manifest(state: &ProjectState, subjects: &[String]) -> CutManifest {
    let manifest = filter_verified_subjects(subjects, &verified_complete_ids(state));
    let mut parts: Vec<String> = Vec::new();
    let bundles = assemble(state)
        .bundles
        .iter()
        .map(|b| {
            let ids = b
                .members
                .iter()
                .map(TicketId::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            format!("{} [{}]", b.name, ids)
        })
        .collect::<Vec<_>>()
        .join("; ");
    if !bundles.is_empty() {
        parts.push(format!("bundles: {bundles}"));
    }
    if !manifest.included_ids.is_empty() {
        parts.push(format!("included: {}", manifest.included_ids.join(", ")));
    }
    if !manifest.excluded_ids.is_empty() {
        parts.push(format!("excluded: {}", manifest.excluded_ids.join(", ")));
    }
    let note = if parts.is_empty() {
        String::new()
    } else {
        format!(" RC: {}", parts.join("; "))
    };
    CutManifest {
        included: manifest.included,
        note,
    }
}
