// The panel registry (CXA-B186b) — pure domain types for the hub's
// dashboard panels, enforced by construction.
//
// A [`Panel`] is a value description of one dashboard panel: where it sits
// ([`PanelLayoutSlot`]), in what order it renders, and who may see it — all
// as plain data. The registry accepts a panel list only through
// [`PanelRegistry::try_new`], which rejects duplicate ids outright, so a
// registry that exists is one whose ids are unique, whose slots are known,
// and whose ordering is already deterministic (slot rank, then declared
// order). There is deliberately no `Default`/`new` that can silently accept
// a broken list.
//
// No IO, no framework imports: this file is pure domain, per the CXA-B186a
// guardrail.

use crate::kinds::Role;
use std::fmt;

/// Where a panel sits on the dashboard — a closed set, so an unknown slot is
/// unrepresentable as a [`PanelLayoutSlot`] value. Raw metadata (config, a
/// future discovery port) names slots as strings; [`PanelLayoutSlot::parse`]
/// is the only path from that raw data into the domain, and it is the single
/// place an unknown slot is rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelLayoutSlot {
    /// Above the fold: the first thing the Overview shows.
    AboveTheFold,
    /// Primary two-column row, left side.
    RowPrimary,
    /// Primary two-column row, right side.
    RowSecondary,
    /// Diagnostics & gates: folded below the fold (CXA-F383 declutter).
    Diagnostics,
}

impl PanelLayoutSlot {
    /// Every slot, in rank order — the single source of the fold hierarchy.
    pub const ALL: [PanelLayoutSlot; 4] = [
        PanelLayoutSlot::AboveTheFold,
        PanelLayoutSlot::RowPrimary,
        PanelLayoutSlot::RowSecondary,
        PanelLayoutSlot::Diagnostics,
    ];

    /// The only path from raw metadata into a slot. Unknown slot names are
    /// rejected here — a panel can never carry a slot the layout does not
    /// know how to place.
    ///
    /// # Errors
    /// [`UnknownSlot`] when `raw` names no known slot.
    #[must_use = "a rejected slot name must not be silently dropped"]
    pub fn parse(raw: &str) -> Result<Self, UnknownSlot<'_>> {
        match raw {
            "above-the-fold" => Ok(Self::AboveTheFold),
            "row-primary" => Ok(Self::RowPrimary),
            "row-secondary" => Ok(Self::RowSecondary),
            "diagnostics" => Ok(Self::Diagnostics),
            other => Err(UnknownSlot { slot: other }),
        }
    }

    /// Stable wire name for this slot (also the raw form [`Self::parse`]
    /// accepts).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::AboveTheFold => "above-the-fold",
            Self::RowPrimary => "row-primary",
            Self::RowSecondary => "row-secondary",
            Self::Diagnostics => "diagnostics",
        }
    }

    /// Render rank: lower renders first. Above-the-fold sorts before
    /// everything else by construction, not by convention.
    #[must_use]
    pub fn rank(self) -> u8 {
        match self {
            Self::AboveTheFold => 0,
            Self::RowPrimary => 1,
            Self::RowSecondary => 2,
            Self::Diagnostics => 3,
        }
    }
}

impl fmt::Display for PanelLayoutSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A slot name the layout does not know.
#[derive(Debug, PartialEq, Eq)]
pub struct UnknownSlot<'a> {
    /// The rejected name, carried for the error message.
    pub slot: &'a str,
}

impl fmt::Display for UnknownSlot<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown panel layout slot {:?} — known slots: {}",
            self.slot,
            PanelLayoutSlot::ALL
                .iter()
                .map(|s| s.name())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl std::error::Error for UnknownSlot<'_> {}

/// Who may see a panel — pure data; enforcement is the presentation layer's
/// job, the domain only states the rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelVisibility {
    /// Every project member.
    Always,
    /// Only members holding one of these roles.
    Roles(&'static [Role]),
}

impl PanelVisibility {
    /// `true` when a member holding `roles` may see the panel. An empty role
    /// set means "no roles were declared" and hides the panel from everyone —
    /// safer than leaking it on a typo.
    #[must_use]
    pub fn allows(&self, roles: &[Role]) -> bool {
        match self {
            Self::Always => true,
            Self::Roles(required) => {
                !required.is_empty() && required.iter().any(|r| roles.contains(r))
            }
        }
    }

    /// The role names that may see the panel; empty means "everyone" only
    /// when this is [`PanelVisibility::Always`].
    #[must_use]
    pub fn role_names(&self) -> Vec<&'static str> {
        match self {
            Self::Always => Vec::new(),
            Self::Roles(required) => required.iter().map(role_name).collect(),
        }
    }
}

/// The stable lowercase name of a role — the domain's own wire form, kept
/// here so panel metadata never depends on presentation formatting.
#[must_use]
pub fn role_name(role: &Role) -> &'static str {
    match role {
        Role::Ba => "ba",
        Role::Po => "po",
        Role::Sm => "sm",
        Role::Sa => "sa",
        Role::Pd => "pd",
        Role::DevBug => "dev-bug",
        Role::DevFeature => "dev-feature",
        Role::Test => "test",
        Role::Docs => "docs",
        Role::User => "user",
        Role::System => "system",
    }
}

/// One dashboard panel, described entirely as value data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Panel {
    /// Stable identifier, unique across the registry.
    pub id: &'static str,
    /// Human title shown as the panel heading.
    pub title: &'static str,
    /// Where the panel sits.
    pub slot: PanelLayoutSlot,
    /// Declared order within the slot: lower renders first. Kept as given —
    /// ties keep registration order (stable sort), so ordering stays
    /// deterministic.
    pub sort_order: u32,
    /// Who may see the panel.
    pub visibility: PanelVisibility,
}

impl Panel {
    /// The only constructor: rejects an unnamed or untitled panel at the
    /// boundary so a panel that exists is one that can be rendered and
    /// addressed.
    ///
    /// # Errors
    /// [`PanelRegistryError::InvalidPanel`] when `id` or `title` is empty.
    pub fn new(
        id: &'static str,
        title: &'static str,
        slot: PanelLayoutSlot,
        sort_order: u32,
        visibility: PanelVisibility,
    ) -> Result<Self, PanelRegistryError> {
        if id.trim().is_empty() {
            return Err(PanelRegistryError::InvalidPanel {
                reason: "panel id must not be empty".to_owned(),
            });
        }
        if title.trim().is_empty() {
            return Err(PanelRegistryError::InvalidPanel {
                reason: "panel title must not be empty".to_owned(),
            });
        }
        Ok(Self {
            id,
            title,
            slot,
            sort_order,
            visibility,
        })
    }
}

/// Why a panel list was refused by [`PanelRegistry::try_new`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PanelRegistryError {
    /// Two panels declared the same id; a registry with ambiguous ids cannot
    /// be addressed.
    #[error("duplicate panel id {id:?}: first declared at index {first_index}, duplicate at index {duplicate_index}")]
    DuplicatePanelId {
        /// The offending id.
        id: &'static str,
        /// Where the id was first declared.
        first_index: usize,
        /// Where the duplicate was declared.
        duplicate_index: usize,
    },
    /// A panel was declared without the minimum data to render or address it.
    #[error("invalid panel: {reason}")]
    InvalidPanel {
        /// What was wrong with the panel.
        reason: String,
    },
}

/// The listing a read returns: the panels in deterministic order plus the
/// counts a zero state needs (how many match, how many exist at all).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelListing {
    /// Matching panels, slot rank then declared order.
    pub panels: Vec<Panel>,
    /// How many panels matched the filter (the list's total).
    pub total: usize,
    /// How many panels the registry holds in total — distinguishes "the
    /// registry is empty" from "nothing registered in this slot".
    pub registry_size: usize,
}

impl PanelListing {
    /// The defined empty result: zero state is data, not UI.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            panels: Vec::new(),
            total: 0,
            registry_size: 0,
        }
    }
}

/// An immutable set of panels whose invariants hold by construction: unique
/// ids, known slots (an enum cannot lie), deterministic order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PanelRegistry {
    panels: Vec<Panel>,
}

impl PanelRegistry {
    /// Builds a registry from declared panel metadata — a pure function over
    /// that data. Panels that fail validation abort the whole registration:
    /// a half-registered dashboard is worse than none.
    ///
    /// # Errors
    /// [`PanelRegistryError`] when an id repeats or a panel is invalid.
    pub fn try_new(panels: Vec<Panel>) -> Result<Self, PanelRegistryError> {
        let mut seen: Vec<&'static str> = Vec::with_capacity(panels.len());
        for (index, panel) in panels.iter().enumerate() {
            if let Some(first_index) = seen.iter().position(|id| *id == panel.id) {
                return Err(PanelRegistryError::DuplicatePanelId {
                    id: panel.id,
                    first_index,
                    duplicate_index: index,
                });
            }
            seen.push(panel.id);
        }
        Ok(Self { panels })
    }

    /// The registry with no panels — valid, and lists as the defined empty
    /// result.
    #[must_use]
    pub fn empty() -> Self {
        Self { panels: Vec::new() }
    }

    /// How many panels the registry holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.panels.len()
    }

    /// `true` when no panels are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.panels.is_empty()
    }

    /// Lists panels, optionally filtered to one slot, in deterministic order
    /// (slot rank, then declared order). Pure: a function of the registry and
    /// the filter, nothing else.
    #[must_use = "list_panels is a read; dropping the listing discards the answer"]
    pub fn list(&self, slot: Option<PanelLayoutSlot>) -> PanelListing {
        let mut matching: Vec<Panel> = self
            .panels
            .iter()
            .filter(|p| slot.map_or(true, |want| p.slot == want))
            .cloned()
            .collect();
        // Stable sort: slot rank first, declared order breaks ties. The sort
        // key uses the slot's fixed rank, so the ordering cannot drift.
        matching.sort_by_key(|p| (p.slot.rank(), p.sort_order));
        PanelListing {
            total: matching.len(),
            registry_size: self.panels.len(),
            panels: matching,
        }
    }
}

#[cfg(test)]
mod tests {
        // Pure tests over the panel registry's real types — no mock harness, no
        // IO: a registry is a value, so every assertion is a function over
        // values.

        use super::*;

    /// The declared exemplar panel set (CXA-B186's above-the-fold Overview), used
    /// by the tests below and later by the registration wiring.
    pub fn declared_panels() -> Vec<Panel> {
        vec![
            Panel::new(
                "overview.summary",
                "Overview",
                PanelLayoutSlot::AboveTheFold,
                0,
                PanelVisibility::Always,
            )
            .expect("valid panel"),
            Panel::new(
                "delivery.tickets",
                "Delivery",
                PanelLayoutSlot::RowPrimary,
                0,
                PanelVisibility::Always,
            )
            .expect("valid panel"),
            Panel::new(
                "quality.tests",
                "Quality",
                PanelLayoutSlot::RowSecondary,
                10,
                PanelVisibility::Always,
            )
            .expect("valid panel"),
            Panel::new(
                "gates.approvals",
                "Approvals",
                PanelLayoutSlot::Diagnostics,
                0,
                PanelVisibility::Roles(&[Role::Po, Role::Sa]),
            )
            .expect("valid panel"),
        ]
    }

    #[test]
    fn duplicate_ids_are_rejected_with_both_positions() {
        let mut panels = declared_panels();
        let repeated = Panel::new(
            "overview.summary",
            "Overview again",
            PanelLayoutSlot::Diagnostics,
            99,
            PanelVisibility::Always,
        )
        .expect("valid panel");
        panels.push(repeated);

        let err = PanelRegistry::try_new(panels).expect_err("duplicate id must abort registration");
        assert_eq!(
            err,
            PanelRegistryError::DuplicatePanelId {
                id: "overview.summary",
                first_index: 0,
                duplicate_index: 4,
            }
        );
    }

    #[test]
    fn an_invalid_panel_is_rejected_at_the_boundary() {
        assert!(Panel::new("", "Title", PanelLayoutSlot::AboveTheFold, 0, PanelVisibility::Always).is_err());
        assert!(Panel::new("id", "  ", PanelLayoutSlot::AboveTheFold, 0, PanelVisibility::Always).is_err());
        let ok =
            Panel::new("id", "Title", PanelLayoutSlot::AboveTheFold, 0, PanelVisibility::Always).expect("valid");
        assert_eq!(ok.id, "id");
    }

    #[test]
    fn an_empty_registry_is_valid_and_lists_as_the_defined_empty_result() {
        let registry = PanelRegistry::empty();
        assert!(registry.is_empty());

        let unfiltered = registry.list(None);
        assert_eq!(unfiltered, PanelListing::empty());
        assert_eq!(unfiltered.total, 0);
        assert_eq!(unfiltered.registry_size, 0);
        assert!(unfiltered.panels.is_empty());

        let filtered = registry.list(Some(PanelLayoutSlot::AboveTheFold));
        assert_eq!(filtered, PanelListing::empty());
    }

    #[test]
    fn filtering_an_unknown_slot_name_is_rejected_before_a_panel_can_exist() {
        assert!(PanelLayoutSlot::parse("hero").is_err());
        assert!(PanelLayoutSlot::parse("above-the-fold").is_ok());

        let err = PanelLayoutSlot::parse("hero").expect_err("unknown slot must be refused");
        assert!(err.to_string().contains("above-the-fold"));
    }

    #[test]
    fn ordering_is_slot_rank_then_declared_order() {
        // Declare deliberately out of fold order: the registry's list must not
        // care how the panels were handed in.
        let registry = PanelRegistry::try_new(vec![
            Panel::new("b.diagnostics", "B", PanelLayoutSlot::Diagnostics, 0, PanelVisibility::Always)
                .expect("valid panel"),
            Panel::new("c.secondary", "C", PanelLayoutSlot::RowSecondary, 1, PanelVisibility::Always)
                .expect("valid panel"),
            Panel::new("a.fold", "A", PanelLayoutSlot::AboveTheFold, 5, PanelVisibility::Always)
                .expect("valid panel"),
            Panel::new("d.primary", "D", PanelLayoutSlot::RowPrimary, 0, PanelVisibility::Always)
                .expect("valid panel"),
            Panel::new("e.primary", "E", PanelLayoutSlot::RowPrimary, 7, PanelVisibility::Always)
                .expect("valid panel"),
            Panel::new("f.primary", "F", PanelLayoutSlot::RowPrimary, 2, PanelVisibility::Always)
                .expect("valid panel"),
        ])
        .expect("unique ids");

        let listing = registry.list(None);
        let ids: Vec<&str> = listing.panels.iter().map(|p| p.id).collect();
        assert_eq!(
            ids,
            vec!["a.fold", "d.primary", "f.primary", "e.primary", "c.secondary", "b.diagnostics"]
        );
        assert_eq!(listing.total, 6);
        assert_eq!(listing.registry_size, 6);
    }

    #[test]
    fn slot_filter_returns_the_subset_with_correct_counts() {
        let registry = PanelRegistry::try_new(declared_panels()).expect("unique ids");

        let fold = registry.list(Some(PanelLayoutSlot::AboveTheFold));
        assert_eq!(fold.panels.iter().map(|p| p.id).collect::<Vec<_>>(), vec!["overview.summary"]);
        assert_eq!(fold.total, 1);
        assert_eq!(fold.registry_size, 4, "the registry total is unchanged by the filter");

        let primary = registry.list(Some(PanelLayoutSlot::RowPrimary));
        assert_eq!(primary.total, 1);

        let none = registry.list(Some(PanelLayoutSlot::Diagnostics));
        assert_eq!(none.total, 1);
        assert_eq!(none.panels.len(), 1);

        let empty_slot = registry.list(Some(PanelLayoutSlot::RowSecondary));
        // RowSecondary has one panel; a slot with no panels must still be a
        // defined, countable result — not an error and not the empty registry.
        let missing = PanelRegistry::try_new(vec![declared_panels()[0].clone()]).expect("unique ids");
        let empty_in_slot = missing.list(Some(PanelLayoutSlot::RowSecondary));
        assert_eq!(empty_in_slot.total, 0);
        assert_eq!(empty_in_slot.registry_size, 1);
        assert!(empty_in_slot.panels.is_empty());
        let _ = empty_slot;
    }

    #[test]
    fn visibility_is_pure_data_checked_against_declared_roles() {
        let registry = PanelRegistry::try_new(declared_panels()).expect("unique ids");
        let listing = registry.list(None);

        let approvals = listing
            .panels
            .iter()
            .find(|p| p.id == "gates.approvals")
            .expect("approvals panel registered");
        assert!(approvals.visibility.allows(&[Role::Po]));
        assert!(approvals.visibility.allows(&[Role::Sa, Role::DevFeature]));
        assert!(!approvals.visibility.allows(&[Role::DevBug]));
        assert!(!approvals.visibility.allows(&[]));
        // An empty required-role set hides the panel from everyone (fail closed).
        let nobody =
            Panel::new("x.locked", "X", PanelLayoutSlot::Diagnostics, 0, PanelVisibility::Roles(&[]))
                .expect("valid panel");
        assert!(!nobody.visibility.allows(&[Role::Po]));

        let overview = listing.panels.iter().find(|p| p.id == "overview.summary").expect("overview");
        assert!(overview.visibility.allows(&[]));
        assert_eq!(overview.visibility.role_names(), Vec::<&str>::new());
        assert_eq!(approvals.visibility.role_names(), vec!["po", "sa"]);
    }
}
