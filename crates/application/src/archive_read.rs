//! Pure read model over the archived-ticket cold store (CXA-F274).
//!
//! The board's archive surface and the detail fallback both need the same
//! three decisions — order, page window, totals — so they live here as pure
//! functions over the [`ArchiveStorePort`] result, unit-testable with struct
//! literals and no IO. The HTTP adapter (presentation) only wires parameters
//! and serialization around these.

use crate::error::PortError;
use crate::ports::outbound::ArchiveStorePort;
use coxagent_domain::ticket::Ticket;

/// Largest page the archive endpoint serves, regardless of what the caller
/// asks for (`limit` clamped 1..=[`MAX_LIMIT`]).
pub const MAX_LIMIT: usize = 200;

/// Page size when the caller does not ask for one.
pub const DEFAULT_LIMIT: usize = 50;

/// The whole archive ordered by ticket id, descending — newest ticket ids
/// first, the board's "newest first" archive column.
#[must_use]
pub fn sorted_desc(mut all: Vec<Ticket>) -> Vec<Ticket> {
    all.sort_by(|a, b| b.id().as_str().cmp(a.id().as_str()));
    all
}

/// Resolve the requested `(offset, limit)` query parameters onto a concrete
/// window: missing/negative offset is 0, missing limit is [`DEFAULT_LIMIT`],
/// limit below 1 or above [`MAX_LIMIT`] is clamped into `1..=MAX_LIMIT`.
#[must_use]
pub fn clamp_window(offset: Option<i64>, limit: Option<i64>) -> (usize, usize) {
    // Clamp in the query's i64 domain, then cross the width boundary with
    // checked conversions — the constants are far below any edge, but no
    // cast here is silent.
    let max = i64::try_from(MAX_LIMIT).unwrap_or(i64::MAX);
    let default = i64::try_from(DEFAULT_LIMIT).unwrap_or(max);
    let offset = usize::try_from(offset.unwrap_or(0).max(0)).unwrap_or(0);
    let limit = usize::try_from(limit.unwrap_or(default).clamp(1, max)).unwrap_or(MAX_LIMIT);
    (offset, limit)
}

/// One paged read over the archive: the requested window of tickets (ordered
/// id-descending) plus the TOTAL across all pages, so the board can size its
/// "Load more" affordance without fetching everything. Store errors propagate
/// untouched — the HTTP adapter decides the status mapping.
///
/// # Errors
/// [`PortError`] from the store adapter, verbatim.
pub async fn page(
    store: &dyn ArchiveStorePort,
    project: &str,
    offset: usize,
    limit: usize,
) -> Result<(Vec<Ticket>, usize), PortError> {
    let all = sorted_desc(store.list(project).await?);
    let total = all.len();
    let tickets = all.into_iter().skip(offset).take(limit).collect();
    Ok((tickets, total))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use async_trait::async_trait;
    use coxagent_domain::{Complexity, Priority, Ticket, TicketId, TicketType};

    fn ticket(id: &str) -> Ticket {
        Ticket::new(
            TicketId::new(id).unwrap(),
            TicketType::Feature,
            format!("ticket {id}"),
            "body",
            Priority::Medium,
            Complexity::Small,
            false,
        )
        .unwrap()
    }

    /// A cold store that IS its fixture: the tickets handed in answer every
    /// read. No IO, no server — the port contract in its smallest form.
    struct FixedStore(Vec<Ticket>);

    #[async_trait]
    impl ArchiveStorePort for FixedStore {
        async fn put(&self, _project: &str, _ticket: &Ticket) -> Result<(), PortError> {
            unreachable!("the read model never writes")
        }
        async fn get(&self, _project: &str, _id: &str) -> Result<Option<Ticket>, PortError> {
            unreachable!("paging reads through list")
        }
        async fn list(&self, _project: &str) -> Result<Vec<Ticket>, PortError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn ids_sort_descending_regardless_of_input_order() {
        let all = vec![ticket("CXC-F010"), ticket("CXC-B002"), ticket("CXC-F100")];
        let ids: Vec<String> = sorted_desc(all)
            .iter()
            .map(|t| t.id().as_str().to_owned())
            .collect();
        assert_eq!(ids, ["CXC-F100", "CXC-F010", "CXC-B002"]);
    }

    #[test]
    fn the_window_clamps_to_the_pinned_bounds() {
        assert_eq!(clamp_window(None, None), (0, DEFAULT_LIMIT));
        assert_eq!(clamp_window(Some(-5), Some(-1)), (0, 1));
        assert_eq!(clamp_window(Some(0), Some(0)), (0, 1));
        assert_eq!(clamp_window(Some(0), Some(10_000)), (0, MAX_LIMIT));
        assert_eq!(clamp_window(Some(120), Some(50)), (120, 50));
    }

    #[tokio::test]
    async fn paging_serves_windows_with_the_full_total() {
        // Seven tickets land id-descending as F006..F000; a 5-offset/2-limit
        // window is exactly the last two.
        let all: Vec<Ticket> = (0..7).rev().map(|i| ticket(&format!("CXC-F00{i}"))).collect();
        let (tickets, total) = page(&FixedStore(all), "p", 5, 2).await.unwrap();
        let ids: Vec<String> = tickets
            .iter()
            .map(|t| t.id().as_str().to_owned())
            .collect();
        assert_eq!(ids, ["CXC-F001", "CXC-F000"]);
        assert_eq!(total, 7);
    }

    #[tokio::test]
    async fn a_window_past_the_end_is_empty_while_the_total_still_counts() {
        let all: Vec<Ticket> = (0..3).map(|i| ticket(&format!("CXC-F00{i}"))).collect();
        let (tickets, total) = page(&FixedStore(all), "p", 30, 50).await.unwrap();
        assert!(tickets.is_empty());
        assert_eq!(total, 3);
    }

    #[tokio::test]
    async fn an_empty_archive_pages_to_nothing_with_a_zero_total() {
        let (tickets, total) = page(&FixedStore(Vec::new()), "p", 0, DEFAULT_LIMIT)
            .await
            .unwrap();
        assert!(tickets.is_empty());
        assert_eq!(total, 0);
    }
}
