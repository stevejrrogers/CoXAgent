//! `AddTicketUseCase` — the first vertical slice: load state, append a validated
//! ticket, persist atomically. Demonstrates the port wiring end to end.

use crate::error::AppError;
use crate::ports::outbound::StateStorePort;
use coxagent_domain::{Complexity, Priority, Ticket, TicketId, TicketType};
use std::sync::Arc;

/// Input for adding a ticket. Id minting lives here for now (per-type prefix).
pub struct AddTicketInput {
    pub ticket_type: TicketType,
    pub title: String,
    pub description: String,
    pub priority: Priority,
    pub complexity: Complexity,
    pub has_ui: bool,
    /// Up to 5 acceptance criteria (blanks trimmed, extras dropped).
    pub acceptance_criteria: Vec<String>,
}

/// Marker prefix on the duplicate-refusal error, so best-effort callers that
/// file MANY tickets (conformance drift, discussion outcomes) can skip a
/// refused one instead of aborting the whole run.
pub const DUPLICATE_REFUSED: &str = "duplicate ticket refused";

/// Adds a new ticket to the project backlog.
pub struct AddTicketUseCase<S: StateStorePort + ?Sized> {
    store: Arc<S>,
}

impl<S: StateStorePort + ?Sized> AddTicketUseCase<S> {
    pub fn new(store: Arc<S>) -> Self {
        Self { store }
    }

    /// Execute the use case, returning the id of the created ticket.
    ///
    /// # Errors
    /// - [`AppError::Domain`] when the ticket fails construction.
    /// - [`AppError::Port`] when load/save fails or the result is invalid.
    pub async fn execute(&self, input: AddTicketInput) -> Result<TicketId, AppError> {
        let mut state = self.store.load().await?;

        // Duplicate gate at the ONE shared creation path. The BA's insert
        // loop already dedupes, but discussion/sentinel/escalation tickets
        // came through here unchecked — the SM's bug-backlog discussions
        // filed five near-identical "bug triage" tickets in a week, each a
        // different string, same ticket. Same predicate as the BA's
        // (normalised match, near-paraphrase, ceremony-class).
        // Active tickets block a duplicate outright. REJECTED titles block it
        // too: a rejection is a human "no" to the whole theme, and terminal
        // status must not reopen the door — the BA refiled the same bug-triage
        // meta ticket a 7th time the moment its 6 predecessors were rejected.
        let active: Vec<String> = state
            .tickets
            .iter()
            .filter(|t| {
                !matches!(
                    t.status(),
                    coxagent_domain::Status::Done
                        | coxagent_domain::Status::Documented
                        | coxagent_domain::Status::Rejected
                        | coxagent_domain::Status::Verified
                )
            })
            .map(|t| t.title().to_owned())
            .collect();
        let rejected: Vec<String> = state
            .tickets
            .iter()
            .filter(|t| t.status() == coxagent_domain::Status::Rejected)
            .map(|t| t.title().to_owned())
            .collect();
        if crate::parsing::duplicates_existing(&input.title, &active) {
            return Err(crate::error::PortError::Backend(format!(
                "{DUPLICATE_REFUSED}: \"{}\" matches an open ticket (same theme \
                 already tracked)",
                input.title
            ))
            .into());
        }
        if crate::parsing::duplicates_existing(&input.title, &rejected) {
            return Err(crate::error::PortError::Backend(format!(
                "{DUPLICATE_REFUSED}: \"{}\" matches a REJECTED ticket — a human \
                 already said no to this theme; do not refile it",
                input.title
            ))
            .into());
        }

        let id = mint_id(input.ticket_type, &state)?;
        let mut ticket = Ticket::new(
            id.clone(),
            input.ticket_type,
            input.title,
            input.description,
            input.priority,
            input.complexity,
            input.has_ui,
        )?;
        ticket.set_acceptance_criteria(input.acceptance_criteria);
        state.tickets.push(ticket);

        state.validate().map_err(crate::error::PortError::Corrupt)?;
        self.store.save(&state).await?;
        Ok(id)
    }
}

/// Mint the next id for a type by counting existing tickets of that prefix.
///
/// # Errors
/// Propagates [`DomainError`] from id construction (unreachable in practice
/// since the formatted string is always non-empty, but kept honest).
pub(crate) fn mint_id(
    ticket_type: TicketType,
    state: &crate::state::ProjectState,
) -> Result<TicketId, AppError> {
    // One-letter type code: F(eature), B(ug), C(hore).
    let code = match ticket_type {
        TicketType::Feature => 'F',
        TicketType::Bug => 'B',
        TicketType::Chore => 'C',
    };
    // Ids read `CXC-F001` with an alias, or `F001` without one.
    let prefix = if state.alias.is_empty() {
        code.to_string()
    } else {
        format!("{}-{code}", state.alias)
    };
    let next = state
        .tickets
        .iter()
        .filter(|t| t.id().as_str().starts_with(&prefix))
        .count()
        + 1;
    Ok(TicketId::new(format!("{prefix}{next:03}"))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::outbound::StateStorePort;
    use crate::state::ProjectState;
    use crate::PortError;
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// In-memory store — lets us unit-test the use case with no filesystem.
    #[derive(Default)]
    struct MemStore {
        state: Mutex<ProjectState>,
    }

    #[async_trait]
    impl StateStorePort for MemStore {
        async fn load(&self) -> Result<ProjectState, PortError> {
            Ok(self.state.lock().map_err(poison)?.clone())
        }
        async fn save(&self, state: &ProjectState) -> Result<(), PortError> {
            state.validate().map_err(PortError::Corrupt)?;
            *self.state.lock().map_err(poison)? = state.clone();
            Ok(())
        }
    }

    fn poison<T>(_: std::sync::PoisonError<T>) -> PortError {
        PortError::Backend("lock poisoned".to_owned())
    }

    fn input(ticket_type: TicketType) -> AddTicketInput {
        input_titled(ticket_type, "T")
    }

    fn input_titled(ticket_type: TicketType, title: &str) -> AddTicketInput {
        AddTicketInput {
            ticket_type,
            title: title.to_owned(),
            description: String::new(),
            priority: Priority::Medium,
            complexity: Complexity::Small,
            has_ui: false,
            acceptance_criteria: Vec::new(),
        }
    }

    #[tokio::test]
    async fn mints_sequential_ids_per_type() {
        let store = Arc::new(MemStore::default());
        let uc = AddTicketUseCase::new(Arc::clone(&store));

        let a = uc
            .execute(input_titled(TicketType::Feature, "alpha widget"))
            .await
            .expect("a");
        let b = uc
            .execute(input_titled(TicketType::Feature, "beta exporter"))
            .await
            .expect("b");
        let c = uc
            .execute(input_titled(TicketType::Bug, "gamma crash"))
            .await
            .expect("c");

        assert_eq!(a.as_str(), "F001");
        assert_eq!(b.as_str(), "F002");
        assert_eq!(c.as_str(), "B001");
        assert_eq!(store.load().await.expect("load").tickets.len(), 3);
    }

    #[tokio::test]
    async fn refuses_near_duplicate_titles_from_any_caller() {
        let store = Arc::new(MemStore::default());
        let uc = AddTicketUseCase::new(Arc::clone(&store));
        uc.execute(input_titled(
            TicketType::Feature,
            "Bug triage and burn-down cadence",
        ))
        .await
        .expect("first");
        // A rephrasing of the same ceremony theme — the class that produced
        // five near-identical tickets in one week — is refused.
        assert!(uc
            .execute(input_titled(
                TicketType::Feature,
                "Backlog triage: assess bug risk and burn down"
            ))
            .await
            .is_err());
        assert_eq!(store.load().await.expect("load").tickets.len(), 1);
    }

    #[tokio::test]
    async fn refuses_refiling_a_rejected_theme() {
        let store = Arc::new(MemStore::default());
        let uc = AddTicketUseCase::new(Arc::clone(&store));
        let id = uc
            .execute(input_titled(
                TicketType::Feature,
                "Bug triage and burn-down cadence",
            ))
            .await
            .expect("first");
        // A human rejects the theme…
        {
            let mut s = store.load().await.expect("load");
            s.ticket_mut(&id)
                .expect("ticket")
                .transition_to(coxagent_domain::Role::Po, coxagent_domain::Status::Rejected)
                .expect("reject");
            store.save(&s).await.expect("save");
        }
        // …so refiling the same theme is refused even though nothing active
        // matches — a rejection must not reopen the door (the BA refiled the
        // triage meta ticket a 7th time the moment its predecessors died).
        let err = uc
            .execute(input_titled(
                TicketType::Feature,
                "Backlog triage: assess bug risk and burn down",
            ))
            .await
            .expect_err("refused");
        assert!(err.to_string().contains("REJECTED"), "{err}");
        assert_eq!(store.load().await.expect("load").tickets.len(), 1);
    }

    #[tokio::test]
    async fn rejects_blank_title() {
        let store = Arc::new(MemStore::default());
        let uc = AddTicketUseCase::new(store);
        let mut bad = input(TicketType::Feature);
        bad.title = "   ".to_owned();
        assert!(uc.execute(bad).await.is_err());
    }
}
