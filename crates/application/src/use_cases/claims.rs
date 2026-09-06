//! The human claim lifecycle (CXA-F283): a person pulls a claimable ticket
//! into their own `InProgress` claim (takeover) or completes the work they
//! did by hand and returns the ticket to the loop (handback).
//!
//! Authority model: the domain only ever lets `System` touch a claim
//! (`claim`/`take_over`/`release_claim`), so this use case is the bookkeeping
//! the authorized human requested. WHO may take that decision is the pure
//! [`may_touch_claim`] predicate — a manager, or the account whose own run
//! holds the claim (their own crashed run — the same ownership philosophy the
//! crash-recovery release follows). Every external fact (the store, the
//! live-runner identity) arrives through the port or as a parameter: the
//! decisions here are pure functions over project state.

use crate::error::AppError;
use crate::ports::outbound::{mutate_state, StateStorePort};
use crate::state::ProjectState;
use coxagent_domain::{Role, Status, TicketId, TicketType};
use std::sync::Arc;

/// Whether `caller` may act on the claim currently held by `holder`.
///
/// `Some(holder)` is the `account@host` stamp on a claimed ticket: a manager
/// may always act, and so may the account whose own name is on the claim.
/// `None` (unclaimed ticket) has no owner to match — managers only.
#[must_use]
pub fn may_touch_claim(can_manage: bool, caller: &str, holder: Option<&str>) -> bool {
    match holder {
        None => can_manage,
        Some(held) => can_manage || claim_account(held) == caller,
    }
}

/// The account part of a worker identity (`account@host` -> `account`).
fn claim_account(worker: &str) -> &str {
    worker.split('@').next().unwrap_or(worker)
}

/// Why a claim action was refused. The presentation layer maps these onto
/// 404/403/409/500; the messages are the operator-facing "why".
#[derive(Debug, thiserror::Error)]
pub enum ClaimError {
    #[error("no such ticket")]
    NotFound,
    #[error("your role may not take this decision")]
    NotPermitted,
    #[error("{0}")]
    Conflict(String),
    #[error(transparent)]
    Store(#[from] AppError),
}

/// The result of a successful takeover: who held the claim before the swap
/// (the takeover's attribution), the new holder, and the mutated ticket.
#[derive(Debug, Clone)]
pub struct TakeoverOutcome {
    pub previous_holder: Option<String>,
    pub claimed_by: String,
    pub claimed_at: String,
    pub ticket: coxagent_domain::ticket::Ticket,
}

/// The result of a successful handback: the status the ticket landed on
/// (`Done` for feature/chore, `Fixed` for bug).
#[derive(Debug, Clone, Copy)]
pub struct HandbackOutcome {
    pub status: Status,
}

/// Takeover/handback over the state store. One atomic read-mutate-write per
/// action, with the audit trail (activity + ticket-thread comment) written in
/// the same save — a claim change is never visible without its attribution.
/// `S` may be the port's trait object (`Arc<dyn StateStorePort>`), so handlers
/// over any backing store can construct it directly from `ProjectHandle`.
pub struct ClaimsUseCase<S: StateStorePort + ?Sized> {
    store: Arc<S>,
}

impl<S: StateStorePort + ?Sized> ClaimsUseCase<S> {
    pub fn new(store: Arc<S>) -> Self {
        Self { store }
    }

    /// Pull ticket `id` into a human claim for `worker` (`account@host`).
    ///
    /// Two shapes, one invariant: an unclaimed claimable ticket (feature/chore
    /// on `Ready`, bug on `Open`) is claimed fresh; a claimed `InProgress`
    /// ticket has its holder swapped via `Ticket::take_over` — the operator
    /// taking over a stalled run. Refused while `active_runner` (this hub's
    /// runner identity, passed only when the runner is actually running) is
    /// the current holder: pause it first.
    ///
    /// # Errors
    /// [`ClaimError`] — the caller may not take this decision, the ticket is
    /// not claimable, or the store failed.
    pub async fn takeover_claim(
        &self,
        id: &TicketId,
        worker: &str,
        now: &str,
        can_manage: bool,
        caller: &str,
        active_runner: Option<&str>,
    ) -> Result<TakeoverOutcome, ClaimError> {
        let mut outcome = Err(ClaimError::NotFound);
        let saved = mutate_state(self.store.as_ref(), |s| {
            outcome = takeover_in_state(s, id, worker, now, can_manage, caller, active_runner);
            Ok(())
        })
        .await;
        match saved {
            Ok(()) => outcome,
            Err(e) => Err(ClaimError::Store(e.into())),
        }
    }

    /// Complete the work a human did by hand on `id`: feature/chore -> `Done`,
    /// bug -> `Fixed`. The implementation `note` (mandatory — the handback's
    /// audit trail) and the optional `branch_ref` (where the work lives) are
    /// recorded on the ticket thread in the same write. The transition out of
    /// `InProgress` clears the claim, so the loop picks the ticket up from its
    /// next queue (DOCS for features, TEST verification for bugs) and no DEV
    /// can re-claim it.
    ///
    /// # Errors
    /// [`ClaimError`] — the caller may not take this decision, the ticket is
    /// not in progress, or the store failed.
    pub async fn handback(
        &self,
        id: &TicketId,
        note: &str,
        branch_ref: &str,
        can_manage: bool,
        caller: &str,
        active_runner: Option<&str>,
    ) -> Result<HandbackOutcome, ClaimError> {
        let mut outcome = Err(ClaimError::NotFound);
        let saved = mutate_state(self.store.as_ref(), |s| {
            outcome = handback_in_state(s, id, note, branch_ref, can_manage, caller, active_runner);
            Ok(())
        })
        .await;
        match saved {
            Ok(()) => outcome,
            Err(e) => Err(ClaimError::Store(e.into())),
        }
    }
}

/// The runner-busy refusal: an agent is executing this very ticket on the
/// live runner, and pulling or completing it under the agent would race the
/// in-flight phase. The operator pauses first, then retries.
fn runner_busy(holder: Option<&str>, active_runner: Option<&str>) -> Option<String> {
    let held = holder?;
    let runner = active_runner?;
    (runner == held).then(|| {
        format!("runner {held} is actively working this ticket — pause it first")
    })
}

/// Pure takeover decision over one state snapshot (no IO — testable with a
/// struct-literal state, per the hexagonal gate).
fn takeover_in_state(
    state: &mut ProjectState,
    id: &TicketId,
    worker: &str,
    now: &str,
    can_manage: bool,
    caller: &str,
    active_runner: Option<&str>,
) -> Result<TakeoverOutcome, ClaimError> {
    let Some(view) = state.ticket(id) else {
        return Err(ClaimError::NotFound);
    };
    let holder = view.claimed_by().map(str::to_owned);
    // Owned copies up front: every later decision reads these, never `view`,
    // so the one mutable lookup below stays the only one.
    let kind = view.ticket_type();
    let status = view.status();
    if !may_touch_claim(can_manage, caller, holder.as_deref()) {
        return Err(ClaimError::NotPermitted);
    }
    if let Some(why) = runner_busy(holder.as_deref(), active_runner) {
        return Err(ClaimError::Conflict(why));
    }
    let conflict = |e: coxagent_domain::DomainError| ClaimError::Conflict(e.to_string());
    let t = state.ticket_mut(id).ok_or(ClaimError::NotFound)?;
    let (previous, comment) = if holder.is_some() {
        let previous = t.take_over(Role::System, worker, now).map_err(conflict)?;
        // The takeover names BOTH sides: who took it (the audit's attribution)
        // and whose claim was displaced.
        let from = previous
            .as_deref()
            .map(|h| format!(" (was {h})"))
            .unwrap_or_default();
        (previous, format!("🧑‍💻 {id} taken over by @{caller}{from}"))
    } else {
        // Fresh claim: only a Ready feature/chore or an Open bug is claimable —
        // the same queue the DEV agents pick from.
        let claimable = match kind {
            TicketType::Bug => status == Status::Open,
            TicketType::Feature | TicketType::Chore => status == Status::Ready,
        };
        if !claimable {
            return Err(ClaimError::Conflict(format!(
                "{id} is {} — only a Ready feature/chore or an Open bug can be taken over",
                format!("{status:?}").to_lowercase()
            )));
        }
        t.claim(Role::System, worker, now).map_err(conflict)?;
        (None, format!("🧑‍💻 {id} taken over by @{caller}"))
    };
    let ticket = t.clone();
    // One save carries the swap AND its audit trail.
    state.log_activity(caller, "took over the ticket", Some(id.to_string()));
    state.post_comment("USER", &comment, Some(id.to_string()));
    Ok(TakeoverOutcome {
        previous_holder: previous,
        claimed_by: worker.to_owned(),
        claimed_at: now.to_owned(),
        ticket,
    })
}

/// Pure handback decision over one state snapshot (no IO).
fn handback_in_state(
    state: &mut ProjectState,
    id: &TicketId,
    note: &str,
    branch_ref: &str,
    can_manage: bool,
    caller: &str,
    active_runner: Option<&str>,
) -> Result<HandbackOutcome, ClaimError> {
    let Some(view) = state.ticket(id) else {
        return Err(ClaimError::NotFound);
    };
    let holder = view.claimed_by().map(str::to_owned);
    if !may_touch_claim(can_manage, caller, holder.as_deref()) {
        return Err(ClaimError::NotPermitted);
    }
    if let Some(why) = runner_busy(holder.as_deref(), active_runner) {
        return Err(ClaimError::Conflict(why));
    }
    if view.status() != Status::InProgress {
        return Err(ClaimError::Conflict(format!(
            "{id} is {} — nothing in progress to hand back",
            format!("{:?}", view.status()).to_lowercase()
        )));
    }
    let to = match view.ticket_type() {
        TicketType::Bug => Status::Fixed,
        TicketType::Feature | TicketType::Chore => Status::Done,
    };
    let conflict = |e: coxagent_domain::DomainError| ClaimError::Conflict(e.to_string());
    let t = state
        .ticket_mut(id)
        .ok_or(ClaimError::NotFound)?;
    // Leaving InProgress through the transition table clears the claim —
    // the aggregate's own rule, not a separate un-claim.
    t.transition_to(Role::System, to).map_err(conflict)?;
    let mut body = format!("↩️ handed back to the loop: {note}");
    if !branch_ref.is_empty() {
        use std::fmt::Write as _;
        let _ = write!(body, "\nbranch/commit: {branch_ref}");
    }
    state.log_activity(caller, "handed back to the loop", Some(id.to_string()));
    state.post_comment("USER", &body, Some(id.to_string()));
    Ok(HandbackOutcome { status: to })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::{documentable_candidates, ready_feature_candidates};
    use crate::PortError;
    use coxagent_domain::ticket::Ticket;
    use coxagent_domain::{Complexity, Priority, TechnicalDesign};
    use std::sync::Mutex;

    const NOW: &str = "2026-09-06T12:00:00Z";

    #[derive(Default)]
    struct MemStore {
        state: Mutex<ProjectState>,
    }
    #[async_trait::async_trait]
    impl StateStorePort for MemStore {
        async fn load(&self) -> Result<ProjectState, PortError> {
            Ok(self.state.lock().expect("lock").clone())
        }
        async fn save(&self, s: &ProjectState) -> Result<(), PortError> {
            s.validate().map_err(PortError::Corrupt)?;
            *self.state.lock().expect("lock") = s.clone();
            Ok(())
        }
    }

    /// A designed feature claimed by `holder`, stuck InProgress.
    fn claimed_feature(id: &str, holder: &str) -> Ticket {
        let mut t = Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Feature,
            "f",
            "",
            Priority::High,
            Complexity::Small,
            false,
        )
        .expect("t");
        t.set_technical_design(Role::Sa, TechnicalDesign::default())
            .expect("d");
        t.transition_to(Role::Sa, Status::Ready).expect("ready");
        t.claim(Role::DevFeature, holder, NOW).expect("claim");
        t
    }

    /// A fresh bug: Open, unclaimed — the DEV agents' pickings.
    fn open_bug(id: &str) -> Ticket {
        Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Bug,
            "b",
            "",
            Priority::High,
            Complexity::Small,
            false,
        )
        .expect("bug")
    }

    fn store_with(tickets: Vec<Ticket>) -> Arc<MemStore> {
        Arc::new(MemStore {
            state: Mutex::new(ProjectState {
                tickets,
                ..ProjectState::default()
            }),
        })
    }

    #[tokio::test]
    async fn takeover_of_a_claimed_ticket_swaps_the_holder_and_persists() {
        let store = store_with(vec![claimed_feature("CXC-F1", "dev@mac")]);
        let uc = ClaimsUseCase::new(Arc::clone(&store));
        let out = uc
            .takeover_claim(
                &TicketId::new("CXC-F1").expect("id"),
                "alice@hub",
                NOW,
                true,
                "alice",
                None,
            )
            .await
            .expect("takeover");
        assert_eq!(out.previous_holder.as_deref(), Some("dev@mac"));
        assert_eq!(out.claimed_by, "alice@hub");
        assert_eq!(out.ticket.claimed_by(), Some("alice@hub"));
        let state = store.load().await.expect("load");
        let t = state.ticket(&TicketId::new("CXC-F1").expect("id")).expect("t");
        assert_eq!(t.status(), Status::InProgress);
        assert_eq!(t.claimed_by(), Some("alice@hub"));
    }

    #[tokio::test]
    async fn takeover_of_an_unclaimed_open_bug_claims_it_fresh() {
        let store = store_with(vec![open_bug("CXC-B1")]);
        let uc = ClaimsUseCase::new(Arc::clone(&store));
        let out = uc
            .takeover_claim(
                &TicketId::new("CXC-B1").expect("id"),
                "alice@hub",
                NOW,
                true,
                "alice",
                None,
            )
            .await
            .expect("takeover");
        assert_eq!(out.previous_holder, None);
        let t = store
            .load()
            .await
            .expect("load")
            .ticket(&TicketId::new("CXC-B1").expect("id"))
            .expect("t")
            .clone();
        assert_eq!(t.status(), Status::InProgress);
        assert_eq!(t.claimed_by(), Some("alice@hub"));
    }

    #[tokio::test]
    async fn takeover_refuses_a_non_claimable_status() {
        // A fresh feature sits on Pending (design gate) — nothing to take.
        let store = store_with(vec![claimed_feature("CXC-F1", "dev@mac")]);
        let mut state = store.load().await.expect("load");
        let mut pending = Ticket::new(
            TicketId::new("CXC-F2").expect("id"),
            TicketType::Feature,
            "gated",
            "",
            Priority::Medium,
            Complexity::Small,
            false,
        )
        .expect("pending");
        pending.stamp_created_at(NOW);
        state.tickets.push(pending);
        store.save(&state).await.expect("save");

        let err = ClaimsUseCase::new(store)
            .takeover_claim(
                &TicketId::new("CXC-F2").expect("id"),
                "alice@hub",
                NOW,
                true,
                "alice",
                None,
            )
            .await
            .expect_err("pending is not claimable");
        assert!(matches!(err, ClaimError::Conflict(_)));
    }

    #[tokio::test]
    async fn a_member_can_take_over_their_own_stalled_run() {
        // Not a manager — but the claim carries carol's own account (her
        // crashed run), and may_touch_claim hands her the decision.
        let store = store_with(vec![claimed_feature("CXC-F1", "carol@mac")]);
        let out = ClaimsUseCase::new(Arc::clone(&store))
            .takeover_claim(
                &TicketId::new("CXC-F1").expect("id"),
                "carol@hub",
                NOW,
                false,
                "carol",
                None,
            )
            .await
            .expect("own run may be taken over");
        assert_eq!(out.previous_holder.as_deref(), Some("carol@mac"));
        assert_eq!(out.ticket.claimed_by(), Some("carol@hub"));
        let state = store.load().await.expect("load");
        assert!(state
            .comments
            .iter()
            .any(|c| c.body.contains("taken over by @carol")
                && c.body.contains("was carol@mac")));
    }

    #[tokio::test]
    async fn takeover_refused_while_the_runner_is_working_the_ticket() {
        let store = store_with(vec![claimed_feature("CXC-F1", "op@mac")]);
        let err = ClaimsUseCase::new(store)
            .takeover_claim(
                &TicketId::new("CXC-F1").expect("id"),
                "alice@hub",
                NOW,
                true,
                "alice",
                Some("op@mac"),
            )
            .await
            .expect_err("runner is actively working");
        match err {
            ClaimError::Conflict(msg) => {
                assert!(msg.contains("pause it first"), "{msg}");
            }
            other => panic!("expected runner-busy conflict, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handback_completes_a_feature_as_done_and_clears_the_claim() {
        let store = store_with(vec![claimed_feature("CXC-F1", "carol@hub")]);
        let out = ClaimsUseCase::new(Arc::clone(&store))
            .handback(
                &TicketId::new("CXC-F1").expect("id"),
                "implemented on the branch",
                "feat/human",
                false,
                "carol",
                None,
            )
            .await
            .expect("handback");
        assert_eq!(out.status, Status::Done);
        let state = store.load().await.expect("load");
        let t = state.ticket(&TicketId::new("CXC-F1").expect("id")).expect("t");
        assert_eq!(t.status(), Status::Done);
        assert_eq!(t.claimed_by(), None, "the transition clears the claim");
        // The note + ref are recorded on the ticket thread.
        let note = state
            .comments
            .iter()
            .find(|c| c.ticket.as_deref() == Some("CXC-F1"))
            .expect("handback comment");
        assert!(note.body.contains("handed back to the loop"));
        assert!(note.body.contains("implemented on the branch"));
        assert!(note.body.contains("feat/human"));
        // Activity attribution.
        assert!(state
            .activity
            .iter()
            .any(|a| a.agent == "carol" && a.action == "handed back to the loop"));
    }

    #[tokio::test]
    async fn handback_completes_a_bug_as_fixed() {
        let mut b = open_bug("CXC-B1");
        b.claim(Role::DevBug, "carol@hub", NOW).expect("claim");
        let store = store_with(vec![b]);
        let out = ClaimsUseCase::new(Arc::clone(&store))
            .handback(
                &TicketId::new("CXC-B1").expect("id"),
                "fixed at source",
                "",
                false,
                "carol",
                None,
            )
            .await
            .expect("handback");
        assert_eq!(out.status, Status::Fixed);
        let t = store
            .load()
            .await
            .expect("load")
            .ticket(&TicketId::new("CXC-B1").expect("id"))
            .expect("t")
            .clone();
        assert_eq!(t.status(), Status::Fixed);
        assert_eq!(t.claimed_by(), None);
    }

    #[tokio::test]
    async fn handback_refused_off_in_progress() {
        let store = store_with(vec![open_bug("CXC-B1")]);
        let err = ClaimsUseCase::new(store)
            .handback(
                &TicketId::new("CXC-B1").expect("id"),
                "fixed at source",
                "",
                true,
                "alice",
                None,
            )
            .await
            .expect_err("Open bug has no work in flight");
        assert!(matches!(err, ClaimError::Conflict(_)));
    }

    #[tokio::test]
    async fn unknown_ticket_is_not_found() {
        let store = store_with(Vec::new());
        let err = ClaimsUseCase::new(store)
            .takeover_claim(
                &TicketId::new("CXC-F9").expect("id"),
                "alice@hub",
                NOW,
                true,
                "alice",
                None,
            )
            .await
            .expect_err("no such ticket");
        assert!(matches!(err, ClaimError::NotFound));
    }

    #[test]
    fn may_touch_claim_truth_table() {
        // Managers may act on any claim — and on unclaimed tickets.
        assert!(may_touch_claim(true, "alice", Some("dev@mac")));
        assert!(may_touch_claim(true, "alice", Some("carol@hub")));
        assert!(may_touch_claim(true, "alice", None));
        // A member may act only on their OWN account's claim.
        assert!(may_touch_claim(false, "carol", Some("carol@hub")));
        assert!(may_touch_claim(false, "carol", Some("carol"))); // bare-account holder
        assert!(!may_touch_claim(false, "carol", Some("dev@mac")));
        assert!(!may_touch_claim(false, "carol", None), "unclaimed -> managers only");
    }

    /// CXA-F283 AC5: the loop picks human work up with NO orchestrator change.
    /// A handed-back feature flows to DOCS (documentable_candidates); a
    /// handed-back bug reaches TEST verification through the existing Fixed
    /// queue run_test.rs sweeps; and neither can be re-claimed by DEV, because
    /// neither is Ready/Open any more.
    #[tokio::test]
    async fn handed_back_work_flows_into_the_existing_queues() {
        let f = claimed_feature("CXC-F1", "carol@hub");
        let mut b = open_bug("CXC-B1");
        b.claim(Role::DevBug, "carol@hub", NOW).expect("claim");
        let store = store_with(vec![f, b]);
        let uc = ClaimsUseCase::new(Arc::clone(&store));
        uc.handback(
            &TicketId::new("CXC-F1").expect("id"),
            "done by hand",
            "",
            false,
            "carol",
            None,
        )
        .await
        .expect("feature handback");
        uc.handback(
            &TicketId::new("CXC-B1").expect("id"),
            "fixed by hand",
            "",
            false,
            "carol",
            None,
        )
        .await
        .expect("bug handback");

        let state = store.load().await.expect("load");
        let fid = TicketId::new("CXC-F1").expect("id");
        let bid = TicketId::new("CXC-B1").expect("id");
        // DOCS queue picks the Done feature up for documentation.
        assert!(documentable_candidates(&state).contains(&fid));
        // The bug sits on Fixed — the exact queue run_test.rs verifies against
        // regression before promoting to Verified.
        assert_eq!(
            state.ticket(&bid).expect("bug").status(),
            Status::Fixed
        );
        // DEV's claim queue (Ready/Open) holds neither any more.
        assert!(
            !ready_feature_candidates(&state).contains(&fid),
            "a handed-back feature can never be re-claimed by DEV"
        );
        assert_eq!(state.ticket(&fid).expect("f").status(), Status::Done);
    }
}
