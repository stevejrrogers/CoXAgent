//! `list_panels` (CXA-B186b) — the read the dashboard uses to discover which
//! panels exist and in what order to render them.
//!
//! Pure application: the panel registry lives in the domain and holds no IO,
//! so this use case is a function over domain values — no port is needed for
//! the listing itself. If panel metadata is later discovered from the
//! filesystem or config, that read must arrive through a port in
//! `ports/outbound/` and the *registration* stays a pure function over the
//! metadata the port returns.

use coxagent_domain::{PanelLayoutSlot, PanelListing, PanelRegistry};

/// The slot filter a caller may pass: all panels, or exactly one slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelSlotFilter {
    /// Every registered panel.
    #[default]
    All,
    /// Only panels in this slot.
    Only(PanelLayoutSlot),
}

impl PanelSlotFilter {
    /// The domain filter this maps to (`None` = unfiltered).
    #[must_use]
    pub fn slot(self) -> Option<PanelLayoutSlot> {
        match self {
            Self::All => None,
            Self::Only(slot) => Some(slot),
        }
    }

    /// Parses a raw slot name into a filter; the only place an unknown slot
    /// name surfaces to the caller.
    ///
    /// # Errors
    /// [`coxagent_domain::UnknownSlot`] when `raw` names no known slot.
    pub fn parse(raw: &str) -> Result<Self, coxagent_domain::UnknownSlot<'_>> {
        PanelLayoutSlot::parse(raw).map(Self::Only)
    }
}

/// The use case entry point: list the registry's panels, optionally filtered
/// to one slot, in deterministic order (slot rank, then declared order) with
/// total counts and a defined empty result.
#[must_use = "list_panels is a read; dropping the listing discards the answer"]
pub fn list_panels(registry: &PanelRegistry, filter: PanelSlotFilter) -> PanelListing {
    registry.list(filter.slot())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::use_cases::list_panels::PanelSlotFilter::{All, Only};

    fn registry() -> PanelRegistry {
        PanelRegistry::try_new(vec![
            panel(
                "overview.summary",
                "Overview",
                PanelLayoutSlot::AboveTheFold,
                0,
            ),
            panel(
                "delivery.tickets",
                "Delivery",
                PanelLayoutSlot::RowPrimary,
                0,
            ),
            panel("quality.tests", "Quality", PanelLayoutSlot::RowPrimary, 5),
        ])
        .expect("unique ids")
    }

    fn panel(
        id: &'static str,
        title: &'static str,
        slot: PanelLayoutSlot,
        order: u32,
    ) -> coxagent_domain::Panel {
        coxagent_domain::Panel::new(
            id,
            title,
            slot,
            order,
            coxagent_domain::PanelVisibility::Always,
        )
        .expect("valid panel")
    }

    #[test]
    fn all_is_the_unfiltered_listing_with_totals() {
        let listing = list_panels(&registry(), All);
        assert_eq!(listing.total, 3);
        assert_eq!(listing.registry_size, 3);
        assert_eq!(
            listing.panels.iter().map(|p| p.id).collect::<Vec<_>>(),
            vec!["overview.summary", "delivery.tickets", "quality.tests"]
        );
    }

    #[test]
    fn only_limits_to_the_slot_and_keeps_the_registry_total() {
        let listing = list_panels(&registry(), Only(PanelLayoutSlot::RowPrimary));
        assert_eq!(listing.total, 2);
        assert_eq!(listing.registry_size, 3);
        assert_eq!(
            listing.panels.iter().map(|p| p.id).collect::<Vec<_>>(),
            vec!["delivery.tickets", "quality.tests"]
        );
    }

    #[test]
    fn the_empty_registry_returns_the_defined_empty_result_for_every_filter() {
        let empty = PanelRegistry::empty();
        assert_eq!(list_panels(&empty, All), PanelListing::empty());
        assert_eq!(
            list_panels(&empty, Only(PanelLayoutSlot::AboveTheFold)),
            PanelListing::empty()
        );
    }

    #[test]
    fn raw_slot_names_parse_into_filters_and_unknown_names_are_refused() {
        assert_eq!(
            PanelSlotFilter::parse("above-the-fold").expect("known slot"),
            Only(PanelLayoutSlot::AboveTheFold)
        );
        assert!(PanelSlotFilter::parse("hero").is_err());
    }
}
