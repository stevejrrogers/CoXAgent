//! Dependency-aware blocked-ticket radar (CXA-F237) — the pure derivation that
//! answers WHY a Ready ticket is not running: which unfinished upstream work
//! blocks it (transitively), which `depends_on` ids name no ticket at all, and
//! which chain of unfinished work is the critical path to the next release.
//!
//! Same discipline as `selection.rs` and `release_candidates.rs`: every answer
//! is computed deterministically from `ProjectState` alone — no engine, no
//! server, no port, no IO — so it is testable with struct-literal fixtures and
//! safe to run on every state serialization (the 1 Hz snapshot includes it).
//!
//! "Resolved" is the exact predicate the DEV gate applies
//! (`selection::deps_satisfied`): `Done | Documented | Verified`, regardless of
//! ticket type. The radar and the gate are two views of one fact — if they ever
//! disagreed, the backlog would badge work DEV can actually take, or DEV would
//! starve behind work the backlog calls unblocked. `Unknown` dependencies are
//! never resolved: an id absent from the project state is surfaced, never
//! silently treated as satisfied.
//!
//! The derivation is total over states that could never be persisted:
//! `ProjectState::validate` refuses cycles and dangling deps at every write,
//! but the radar still observes them (restore/preview paths) and must terminate
//! on them — every walk here is visited-set guarded, and cycle members are
//! flagged, never hung on.

use crate::state::ProjectState;
use coxagent_domain::{Priority, Status, TicketId};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};

/// One blocker as the ticket-detail surface reports it: the blocking ticket
/// and its LIVE status at the moment of the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Blocker {
    pub ticket: TicketId,
    pub status: Status,
}

/// A Ready ticket the radar reports as blocked, with the full blocking chain
/// (prerequisites-first) that the backlog BLOCKED badge renders. Ids the
/// project state does not know are appended verbatim: the badge must fire for
/// them too, because the DEV gate refuses the ticket all the same.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockedSummary {
    pub id: TicketId,
    pub blockers: Vec<TicketId>,
}

/// The derived radar summary served beside the state snapshot. Fields the
/// project has nothing to report are omitted at serialization, so clients
/// treat absence as an empty radar rather than an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct Radar {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blocked: Vec<BlockedSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub critical_path: Vec<TicketId>,
}

/// One node of the dependency graph: a ticket that EXISTS in project state,
/// with its live status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphNode {
    pub id: TicketId,
    pub status: Status,
}

/// One directed edge of the dependency graph: `dependent` must wait for
/// `prerequisite`. Every edge corresponds to an actual `depends_on` entry —
/// the derivation fabricates nothing (no transitive shortcuts, no goal or
/// parent edges, and no phantom node for an id the state does not have).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphEdge {
    pub dependent: TicketId,
    pub prerequisite: TicketId,
}

/// A dependency is resolved when it reached the same shipped-complete set the
/// DEV gate accepts (`selection::deps_satisfied`).
fn resolved(status: Status) -> bool {
    matches!(status, Status::Done | Status::Documented | Status::Verified)
}

/// The direct blockers of `ticket`: every `depends_on` entry whose target
/// exists in state and has not reached shipped-complete, each with its live
/// status, in declaration order. Absent ids are NOT blockers here — they
/// surface through [`unknown_dependencies`].
#[must_use]
pub fn blocked_by(state: &ProjectState, ticket: &TicketId) -> Vec<Blocker> {
    let Some(t) = state.ticket(ticket) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    t.depends_on()
        .iter()
        .filter_map(|dep| state.ticket(dep).map(|d| (dep, d)))
        .filter(|(_, d)| !resolved(d.status()))
        .filter(|(dep, _)| seen.insert((*dep).clone()))
        .map(|(dep, d)| Blocker {
            ticket: dep.clone(),
            status: d.status(),
        })
        .collect()
}

/// The FULL blocking chain of `ticket`: every transitive `depends_on`
/// prerequisite that has not reached shipped-complete, prerequisites-first
/// (the direct blocker, then its own blocker, ...). Visited-set guarded, so it
/// terminates on cyclic state by construction and never repeats an id.
#[must_use]
pub fn blocking_chain(state: &ProjectState, ticket: &TicketId) -> Vec<TicketId> {
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    seen.insert(ticket.clone());
    walk_chain(state, ticket, &mut chain, &mut seen);
    chain
}

fn walk_chain(
    state: &ProjectState,
    ticket: &TicketId,
    chain: &mut Vec<TicketId>,
    seen: &mut HashSet<TicketId>,
) {
    let Some(t) = state.ticket(ticket) else {
        return;
    };
    for dep in t.depends_on() {
        if !seen.insert(dep.clone()) {
            continue;
        }
        let Some(d) = state.ticket(dep) else {
            continue;
        };
        if !resolved(d.status()) {
            chain.push(dep.clone());
            walk_chain(state, dep, chain, seen);
        }
    }
}

/// Every ticket sitting on a `depends_on` cycle — the cycle marker's member
/// set, so the graph can flag ALL of them, not just the first one found. A
/// ticket is a cycle member exactly when it can reach itself across at least
/// one dependency edge, so a per-node reachability walk settles each node.
#[must_use]
pub fn cycle_members(state: &ProjectState) -> BTreeSet<TicketId> {
    let mut members = BTreeSet::new();
    for t in &state.tickets {
        let start = t.id();
        let mut seen: HashSet<&TicketId> = HashSet::new();
        let mut stack: Vec<&TicketId> = t.depends_on().iter().collect();
        while let Some(id) = stack.pop() {
            if id == start {
                members.insert(start.clone());
                break;
            }
            if seen.insert(id) {
                if let Some(n) = state.ticket(id) {
                    stack.extend(n.depends_on().iter());
                }
            }
        }
    }
    members
}

/// Every `(dependent, missing)` pair whose `depends_on` names a ticket absent
/// from the project state — the 'unknown dependency' surface. Unknown ids are
/// never treated as satisfied (the DEV gate refuses the ticket), and they are
/// kept off the graph's node set; they are a radar finding of their own.
/// Repeated entries in one ticket's `depends_on` (possible only in a restored
/// or hand-edited state — `Ticket::add_dependency` refuses duplicates) are
/// reported once.
#[must_use]
pub fn unknown_dependencies(state: &ProjectState) -> Vec<(TicketId, TicketId)> {
    let mut out = Vec::new();
    for t in &state.tickets {
        let mut seen = HashSet::new();
        for dep in t.depends_on() {
            if seen.insert(dep.clone()) && state.ticket(dep).is_none() {
                out.push((t.id().clone(), dep.clone()));
            }
        }
    }
    out
}

/// The nodes/edges the dependencies endpoint serves, derived ONLY from
/// `ProjectState`: nodes are exactly the tickets in state (no phantom node for
/// an unknown dependency), edges are exactly the declared `depends_on` pairs
/// (including an edge whose target is absent — it IS a declared entry; the
/// absent id surfaces as unknown, not as a node). A repeated entry in one
/// ticket's `depends_on` is served once: the edge exists, it is not doubled.
#[must_use]
pub fn dependency_graph(state: &ProjectState) -> (Vec<GraphNode>, Vec<GraphEdge>) {
    let nodes = state
        .tickets
        .iter()
        .map(|t| GraphNode {
            id: t.id().clone(),
            status: t.status(),
        })
        .collect();
    let mut edges = Vec::new();
    for t in &state.tickets {
        let mut seen = HashSet::new();
        for dep in t.depends_on() {
            if seen.insert(dep.clone()) {
                edges.push(GraphEdge {
                    dependent: t.id().clone(),
                    prerequisite: dep.clone(),
                });
            }
        }
    }
    (nodes, edges)
}

/// The critical path to the next release: the longest chain of unfinished
/// (not shipped-complete) dependency work, dependent-first, matching
/// [`blocking_chain`]'s order. A chain spans at least one dependency EDGE — a
/// lone unfinished ticket with nothing waiting on it is workable work, not a
/// queue, and is no finding. Equal-length chains are broken by the highest
/// priority on the chain, then by the lexicographically smaller id sequence —
/// deterministic on every snapshot. Memoized per node, so a diamond-heavy DAG
/// costs nodes×edges and can never blow up exponentially on fan-in; the
/// visited-set guard makes a cyclic state terminate too (a back-edge is
/// simply not followed).
#[must_use]
pub fn critical_path(state: &ProjectState) -> Vec<TicketId> {
    let mut best: Vec<TicketId> = Vec::new();
    let mut memo: HashMap<TicketId, Vec<TicketId>> = HashMap::new();
    for t in &state.tickets {
        if resolved(t.status()) {
            continue;
        }
        let mut candidate = vec![t.id().clone()];
        candidate.extend(longest_unresolved_from(
            state,
            t.id(),
            &HashSet::new(),
            &mut memo,
        ));
        if candidate.len() >= 2 && better_chain(state, &candidate, &best) {
            best = candidate;
        }
    }
    best
}

/// The longest unresolved chain reachable from `ticket`'s dependencies,
/// excluding `ticket` itself. `seen` carries the nodes on the current walk so
/// a dependency cycle terminates the walk instead of recursing forever;
/// `memo` caches each node's best suffix so shared sub-chains are computed
/// once. On a DAG (everything that passes `ProjectState::validate`) the cache
/// is exact — the walk's ancestors can never reappear as descendants — so the
/// memo never trades correctness for speed.
fn longest_unresolved_from(
    state: &ProjectState,
    ticket: &TicketId,
    seen: &HashSet<TicketId>,
    memo: &mut HashMap<TicketId, Vec<TicketId>>,
) -> Vec<TicketId> {
    if let Some(cached) = memo.get(ticket) {
        return cached.clone();
    }
    let Some(t) = state.ticket(ticket) else {
        return Vec::new();
    };
    let mut seen = seen.clone();
    seen.insert(ticket.clone());
    let mut best: Vec<TicketId> = Vec::new();
    for dep in t.depends_on() {
        if seen.contains(dep) {
            continue;
        }
        let Some(d) = state.ticket(dep) else {
            continue;
        };
        if resolved(d.status()) {
            continue;
        }
        let mut candidate = vec![dep.clone()];
        candidate.extend(longest_unresolved_from(state, dep, &seen, memo));
        if better_chain(state, &candidate, &best) {
            best = candidate;
        }
    }
    memo.insert(ticket.clone(), best.clone());
    best
}

/// Chain preference: longer first, then the highest priority on the chain
/// (more important work first), then the lexicographically smaller id
/// sequence — so two equal candidates always resolve the same way.
fn better_chain(state: &ProjectState, candidate: &[TicketId], best: &[TicketId]) -> bool {
    if candidate.len() != best.len() {
        return candidate.len() > best.len();
    }
    let prio = |chain: &[TicketId]| {
        chain
            .iter()
            .filter_map(|id| state.ticket(id).map(|t| priority_rank(t.priority())))
            .max()
            .unwrap_or(0)
    };
    let candidate_prio = prio(candidate);
    let best_prio = prio(best);
    if candidate_prio != best_prio {
        return candidate_prio > best_prio;
    }
    candidate
        .iter()
        .map(TicketId::as_str)
        .collect::<Vec<&str>>()
        < best.iter().map(TicketId::as_str).collect::<Vec<&str>>()
}

/// Same ordering as `selection::priority_rank` (private there): High first.
fn priority_rank(p: Priority) -> u8 {
    match p {
        Priority::Low => 0,
        Priority::Medium => 1,
        Priority::High => 2,
    }
}

/// The derived radar summary for a state snapshot: every READY ticket the DEV
/// gate would refuse over dependencies — its full blocking chain, with unknown
/// dep ids appended (the gate refuses those too) — plus the critical path.
/// Tickets still being designed (Pending) or already running (InProgress) are
/// not radar findings: the operator's question is why work that LOOKS
/// actionable is not running.
#[must_use]
pub fn radar(state: &ProjectState) -> Radar {
    let unknown = unknown_dependencies(state);
    let blocked = state
        .tickets
        .iter()
        .filter(|t| t.status() == Status::Ready)
        .filter_map(|t| {
            let mut blockers = blocking_chain(state, t.id());
            blockers.extend(
                unknown
                    .iter()
                    .filter(|(dep, _)| dep == t.id())
                    .map(|(_, missing)| missing.clone()),
            );
            (!blockers.is_empty()).then(|| BlockedSummary {
                id: t.id().clone(),
                blockers,
            })
        })
        .collect();
    Radar {
        blocked,
        critical_path: critical_path(state),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coxagent_domain::{Complexity, Role, TechnicalDesign, Ticket, TicketType};

    fn tid(s: &str) -> TicketId {
        TicketId::new(s).expect("valid ticket id")
    }

    /// A feature walked to `status` along the only legal edges, declaring
    /// `deps` via the SA's `add_dependency` authority (same fixture shape the
    /// TDD suite pins).
    fn feature_at(id: &str, status: Status, deps: &[&str]) -> Ticket {
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
        for dep in deps {
            t.add_dependency(Role::Sa, tid(dep))
                .expect("SA declares the dependency");
        }
        if status != Status::Pending {
            t.set_technical_design(Role::Sa, TechnicalDesign::default())
                .expect("attach design");
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

    // --- the SA test plan: multi-hop collapse ---

    #[test]
    fn a_done_mid_link_releases_the_downstream_chain_like_the_dev_gate() {
        // C waits on B waits on A; A is the root blocker (InProgress).
        let state = state_with(vec![
            feature_at("FEAT-A", Status::InProgress, &[]),
            feature_at("FEAT-B", Status::Ready, &["FEAT-A"]),
            feature_at("FEAT-C", Status::Ready, &["FEAT-B"]),
        ]);
        assert_eq!(
            blocking_chain(&state, &tid("FEAT-C")),
            vec![tid("FEAT-B"), tid("FEAT-A")],
            "the full chain is transitive, prerequisites-first"
        );
        // B reaching Done releases C entirely — the DEV gate reads only
        // C's DIRECT deps, so the radar must stop the chain there too,
        // never badge work DEV can actually take. The walk is the only
        // legal feature edge pair, nothing staged.
        let mut live = state;
        let b = live.ticket_mut(&tid("FEAT-B")).expect("FEAT-B present");
        b.transition_to(Role::DevFeature, Status::InProgress)
            .expect("Ready -> InProgress");
        b.transition_to(Role::DevFeature, Status::Done)
            .expect("InProgress -> Done");
        assert_eq!(
            blocking_chain(&live, &tid("FEAT-C")),
            Vec::<TicketId>::new(),
            "a Done mid-link releases its dependents entirely: the radar reads \
             dependencies exactly like the DEV gate — DIRECT deps only, so a \
             resolved mid-link ends the chain"
        );
    }

    // --- the SA test plan: cycle safety ---

    #[test]
    fn cycles_terminate_and_flag_every_member_plus_the_observer() {
        // Pure cycle A<->B; C depends on the cycle (observes it, not on it).
        let state = state_with(vec![
            feature_at("FEAT-A", Status::Ready, &["FEAT-B"]),
            feature_at("FEAT-B", Status::Ready, &["FEAT-A"]),
            feature_at("FEAT-C", Status::Ready, &["FEAT-A"]),
        ]);
        assert_eq!(
            cycle_members(&state),
            BTreeSet::from([tid("FEAT-A"), tid("FEAT-B")]),
            "both cycle members flagged; the observer is not on the cycle"
        );
        // Every walk over the cyclic state returns instead of recursing forever.
        assert_eq!(blocking_chain(&state, &tid("FEAT-A")), vec![tid("FEAT-B")]);
        assert_eq!(
            blocking_chain(&state, &tid("FEAT-C")),
            vec![tid("FEAT-A"), tid("FEAT-B")]
        );
        assert_eq!(
            critical_path(&state).len(),
            3,
            "the walk terminates: C, A, B"
        );
        let (nodes, edges) = dependency_graph(&state);
        assert_eq!(nodes.len(), 3);
        assert_eq!(edges.len(), 3, "exactly the three declared edges");
    }

    // --- the SA test plan: critical-path tie-breaking ---

    #[test]
    fn critical_path_prefers_higher_priority_then_smaller_ids_on_ties() {
        // Two equal-length chains: FEAT-H1 is High priority, FEAT-L1 Medium —
        // everything else identical, so the priority rule alone decides.
        let mut high = feature_at("FEAT-H1", Status::Ready, &["FEAT-H0"]);
        high.set_priority(Role::User, Priority::High)
            .expect("a person re-prioritises");
        let low = feature_at("FEAT-L1", Status::Ready, &["FEAT-L0"]);
        let state = state_with(vec![
            feature_at("FEAT-H0", Status::InProgress, &[]),
            feature_at("FEAT-L0", Status::InProgress, &[]),
            high,
            low,
        ]);
        assert_eq!(
            critical_path(&state),
            vec![tid("FEAT-H1"), tid("FEAT-H0")],
            "equal length: the chain with the higher-priority work wins"
        );
    }

    #[test]
    fn critical_path_tie_falls_through_to_id_order_for_determinism() {
        // Two identical Medium chains differing only in ids: the smaller id
        // sequence wins, so the answer is the same on every snapshot.
        let a = feature_at("FEAT-B", Status::Ready, &["FEAT-R"]);
        let b = feature_at("FEAT-A", Status::Ready, &["FEAT-R"]);
        let state = state_with(vec![feature_at("FEAT-R", Status::InProgress, &[]), a, b]);
        assert_eq!(
            critical_path(&state),
            vec![tid("FEAT-A"), tid("FEAT-R")],
            "equal length and priority: the smaller id sequence is deterministic"
        );
    }

    // --- the SA test plan: empty state ---

    #[test]
    fn empty_state_yields_an_empty_radar() {
        let radar = radar(&ProjectState::default());
        assert!(radar.blocked.is_empty());
        assert!(radar.critical_path.is_empty());
        let (nodes, edges) = dependency_graph(&ProjectState::default());
        assert!(nodes.is_empty() && edges.is_empty());
    }

    // --- radar summary semantics ---

    #[test]
    fn the_radar_reports_ready_tickets_only_and_includes_unknown_deps() {
        // FEAT-B: blocked by InProgress FEAT-A. FEAT-U: blocked by an id that
        // does not exist. FEAT-P: still Pending (not the radar's question).
        let state = state_with(vec![
            feature_at("FEAT-A", Status::InProgress, &[]),
            feature_at("FEAT-B", Status::Ready, &["FEAT-A"]),
            feature_at("FEAT-U", Status::Ready, &["FEAT-ZZZ"]),
            feature_at("FEAT-P", Status::Pending, &["FEAT-A"]),
        ]);
        let summary = radar(&state);
        assert_eq!(
            summary.blocked,
            vec![
                BlockedSummary {
                    id: tid("FEAT-B"),
                    blockers: vec![tid("FEAT-A")],
                },
                BlockedSummary {
                    id: tid("FEAT-U"),
                    blockers: vec![tid("FEAT-ZZZ")],
                },
            ],
            "Ready-only, chain first, unknown dep appended: the badge must fire \
             wherever the DEV gate refuses the ticket over dependencies"
        );
        // An InProgress ticket with an unresolved dep is already running — it
        // shows in blocked_by, never in the Ready-only radar.
        let running = state_with(vec![
            feature_at("FEAT-A", Status::Pending, &[]),
            feature_at("FEAT-B", Status::InProgress, &["FEAT-A"]),
        ]);
        assert!(radar(&running).blocked.is_empty());
        assert_eq!(
            blocked_by(&running, &tid("FEAT-B")),
            vec![Blocker {
                ticket: tid("FEAT-A"),
                status: Status::Pending,
            }],
            "the detail surface reports the live status whatever the ticket's own state"
        );
    }

    #[test]
    fn unknown_dependencies_never_leak_into_nodes_or_blockers() {
        let state = state_with(vec![feature_at("FEAT-B", Status::Ready, &["FEAT-ZZZ"])]);
        assert_eq!(
            unknown_dependencies(&state),
            vec![(tid("FEAT-B"), tid("FEAT-ZZZ"))]
        );
        assert!(blocked_by(&state, &tid("FEAT-B")).is_empty());
        let (nodes, edges) = dependency_graph(&state);
        assert_eq!(nodes.len(), 1, "no phantom node for the absent id");
        assert_eq!(edges.len(), 1, "the declared edge is still served");
    }

    // --- review findings: fan-in blowup and duplicate declared entries ---

    #[test]
    fn critical_path_survives_a_diamond_heavy_dag_without_blowing_up() {
        // 24 chained diamonds: F(i) is waited on by two parallel tickets, both
        // waited on by F(i+1). Every simple path is a candidate chain, so an
        // un-memoized walk explodes combinatorially (2^24 here) — the same
        // "must not hang" class AC2 guards for cycles, via fan-in. The
        // memoized walk costs nodes×edges and stays instant.
        let mut tickets = Vec::new();
        for i in 0..=24 {
            let id = format!("FEAT-F{i:02}");
            tickets.push(
                Ticket::new(
                    tid(&id),
                    TicketType::Feature,
                    format!("gate {i}"),
                    "fixture",
                    Priority::Medium,
                    Complexity::Medium,
                    false,
                )
                .expect("ticket"),
            );
            if i < 24 {
                for side in ["A", "B"] {
                    tickets
                        .last_mut()
                        .expect("just pushed")
                        .add_dependency(Role::Sa, tid(&format!("FEAT-{side}{i:02}")))
                        .expect("dep");
                }
                for side in ["A", "B"] {
                    let mut mid = Ticket::new(
                        tid(&format!("FEAT-{side}{i:02}")),
                        TicketType::Feature,
                        format!("parallel {side}{i}"),
                        "fixture",
                        Priority::Medium,
                        Complexity::Medium,
                        false,
                    )
                    .expect("ticket");
                    mid.add_dependency(Role::Sa, tid(&format!("FEAT-F{:02}", i + 1)))
                        .expect("dep");
                    tickets.push(mid);
                }
            }
        }
        let state = state_with(tickets);
        // Longest chain: F00 -> A00 -> F01 -> ... -> F24 — 25 gates + 24
        // parallels. Equal length and priority at every diamond, so the id
        // tie-break deterministically picks every A side.
        let path = critical_path(&state);
        assert_eq!(path.len(), 49, "the full chain, one side per diamond");
        assert_eq!(path[0], tid("FEAT-F00"));
        assert_eq!(path[1], tid("FEAT-A00"));
        assert_eq!(path[2], tid("FEAT-F01"));
    }

    #[test]
    fn a_repeated_declared_dependency_is_reported_once_everywhere() {
        // `add_dependency` refuses duplicates, but a restored or hand-edited
        // state file can carry a repeated entry: every surface must treat the
        // pair as ONE edge/finding, never echo it. The duplicate arrives the
        // way such states really do — through the persisted JSON shape.
        let b = feature_at("FEAT-B", Status::Ready, &["FEAT-A", "FEAT-ZZZ"]);
        let mut raw = serde_json::to_value(&b).expect("ticket serializes");
        raw["depends_on"] = serde_json::json!(["FEAT-A", "FEAT-A", "FEAT-ZZZ", "FEAT-ZZZ"]);
        let b: Ticket = serde_json::from_value(raw).expect("restored ticket deserializes");
        let state = state_with(vec![feature_at("FEAT-A", Status::InProgress, &[]), b]);
        assert_eq!(
            blocked_by(&state, &tid("FEAT-B")),
            vec![Blocker {
                ticket: tid("FEAT-A"),
                status: Status::InProgress,
            }],
            "one blocker, not one per repeated entry"
        );
        assert_eq!(
            unknown_dependencies(&state),
            vec![(tid("FEAT-B"), tid("FEAT-ZZZ"))],
            "one unknown finding, not one per repeated entry"
        );
        let (_, edges) = dependency_graph(&state);
        assert_eq!(
            edges.len(),
            2,
            "B->A and B->ZZZ once each: the pair exists, it is not doubled"
        );
    }
}
