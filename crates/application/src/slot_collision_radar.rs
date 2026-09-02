//! Slot collision radar (CXA-F329) — the pure derivation that answers, at
//! claim time, whether two tickets running in parallel slots will touch the
//! same files.
//!
//! Same discipline as `dependency_radar` (CXA-F237): every answer is computed
//! deterministically from `ProjectState` alone — no engine, no server, no
//! port, no IO — so it is testable with struct-literal fixtures and safe to
//! run on every state serialization (the 1 Hz snapshot includes it).
//!
//! The "will touch" signal is the SA-declared `TechnicalDesign.files` on each
//! InProgress ticket: the collision predicate is EXACT normalized-path overlap
//! (trim / drop-empty / dedupe). Renamed or moved files are invisible to an
//! exact match by construction — a prediction beyond declared paths would need
//! repo IO here, and this module must stay pure; the advisory framing exists
//! precisely because the prediction is a floor, not a guarantee.
//!
//! Advisory-only, never blocking: the claim has already succeeded by the time
//! any of this renders. A ticket whose design declares no files cannot be
//! checked at all — it surfaces as `unknown_files` (radar-blind), mirroring
//! `unknown_dependencies`: never silently treated as safe. Cross-project
//! comparisons cannot happen: the derivation sees exactly the one
//! `ProjectState` it is handed.

use crate::state::ProjectState;
use coxagent_domain::{Status, Ticket, TicketId};
use serde::Serialize;
use std::collections::BTreeSet;

/// One collision as the radar reports it: two InProgress tickets (`a` < `b`,
/// lexicographically) and the declared files both would touch, sorted and
/// deduplicated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CollisionPair {
    pub a: TicketId,
    pub b: TicketId,
    pub files: Vec<String>,
}

/// The derived collision summary served beside the state snapshot. Fields the
/// project has nothing to report are omitted at serialization, so clients
/// treat absence as an empty radar rather than an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct CollisionRadar {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pairs: Vec<CollisionPair>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unknown_files: Vec<TicketId>,
}

/// The declared touch surface of a ticket: its SA design's `files`, each
/// trimmed, empties dropped, deduplicated and sorted — so a path repeated or
/// padded in a hand-edited state is one path, and the report is deterministic.
fn declared_files(t: &Ticket) -> BTreeSet<String> {
    t.design()
        .technical
        .as_ref()
        .map(|d| {
            d.files
                .iter()
                .filter_map(|f| {
                    let f = f.trim();
                    (!f.is_empty()).then(|| f.to_owned())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The InProgress tickets whose overlap with `candidate` the radar can prove:
/// every OTHER running ticket sharing at least one declared file, in state
/// order, shared files sorted. A candidate that is not running, unknown to
/// the state, or radar-blind (no declared files) has no provable collisions.
#[must_use]
pub fn collisions_for(state: &ProjectState, candidate: &TicketId) -> Vec<CollisionPair> {
    let Some(c) = state.ticket(candidate) else {
        return Vec::new();
    };
    if c.status() != Status::InProgress {
        return Vec::new();
    }
    let cand_files = declared_files(c);
    if cand_files.is_empty() {
        return Vec::new();
    }
    state
        .tickets
        .iter()
        .filter(|t| t.status() == Status::InProgress && t.id() != candidate)
        .filter_map(|t| {
            let shared: Vec<String> = declared_files(t)
                .intersection(&cand_files)
                .cloned()
                .collect();
            (!shared.is_empty()).then(|| CollisionPair {
                a: candidate.clone(),
                b: t.id().clone(),
                files: shared,
            })
        })
        .collect()
}

/// The whole-state symmetric view the board renders: every InProgress×
/// InProgress pair with an exact declared-file overlap (ids lexicographically
/// sorted within and across pairs, files sorted+deduped), plus the
/// radar-blind running tickets — designs with no file hints. Same state in,
/// same report out, every snapshot.
#[must_use]
pub fn collision_radar(state: &ProjectState) -> CollisionRadar {
    let running: Vec<&Ticket> = state
        .tickets
        .iter()
        .filter(|t| t.status() == Status::InProgress)
        .collect();
    let mut pairs = Vec::new();
    for (i, a) in running.iter().enumerate() {
        for b in &running[i + 1..] {
            let shared: Vec<String> = declared_files(a)
                .intersection(&declared_files(b))
                .cloned()
                .collect();
            if shared.is_empty() {
                continue;
            }
            let (a, b) = if a.id().as_str() <= b.id().as_str() {
                (a, b)
            } else {
                (b, a)
            };
            pairs.push(CollisionPair {
                a: a.id().clone(),
                b: b.id().clone(),
                files: shared,
            });
        }
    }
    pairs.sort_by(|x, y| (x.a.as_str(), x.b.as_str()).cmp(&(y.a.as_str(), y.b.as_str())));
    pairs.dedup();
    let mut unknown_files: Vec<TicketId> = running
        .iter()
        .filter(|t| declared_files(t).is_empty())
        .map(|t| t.id().clone())
        .collect();
    unknown_files.sort();
    CollisionRadar {
        pairs,
        unknown_files,
    }
}

/// The claim-time advisory for the claiming agent's brief: the WARNING block
/// naming each colliding partner and the shared files, or — when the
/// candidate itself declares no files — the low-confidence radar-blind note.
/// Empty string when clean. Never an error, never a refusal: the claim has
/// already succeeded; this only tells the agent what the board already shows.
#[must_use]
pub fn claim_warning(state: &ProjectState, candidate: &TicketId) -> String {
    let collisions = collisions_for(state, candidate);
    if collisions.is_empty() {
        let blind = state
            .ticket(candidate)
            .is_some_and(|t| t.status() == Status::InProgress && declared_files(t).is_empty());
        if blind {
            "\nNOTE (low confidence): this ticket declares no files in its design, so the \
             slot collision radar cannot check it against the other running slot — check \
             the board's radar-blind tickets before touching shared modules.\n"
                .to_owned()
        } else {
            String::new()
        }
    } else {
        use std::fmt::Write as _;
        let mut w = String::new();
        for c in &collisions {
            let _ = write!(
                w,
                "\nWARNING: slot collision — {} (running in another slot) declares the same \
                 files: {}. Touch them only where your ticket requires, expect the other \
                 slot's edits there, and coordinate to avoid a conflicted merge.",
                c.b,
                c.files.join(", ")
            );
        }
        w.push('\n');
        w
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coxagent_domain::{Complexity, Priority, Role, TechnicalDesign, TicketType};

    fn tid(s: &str) -> TicketId {
        TicketId::new(s).expect("valid ticket id")
    }

    /// A feature walked to `status` along the only legal edges, declaring
    /// `files` in the SA's technical design (same fixture shape the TDD
    /// suite pins).
    fn feature_at(id: &str, status: Status, files: &[&str]) -> Ticket {
        let mut t = Ticket::new(
            tid(id),
            TicketType::Feature,
            format!("feature {id}"),
            "fixture",
            Priority::Medium,
            Complexity::Medium,
            false,
        )
        .expect("ticket");
        t.set_technical_design(
            Role::Sa,
            TechnicalDesign {
                files: files.iter().map(|f| (*f).to_owned()).collect(),
                ..TechnicalDesign::default()
            },
        )
        .expect("attach design");
        if status != Status::Pending {
            t.transition_to(Role::Sa, Status::Ready).expect("ready");
            if status != Status::Ready {
                for step in [Status::InProgress, Status::Done] {
                    t.transition_to(Role::DevFeature, step).expect("walk");
                    if step == status {
                        break;
                    }
                }
            }
        }
        t
    }

    fn state_with(tickets: Vec<Ticket>) -> ProjectState {
        ProjectState {
            tickets,
            ..ProjectState::default()
        }
    }

    // --- the SA test plan: overlap detection ---

    #[test]
    fn an_exact_path_overlap_between_running_tickets_is_detected() {
        let state = state_with(vec![
            feature_at("FEAT-A", Status::InProgress, &["crates/app/src/main.rs"]),
            feature_at("FEAT-B", Status::InProgress, &["crates/app/src/main.rs"]),
        ]);
        assert_eq!(
            collision_radar(&state).pairs,
            vec![CollisionPair {
                a: tid("FEAT-A"),
                b: tid("FEAT-B"),
                files: vec!["crates/app/src/main.rs".to_owned()],
            }],
            "the pair is reported with the shared file, ids in state order"
        );
    }

    #[test]
    fn disjoint_files_never_flag() {
        let state = state_with(vec![
            feature_at("FEAT-A", Status::InProgress, &["crates/app/src/main.rs"]),
            feature_at("FEAT-B", Status::InProgress, &["web/app.css"]),
        ]);
        let radar = collision_radar(&state);
        assert!(radar.pairs.is_empty(), "disjoint work stays silent");
        assert!(radar.unknown_files.is_empty());
        assert_eq!(claim_warning(&state, &tid("FEAT-A")), "");
    }

    #[test]
    fn the_candidate_is_excluded_from_its_own_pair_set() {
        let state = state_with(vec![feature_at(
            "FEAT-A",
            Status::InProgress,
            &["crates/app/src/main.rs"],
        )]);
        assert!(
            collisions_for(&state, &tid("FEAT-A")).is_empty(),
            "one running ticket is nobody's collision — there is no other slot"
        );
    }

    #[test]
    fn only_inprogress_tickets_are_paired() {
        let files = ["crates/app/src/main.rs"];
        let state = state_with(vec![
            feature_at("FEAT-P", Status::Pending, &files),
            feature_at("FEAT-R", Status::Ready, &files),
            feature_at("FEAT-D", Status::Done, &files),
            feature_at("FEAT-A", Status::InProgress, &files),
        ]);
        let radar = collision_radar(&state);
        assert!(
            radar.pairs.is_empty(),
            "Pending/Ready/Done tickets are not running in a slot — no pair"
        );
        // The moment a second ticket starts running, only the running pair fires.
        let live = state_with(vec![
            feature_at("FEAT-R", Status::Ready, &files),
            feature_at("FEAT-A", Status::InProgress, &files),
            feature_at("FEAT-B", Status::InProgress, &files),
        ]);
        let pairs = collision_radar(&live).pairs;
        assert_eq!(pairs.len(), 1, "exactly the InProgress×InProgress pair");
        assert_eq!(pairs[0].a, tid("FEAT-A"));
        assert_eq!(pairs[0].b, tid("FEAT-B"));
    }

    // --- the SA test plan: radar-blind handling ---

    #[test]
    fn empty_or_absent_declared_files_land_in_unknown_files() {
        let state = state_with(vec![
            feature_at("FEAT-A", Status::InProgress, &[]),
            feature_at("FEAT-B", Status::InProgress, &["   "]),
        ]);
        let radar = collision_radar(&state);
        assert!(radar.pairs.is_empty(), "nothing declared, nothing provable");
        assert_eq!(
            radar.unknown_files,
            vec![tid("FEAT-A"), tid("FEAT-B")],
            "radar-blind running tickets are surfaced, never silently OK"
        );
    }

    #[test]
    fn a_blind_candidate_gets_the_low_confidence_note_not_silence() {
        let state = state_with(vec![
            feature_at("FEAT-A", Status::InProgress, &[]),
            feature_at("FEAT-B", Status::InProgress, &["web/app.css"]),
        ]);
        let w = claim_warning(&state, &tid("FEAT-A"));
        assert!(
            w.contains("low confidence") && w.contains("declares no files"),
            "AC3: the blind candidate is visibly marked low-confidence: {w}"
        );
        assert_eq!(
            claim_warning(&state, &tid("FEAT-B")),
            "",
            "a candidate with declared files and no overlap is clean — no noise"
        );
    }

    // --- the SA test plan: normalization ---

    #[test]
    fn repeated_and_padded_declared_paths_dedupe() {
        let a = feature_at("FEAT-A", Status::InProgress, &[" web/app.css "]);
        let b = feature_at("FEAT-B", Status::InProgress, &["web/app.css"]);
        // A restored or hand-edited state can carry repeated entries; they
        // arrive through the persisted JSON shape.
        let mut raw = serde_json::to_value(&b).expect("ticket serializes");
        raw["design"]["technical"]["files"] =
            serde_json::json!(["web/app.css", "web/app.css", " web/app.css"]);
        let b: Ticket = serde_json::from_value(raw).expect("restored ticket deserializes");
        let state = state_with(vec![a, b]);
        let pairs = collision_radar(&state).pairs;
        assert_eq!(pairs.len(), 1);
        assert_eq!(
            pairs[0].files,
            vec!["web/app.css".to_owned()],
            "one shared path, trimmed and deduplicated"
        );
    }

    // --- the SA test plan: determinism ---

    #[test]
    fn the_same_state_always_yields_identically_sorted_pairs_and_files() {
        let state = state_with(vec![
            feature_at(
                "FEAT-B",
                Status::InProgress,
                &["web/app.css", "crates/app/src/main.rs"],
            ),
            feature_at("FEAT-C", Status::InProgress, &["web/app.css"]),
            feature_at(
                "FEAT-A",
                Status::InProgress,
                &["crates/app/src/main.rs", "web/app.css"],
            ),
        ]);
        let first = collision_radar(&state);
        let again = collision_radar(&state);
        assert_eq!(first, again, "same state, same report — every snapshot");
        // Ids lexicographically sorted within and across pairs.
        for p in &first.pairs {
            assert!(p.a.as_str() < p.b.as_str(), "a < b within the pair");
        }
        let across: Vec<(&str, &str)> = first
            .pairs
            .iter()
            .map(|p| (p.a.as_str(), p.b.as_str()))
            .collect();
        let mut sorted = across.clone();
        sorted.sort_unstable();
        assert_eq!(across, sorted, "pairs sorted by (a, b)");
        for p in &first.pairs {
            let mut f = p.files.clone();
            f.sort();
            f.dedup();
            assert_eq!(p.files, f, "files sorted+deduped");
        }
    }

    // --- the SA test plan: the warning shapes ---

    #[test]
    fn the_warning_names_the_partner_and_the_files() {
        let state = state_with(vec![
            feature_at("FEAT-A", Status::InProgress, &["crates/app/src/main.rs"]),
            feature_at("FEAT-B", Status::InProgress, &["crates/app/src/main.rs"]),
        ]);
        let w = claim_warning(&state, &tid("FEAT-A"));
        assert!(
            w.contains("WARNING: slot collision")
                && w.contains("FEAT-B")
                && w.contains("running in another slot")
                && w.contains("crates/app/src/main.rs"),
            "the exact WARNING shape: partner id + shared files: {w}"
        );
    }

    #[test]
    fn an_unknown_or_finished_candidate_is_clean() {
        let state = state_with(vec![feature_at(
            "FEAT-B",
            Status::InProgress,
            &["web/app.css"],
        )]);
        assert_eq!(claim_warning(&state, &tid("FEAT-ZZZ")), "");
        assert_eq!(
            claim_warning(&state_with(Vec::new()), &tid("FEAT-B")),
            "",
            "a ticket the state does not know has nothing to collide with"
        );
        let done = state_with(vec![
            feature_at("FEAT-A", Status::Done, &["web/app.css"]),
            feature_at("FEAT-B", Status::InProgress, &["web/app.css"]),
        ]);
        assert_eq!(
            claim_warning(&done, &tid("FEAT-A")),
            "",
            "a ticket that is not running takes no claim-time advisory"
        );
    }

    #[test]
    fn empty_state_yields_an_empty_radar() {
        let radar = collision_radar(&ProjectState::default());
        assert!(radar.pairs.is_empty());
        assert!(radar.unknown_files.is_empty());
    }
}
