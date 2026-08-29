//! Structural-integrity audit for the persisted [`ProjectState`] document
//! (CXA-F229).
//!
//! `ProjectState::validate` guards the schema-level invariants that predate
//! this module (unique ticket ids, dependency edges, dependency cycles). This
//! module is the missing counterpart for the aggregate as a whole: id
//! uniqueness per type, monotonic cycle counters, and the referential edges
//! between the ticket-keyed maps and the ticket list (evidence -> ticket,
//! holds -> ticket, journals -> ticket, …).
//!
//! Everything here is a PURE function over an already-loaded snapshot — no IO,
//! no clock, no engine. The stores call [`StateIntegrityAuditor::check`] next
//! to their existing validation before persisting (so a mutation whose
//! post-state fails the audit is refused at the write boundary and its payload
//! quarantined), and the hub exposes the same findings read-only via
//! `GET /api/projects/:pid/store?op=audit`.

use serde::Serialize;
use std::collections::HashSet;

use super::ProjectState;

/// Rule id: the same ticket id appears twice in `tickets`.
pub const DUPLICATE_TICKET_ID: &str = "duplicate_ticket_id";
/// Rule id: the same wiki page id appears twice in `docs`.
pub const DUPLICATE_DOC_ID: &str = "duplicate_doc_id";
/// Rule id: the same channel slug appears twice in `channels`.
pub const DUPLICATE_CHANNEL_ID: &str = "duplicate_channel_id";
/// Rule id: the same comment id appears twice in `comments`.
pub const DUPLICATE_COMMENT_ID: &str = "duplicate_comment_id";
/// Rule id: the same chat message id appears twice in `chat`.
pub const DUPLICATE_CHAT_ID: &str = "duplicate_chat_id";
/// Rule id: the same planned-sprint id appears twice in `sprint_queue`.
pub const DUPLICATE_PLANNED_SPRINT_ID: &str = "duplicate_planned_sprint_id";
/// Rule id: a ticket-keyed map names a ticket that does not exist. `ticket_id`
/// carries the dangling key; `detail` names the map.
pub const DANGLING_TICKET_REFERENCE: &str = "dangling_ticket_reference";
/// Rule id: a cycle counter is behind a recorded high-water mark, i.e. the
/// counter was rolled back (a restore from an older snapshot mid-flight).
pub const CYCLE_COUNTER_REGRESSION: &str = "cycle_counter_regression";
/// Rule id: `deploy_index` is behind `last_good_deploy.deploy_index`.
pub const DEPLOY_INDEX_REGRESSION: &str = "deploy_index_regression";
/// Rule id: a spend aggregate is negative — money can only accumulate.
pub const NEGATIVE_SPEND: &str = "negative_spend";

/// One structural-integrity finding. `ticket_id` is the ticket a finding is
/// about (the dangling key for reference rules, the duplicated id for
/// uniqueness rules) and `None` for findings that are not about a ticket.
/// Serialized with `ticket_id` always present — `null` when not applicable —
/// so the audit API contract stays `{rule_id, ticket_id, detail}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntegrityFinding {
    pub rule_id: String,
    pub ticket_id: Option<String>,
    pub detail: String,
}

impl IntegrityFinding {
    fn new(rule_id: &str, ticket_id: Option<String>, detail: String) -> Self {
        Self {
            rule_id: rule_id.to_owned(),
            ticket_id,
            detail,
        }
    }
}

/// Why a state snapshot was refused: the named findings the audit produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityViolation {
    pub findings: Vec<IntegrityFinding>,
}

impl std::fmt::Display for IntegrityViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "structural integrity audit failed ({}):",
            self.findings.len()
        )?;
        for finding in &self.findings {
            write!(f, " [{}] {}", finding.rule_id, finding.detail)?;
        }
        Ok(())
    }
}

impl std::error::Error for IntegrityViolation {}

/// The structural-integrity gate. Pure: [`Self::check`] is a total function
/// over a state snapshot and produces the named findings, nothing else.
pub struct StateIntegrityAuditor;

impl StateIntegrityAuditor {
    /// Audit `state`; `Ok(())` when every invariant holds, otherwise the
    /// violation naming every finding (never just the first — an operator
    /// repairing a corrupted document needs the full list).
    ///
    /// # Errors
    /// [`IntegrityViolation`] listing every failed invariant.
    pub fn check(state: &ProjectState) -> Result<(), IntegrityViolation> {
        let findings = state.audit_structural_integrity();
        if findings.is_empty() {
            Ok(())
        } else {
            Err(IntegrityViolation { findings })
        }
    }
}

/// Id uniqueness per type. A duplicated id makes every id-keyed lookup
/// ambiguous: claims, evidence, reactions and the whole ticket API would
/// silently hit one of two aggregates.
fn audit_id_uniqueness(state: &ProjectState) -> Vec<IntegrityFinding> {
    let mut findings = Vec::new();
    for id in duplicates(state.tickets.iter().map(|t| t.id().to_string())) {
        findings.push(IntegrityFinding::new(
            DUPLICATE_TICKET_ID,
            Some(id.clone()),
            format!("ticket id {id} appears more than once"),
        ));
    }
    for id in duplicates(state.docs.iter().map(|d| d.id.clone())) {
        findings.push(IntegrityFinding::new(
            DUPLICATE_DOC_ID,
            None,
            format!("doc id {id} appears more than once"),
        ));
    }
    for id in duplicates(state.channels.iter().map(|c| c.id.clone())) {
        findings.push(IntegrityFinding::new(
            DUPLICATE_CHANNEL_ID,
            None,
            format!("channel id {id} appears more than once"),
        ));
    }
    for id in duplicates(state.comments.iter().map(|c| c.id.clone())) {
        findings.push(IntegrityFinding::new(
            DUPLICATE_COMMENT_ID,
            None,
            format!("comment id {id} appears more than once"),
        ));
    }
    for id in duplicates(state.chat.iter().map(|m| m.id.clone())) {
        findings.push(IntegrityFinding::new(
            DUPLICATE_CHAT_ID,
            None,
            format!("chat message id {id} appears more than once"),
        ));
    }
    for id in duplicates(state.sprint_queue.iter().map(|p| p.id.to_string())) {
        findings.push(IntegrityFinding::new(
            DUPLICATE_PLANNED_SPRINT_ID,
            None,
            format!("planned sprint id {id} appears more than once"),
        ));
    }
    findings
}

/// Flag every key of the `map`-named ticket-keyed map that does not resolve
/// to a live ticket.
fn flag_dangling<'a>(
    findings: &mut Vec<IntegrityFinding>,
    map: &str,
    keys: impl Iterator<Item = &'a String>,
    tickets: &HashSet<String>,
) {
    for key in keys {
        if !tickets.contains(key) {
            findings.push(IntegrityFinding::new(
                DANGLING_TICKET_REFERENCE,
                Some(key.clone()),
                format!("{map} references missing ticket {key}"),
            ));
        }
    }
}

/// Referential edges: ticket-keyed maps -> tickets. Tickets are never removed
/// from the aggregate, so every key in a ticket-keyed map must resolve. A key
/// that does not is a dangling reference: evidence, holds or journal entries
/// attributed to a ticket nobody can open — exactly the corruption that later
/// poisons goal-line attribution.
fn audit_ticket_references(state: &ProjectState) -> Vec<IntegrityFinding> {
    let mut findings = Vec::new();
    let tickets: HashSet<String> = state.tickets.iter().map(|t| t.id().to_string()).collect();
    flag_dangling(
        &mut findings,
        "hold_reasons",
        state.hold_reasons.keys(),
        &tickets,
    );
    flag_dangling(
        &mut findings,
        "ticket_attachments",
        state.ticket_attachments.keys(),
        &tickets,
    );
    flag_dangling(
        &mut findings,
        "cost_holds",
        state.cost_holds.keys(),
        &tickets,
    );
    flag_dangling(
        &mut findings,
        "ticket_evidence",
        state.ticket_evidence.keys(),
        &tickets,
    );
    flag_dangling(
        &mut findings,
        "ticket_last_merge",
        state.ticket_last_merge.keys(),
        &tickets,
    );
    flag_dangling(
        &mut findings,
        "ticket_redesigns",
        state.ticket_redesigns.keys(),
        &tickets,
    );
    flag_dangling(
        &mut findings,
        "ticket_fail_attempts",
        state.ticket_fail_attempts.keys(),
        &tickets,
    );
    flag_dangling(
        &mut findings,
        "auto_approved_at",
        state.auto_approved_at.keys(),
        &tickets,
    );
    flag_dangling(
        &mut findings,
        "ticket_journal",
        state.ticket_journal.keys(),
        &tickets,
    );
    flag_dangling(
        &mut findings,
        "ticket_failures",
        state.ticket_failures.keys(),
        &tickets,
    );
    flag_dangling(
        &mut findings,
        "swept_tickets",
        state.swept_tickets.iter(),
        &tickets,
    );
    flag_dangling(
        &mut findings,
        "cost_approved",
        state.cost_approved.iter(),
        &tickets,
    );
    // `ticket_sessions` keys are `<ticket>/<role>` (run_dev session keys);
    // the TICKET prefix is what must resolve.
    for key in state.ticket_sessions.keys() {
        let ticket = key.split('/').next().unwrap_or("");
        if !tickets.contains(ticket) {
            findings.push(IntegrityFinding::new(
                DANGLING_TICKET_REFERENCE,
                Some(key.clone()),
                format!("ticket_sessions references missing ticket {key}"),
            ));
        }
    }
    findings
}

/// Monotonic counters. `cycle` only ever moves forward; every recorded
/// cycle-derived datum must therefore sit at or below it. A datum ahead of
/// the counter means the counter rolled back (restored from an older
/// snapshot mid-flight) and cadence/scorecard attribution is now
/// inconsistent. Same reasoning for the deploy ordinal.
fn audit_counters(state: &ProjectState) -> Vec<IntegrityFinding> {
    let mut findings = Vec::new();
    let high_water = state
        .cycle_scores
        .iter()
        .map(|s| s.cycle)
        .chain(state.sweeps_done.iter().copied())
        .max();
    if let Some(recorded) = high_water.filter(|recorded| state.cycle < *recorded) {
        findings.push(IntegrityFinding::new(
            CYCLE_COUNTER_REGRESSION,
            None,
            format!(
                "cycle counter {} is behind the recorded high-water mark {recorded} \
                 (cycle_scores/sweeps_done) — the counter rolled back",
                state.cycle
            ),
        ));
    }
    if let Some(sprint) = state
        .sprint
        .as_ref()
        .filter(|s| s.started_cycle > state.cycle)
    {
        findings.push(IntegrityFinding::new(
            CYCLE_COUNTER_REGRESSION,
            None,
            format!(
                "sprint {} started at cycle {} which is ahead of the cycle counter {}",
                sprint.number, sprint.started_cycle, state.cycle
            ),
        ));
    }
    if let Some(good) = state
        .last_good_deploy
        .as_ref()
        .filter(|d| d.deploy_index > state.deploy_index)
    {
        findings.push(IntegrityFinding::new(
            DEPLOY_INDEX_REGRESSION,
            None,
            format!(
                "deploy_index {} is behind last_good_deploy index {} — the deploy \
                 ordinal rolled back",
                state.deploy_index, good.deploy_index
            ),
        ));
    }
    findings
}

/// Spend aggregates. Spend only accumulates; a negative component is a
/// corrupted write, and every budget gate reading it would make the wrong
/// call.
fn audit_spend(state: &ProjectState) -> Vec<IntegrityFinding> {
    let mut findings = Vec::new();
    if state.spend.total_cost_usd < 0.0 || state.spend_today_usd < 0.0 {
        findings.push(IntegrityFinding::new(
            NEGATIVE_SPEND,
            None,
            format!(
                "negative spend: total {}, today {}",
                state.spend.total_cost_usd, state.spend_today_usd
            ),
        ));
    }
    let role_negative = state
        .spend
        .by_role
        .values()
        .chain(state.spend.metered_cost_by_role.values())
        .any(|v| *v < 0.0);
    let operator_negative = state.spend.by_operator.values().any(|op| op.cost_usd < 0.0);
    if role_negative || operator_negative {
        findings.push(IntegrityFinding::new(
            NEGATIVE_SPEND,
            None,
            "negative per-role or per-operator spend".to_owned(),
        ));
    }
    findings
}

/// Drop every key of a ticket-keyed map that does not resolve to a live
/// ticket, counting the removed entries. The audit and the heal share this
/// exact predicate (`!tickets.contains(key)`), so the heal removes precisely
/// the set the audit reported — audit-first-then-fix.
fn drop_dangling<V>(
    map: &mut std::collections::BTreeMap<String, V>,
    tickets: &HashSet<String>,
    healed: &mut usize,
) {
    let before = map.len();
    map.retain(|k, _| tickets.contains(k));
    *healed += before - map.len();
}

impl ProjectState {
    /// Structural-integrity findings for this snapshot. Pure and total: a
    /// well-formed document yields an empty vec; every invariant this hub
    /// relies on (unique ids per type, monotonic counters, evidence ->
    /// ticket -> milestone referential edges) is checked against real state.
    ///
    /// The ticket-keyed maps whose keys must resolve to a live ticket:
    /// `hold_reasons`, `ticket_attachments`, `cost_holds`, `ticket_evidence`,
    /// `ticket_last_merge`, `ticket_redesigns`, `ticket_fail_attempts`,
    /// `auto_approved_at`, `ticket_journal`, `ticket_failures`,
    /// `swept_tickets`, `cost_approved` and `ticket_sessions` (keyed
    /// `<ticket>/<role>` and checked on the ticket prefix).
    /// `pr_rescue_attempts` is deliberately absent — its keys are PR numbers,
    /// not ticket ids. Tickets are never removed from the aggregate, so a
    /// key that does not resolve is corruption, not cleanup lag; a new
    /// ticket-keyed map must be added here AND to
    /// [`ProjectState::heal_dangling_references`].
    #[must_use]
    pub fn audit_structural_integrity(&self) -> Vec<IntegrityFinding> {
        let mut findings = audit_id_uniqueness(self);
        findings.extend(audit_ticket_references(self));
        findings.extend(audit_counters(self));
        findings.extend(audit_spend(self));
        findings
    }

    /// Remove exactly the dangling ticket-keyed map entries the audit reports
    /// (audit-first-then-fix), then re-audit. Returns the number of map
    /// entries removed.
    ///
    /// Only the `dangling_ticket_reference` class self-heals: a dangling key
    /// has one safe repair (drop the entry — the ticket it names does not
    /// exist). Every other finding needs a human decision (which duplicate
    /// survives? what was the real cycle count?), so any remaining finding
    /// after the removal refuses the heal with the violation intact.
    ///
    /// # Errors
    /// [`IntegrityViolation`] when nothing is healable (no dangling
    /// references) or the post-heal state still fails the audit.
    pub fn heal_dangling_references(&mut self) -> Result<usize, IntegrityViolation> {
        let before = self.audit_structural_integrity();
        if before.is_empty() {
            return Ok(0);
        }
        if before
            .iter()
            .any(|f| f.rule_id != DANGLING_TICKET_REFERENCE)
        {
            return Err(IntegrityViolation { findings: before });
        }
        let tickets: HashSet<String> = self.tickets.iter().map(|t| t.id().to_string()).collect();

        let mut healed = 0;
        drop_dangling(&mut self.hold_reasons, &tickets, &mut healed);
        drop_dangling(&mut self.ticket_attachments, &tickets, &mut healed);
        drop_dangling(&mut self.cost_holds, &tickets, &mut healed);
        drop_dangling(&mut self.ticket_evidence, &tickets, &mut healed);
        drop_dangling(&mut self.ticket_last_merge, &tickets, &mut healed);
        drop_dangling(&mut self.ticket_redesigns, &tickets, &mut healed);
        drop_dangling(&mut self.ticket_fail_attempts, &tickets, &mut healed);
        drop_dangling(&mut self.auto_approved_at, &tickets, &mut healed);
        drop_dangling(&mut self.ticket_journal, &tickets, &mut healed);
        drop_dangling(&mut self.ticket_failures, &tickets, &mut healed);
        // Sets and the `<ticket>/<role>`-keyed session map.
        self.swept_tickets.retain(|k| tickets.contains(k));
        self.cost_approved.retain(|k| tickets.contains(k));
        self.ticket_sessions
            .retain(|k, _| tickets.contains(k.split('/').next().unwrap_or("")));

        let after = self.audit_structural_integrity();
        if !after.is_empty() {
            return Err(IntegrityViolation { findings: after });
        }
        Ok(healed)
    }
}

/// Ids that appear more than once, in first-duplicate order, deduped.
fn duplicates(ids: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut dupes = Vec::new();
    for id in ids {
        if !seen.insert(id.clone()) && !dupes.contains(&id) {
            dupes.push(id);
        }
    }
    dupes
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::state::{CycleScore, DocPage, Milestone, Sprint};
    use coxagent_domain::{Complexity, Priority, Ticket, TicketId, TicketType};

    const T1: &str = "CXA-F001";
    const T2: &str = "CXA-B001";
    const MISSING: &str = "CXA-F999";

    fn ticket(id: &str) -> Ticket {
        Ticket::new(
            TicketId::new(id).expect("valid ticket id"),
            TicketType::Feature,
            "title",
            "",
            Priority::Medium,
            Complexity::Small,
            false,
        )
        .expect("ticket")
    }

    /// A well-formed document exercising every rule family: tickets with
    /// evidence, a milestone goal, docs, channels, a live sprint, recorded
    /// cycle scores and a known-good deploy — all mutually consistent.
    fn seeded() -> ProjectState {
        let mut s = ProjectState::default();
        s.tickets.push(ticket(T1));
        s.tickets.push(ticket(T2));
        s.add_evidence(T1, "test", "regression test", "cargo test passes");
        s.milestones.push(Milestone {
            name: "Public beta".to_owned(),
            goal: "usable by a stranger".to_owned(),
            target_version: "0.5.0".to_owned(),
            goal_complete: false,
            fulfilled: false,
        });
        s.docs.push(DocPage {
            id: "feat-CXA-F001".to_owned(),
            folder: "Product".to_owned(),
            category: "product".to_owned(),
            title: "Feature".to_owned(),
            body: "body".to_owned(),
            updated_at: String::new(),
            updated_by: String::new(),
        });
        s.sprint = Some(Sprint {
            number: 1,
            goal: "ship".to_owned(),
            started_cycle: 4,
            length_cycles: 10,
            committed: vec![TicketId::new(T1).expect("id")],
            started_at: String::new(),
            bug_burn_floor: None,
        });
        s.cycle = 10;
        s.sprint_cycle = 4;
        s.cycle_scores.push(CycleScore {
            cycle: 9,
            ..CycleScore::default()
        });
        s.sweeps_done.push(7);
        s.deploy_index = 3;
        s.last_good_deploy = Some(crate::state::KnownGoodDeploy {
            sha: "abc".to_owned(),
            at: String::new(),
            deploy_index: 3,
            summary: "ok".to_owned(),
        });
        s
    }

    #[test]
    fn a_well_formed_seeded_state_passes_every_invariant() {
        // AC: running the auditor over well-formed state returns Ok for every
        // invariant — unique ids, monotonic counters, no dangling references.
        let mut state = seeded();
        assert!(
            StateIntegrityAuditor::check(&state).is_ok(),
            "seeded state must audit clean, got {:?}",
            state.audit_structural_integrity()
        );
        assert_eq!(
            state.heal_dangling_references().unwrap(),
            0,
            "nothing to heal"
        );
    }

    #[test]
    fn a_default_state_passes_every_invariant() {
        assert!(StateIntegrityAuditor::check(&ProjectState::default()).is_ok());
    }

    #[test]
    fn a_duplicated_ticket_id_is_a_named_violation() {
        // AC: injecting a duplicate id yields the specific named violation.
        let mut state = seeded();
        state.tickets.push(ticket(T1));
        let violation = StateIntegrityAuditor::check(&state).unwrap_err();
        assert!(violation
            .findings
            .iter()
            .any(|f| f.rule_id == DUPLICATE_TICKET_ID && f.ticket_id.as_deref() == Some(T1)));
    }

    #[test]
    fn dangling_evidence_is_a_named_violation() {
        // AC: injecting a dangling evidence reference yields the specific
        // named violation — evidence must never survive its ticket.
        let mut state = seeded();
        state.ticket_evidence.insert(
            MISSING.to_owned(),
            vec![crate::state::Evidence {
                kind: "test".to_owned(),
                label: "orphan".to_owned(),
                detail: "d".to_owned(),
                at: String::new(),
            }],
        );
        let violation = StateIntegrityAuditor::check(&state).unwrap_err();
        assert!(violation
            .findings
            .iter()
            .any(|f| f.rule_id == DANGLING_TICKET_REFERENCE
                && f.ticket_id.as_deref() == Some(MISSING)
                && f.detail.contains("ticket_evidence")));
    }

    #[test]
    fn a_rolled_back_cycle_counter_is_a_named_violation() {
        // AC: injecting a rolled-back cycle counter yields the specific named
        // violation — a counter behind its own recorded high-water mark.
        let mut state = seeded();
        state.cycle = 3;
        let violation = StateIntegrityAuditor::check(&state).unwrap_err();
        assert!(violation
            .findings
            .iter()
            .any(|f| f.rule_id == CYCLE_COUNTER_REGRESSION));
        // A sprint opened "in the future" is the same counter-rollback class.
        let mut future = seeded();
        future.sprint.as_mut().expect("sprint").started_cycle = 99;
        assert!(StateIntegrityAuditor::check(&future)
            .unwrap_err()
            .findings
            .iter()
            .any(|f| f.rule_id == CYCLE_COUNTER_REGRESSION));
    }

    #[test]
    fn a_rolled_back_deploy_index_is_a_named_violation() {
        let mut state = seeded();
        state.deploy_index = 1;
        assert!(StateIntegrityAuditor::check(&state)
            .unwrap_err()
            .findings
            .iter()
            .any(|f| f.rule_id == DEPLOY_INDEX_REGRESSION));
    }

    #[test]
    fn duplicates_in_every_id_bearing_collection_are_violations() {
        let mut state = seeded();
        state.docs.push(DocPage {
            id: "feat-CXA-F001".to_owned(),
            folder: String::new(),
            category: String::new(),
            title: String::new(),
            body: String::new(),
            updated_at: String::new(),
            updated_by: String::new(),
        });
        state.channels.push(crate::state::Channel {
            id: "dup".to_owned(),
            name: "dup".to_owned(),
            owner: String::new(),
            members: Vec::new(),
            inviters: Vec::new(),
            created_at: String::new(),
            kind: "private".to_owned(),
            parent: String::new(),
            open_invite: false,
            topic: String::new(),
            project: String::new(),
        });
        // A second channel with the SAME slug — the duplicate under test.
        state.channels.push(crate::state::Channel {
            id: "dup".to_owned(),
            name: "dup again".to_owned(),
            owner: String::new(),
            members: Vec::new(),
            inviters: Vec::new(),
            created_at: String::new(),
            kind: "private".to_owned(),
            parent: String::new(),
            open_invite: false,
            topic: String::new(),
            project: String::new(),
        });
        state.sprint_queue.push(crate::state::PlannedSprint {
            id: 1,
            goal: "g".to_owned(),
            tickets: Vec::new(),
            created_at: String::new(),
            by: String::new(),
        });
        state.sprint_queue.push(crate::state::PlannedSprint {
            id: 1,
            goal: "g2".to_owned(),
            tickets: Vec::new(),
            created_at: String::new(),
            by: String::new(),
        });
        let rules: Vec<String> = StateIntegrityAuditor::check(&state)
            .unwrap_err()
            .findings
            .iter()
            .map(|f| f.rule_id.clone())
            .collect();
        for expected in [
            DUPLICATE_DOC_ID,
            DUPLICATE_CHANNEL_ID,
            DUPLICATE_PLANNED_SPRINT_ID,
        ] {
            assert!(
                rules.iter().any(|r| r == expected),
                "missing {expected} in {rules:?}"
            );
        }
    }

    #[test]
    fn every_ticket_keyed_map_is_checked_for_dangling_keys() {
        let mut state = seeded();
        state
            .hold_reasons
            .insert(MISSING.to_owned(), "why".to_owned());
        state.cost_holds.insert(MISSING.to_owned(), 1.0);
        state.ticket_failures.insert(MISSING.to_owned(), Vec::new());
        state.swept_tickets.insert(MISSING.to_owned());
        // `ticket_sessions` keys are `<ticket>/<role>`: the TICKET prefix is
        // what must resolve.
        state
            .ticket_sessions
            .insert(format!("{T1}/dev_bug"), "sid".to_owned());
        state
            .ticket_sessions
            .insert(format!("{MISSING}/dev_bug"), "sid2".to_owned());
        let violation = StateIntegrityAuditor::check(&state).unwrap_err();
        let dangling: Vec<&str> = violation
            .findings
            .iter()
            .filter(|f| f.rule_id == DANGLING_TICKET_REFERENCE)
            .map(|f| f.detail.as_str())
            .collect();
        // Four bare-id maps plus the `ticket_sessions` entry whose ticket
        // prefix is dangling.
        assert_eq!(dangling.len(), 5, "{dangling:?}");
        assert!(dangling.iter().any(|d| d.contains("hold_reasons")));
        assert!(dangling.iter().any(|d| d.contains("cost_holds")));
        assert!(dangling.iter().any(|d| d.contains("ticket_failures")));
        assert!(dangling.iter().any(|d| d.contains("swept_tickets")));
        assert!(
            dangling.iter().any(|d| d.contains("ticket_sessions")),
            "session keys are checked on their ticket prefix: {dangling:?}"
        );
        // ...and the healthy `<live-ticket>/<role>` key was never flagged.
        assert!(!dangling
            .iter()
            .any(|d| d.contains(&format!("{T1}/dev_bug"))));
    }

    #[test]
    fn a_negative_spend_is_a_violation_but_zero_is_not() {
        let mut state = seeded();
        state.spend.total_cost_usd = -0.5;
        state.spend_today_usd = -0.5;
        assert!(StateIntegrityAuditor::check(&state)
            .unwrap_err()
            .findings
            .iter()
            .any(|f| f.rule_id == NEGATIVE_SPEND));
        let mut clean = seeded();
        clean.spend.total_cost_usd = 12.5;
        clean.spend.by_role.insert("dev_feature".to_owned(), 0.0);
        assert!(StateIntegrityAuditor::check(&clean).is_ok());
    }

    #[test]
    fn check_reports_every_finding_not_just_the_first() {
        // An operator repairing a corrupted document needs the full list.
        let mut state = seeded();
        state.tickets.push(ticket(T1));
        state.ticket_evidence.insert(MISSING.to_owned(), Vec::new());
        state.cycle = 1;
        let violation = StateIntegrityAuditor::check(&state).unwrap_err();
        let rules: Vec<&str> = violation
            .findings
            .iter()
            .map(|f| f.rule_id.as_str())
            .collect();
        assert!(rules.contains(&DUPLICATE_TICKET_ID), "{rules:?}");
        assert!(rules.contains(&DANGLING_TICKET_REFERENCE), "{rules:?}");
        assert!(rules.contains(&CYCLE_COUNTER_REGRESSION), "{rules:?}");
    }

    #[test]
    fn violation_display_names_every_rule() {
        let mut state = seeded();
        state.tickets.push(ticket(T1));
        let text = StateIntegrityAuditor::check(&state)
            .unwrap_err()
            .to_string();
        assert!(text.contains(DUPLICATE_TICKET_ID), "{text}");
    }

    #[test]
    fn heal_drops_only_dangling_entries_and_keeps_the_rest() {
        let mut state = seeded();
        state.ticket_evidence.insert(MISSING.to_owned(), Vec::new());
        state.hold_reasons.insert(T1.to_owned(), "kept".to_owned());
        state
            .hold_reasons
            .insert(MISSING.to_owned(), "gone".to_owned());
        let healed = state.heal_dangling_references().unwrap();
        assert_eq!(healed, 2, "the two dangling entries are removed");
        assert!(!state.ticket_evidence.contains_key(MISSING));
        assert_eq!(
            state.hold_reasons.get(T1).map(String::as_str),
            Some("kept"),
            "live entries are untouched"
        );
        assert_eq!(state.hold_reasons.len(), 1);
        // Post-heal the document audits clean again.
        assert!(StateIntegrityAuditor::check(&state).is_ok());
    }

    #[test]
    fn heal_refuses_when_anything_beyond_dangling_references_is_wrong() {
        // A duplicate id needs a human decision (which copy survives?) — the
        // heal must refuse, not improvise.
        let mut state = seeded();
        state.tickets.push(ticket(T1));
        state.ticket_evidence.insert(MISSING.to_owned(), Vec::new());
        let err = state.heal_dangling_references().unwrap_err();
        assert!(err
            .findings
            .iter()
            .any(|f| f.rule_id == DUPLICATE_TICKET_ID));
        // And it must not have touched anything.
        assert!(state.ticket_evidence.contains_key(MISSING));
    }

    #[test]
    fn heal_of_a_clean_state_is_a_no_op() {
        let mut state = seeded();
        assert_eq!(state.heal_dangling_references().unwrap(), 0);
    }
}
