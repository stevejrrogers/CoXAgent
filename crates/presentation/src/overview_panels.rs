//! Typed source of truth for the Overview screen's above-the-fold panels
//! (CXA-B195, subtask 1/3 of CXA-B173a).
//!
//! The web Overview (`src/web/index.html`, `view-overview`) deliberately keeps
//! only "does the project need me, and is the team moving?" above the fold —
//! everything diagnostic lives inside the folded `<details id="ov-diag">`
//! block (CXA-F383 declutter). This module freezes that above-the-fold set as
//! data so later subtasks can consume it unchanged:
//!
//! * [`OVERVIEW_PANELS`] — one entry per above-the-fold panel: stable id, the
//!   DOM id it paints into, a one-line purpose, the data dependency feeding it
//!   (the endpoint of the port it consumes) and the full state set it can
//!   reach.
//! * [`TerminalState`] / [`Slot`] / [`required_slots`] — the public contract of
//!   the ONE shared terminal-state component that CXA-B173b will implement and
//!   CXA-B173c will wire. Types and doc comments only: zero implementation
//!   body, zero rendering behaviour, so nothing here can drift from what the
//!   component will actually be asked to do.
//!
//! The guard test `tests/overview_panel_manifest_b195.rs` pins this manifest
//! against the panels really rendered above the fold, so the enumeration
//! cannot silently drift.
//!
//! This module is presentation-layer truth about the *web dashboard*, the same
//! bytes the hub serves (see `server/assets.rs`); it performs no IO.

/// Stable identity of an above-the-fold Overview panel.
///
/// The discriminants are the contract; never match on the display string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OverviewPanelId {
    /// Clean-base drain banner — refactor-mode pause notice.
    Drain,
    /// Operator alert stack — deploy failure, budget cap, open bugs, reverts.
    Alerts,
    /// Live busy-agent strip — who is working on what right now.
    Working,
    /// KPI tiles with 14-day sparklines and signed deltas.
    Kpis,
    /// Team health cards — velocity, WIP, open bugs, PR reject rate.
    Health,
    /// Recent activity feed — last agent actions.
    RecentActivity,
    /// Releases panel — shipped changelog derived from deploy history.
    Releases,
}

impl std::fmt::Display for OverviewPanelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Drain => "drain",
            Self::Alerts => "alerts",
            Self::Working => "working",
            Self::Kpis => "kpis",
            Self::Health => "health",
            Self::RecentActivity => "recent-activity",
            Self::Releases => "releases",
        };
        f.write_str(name)
    }
}

/// Every state an above-the-fold panel can be observed in.
///
/// The shared terminal-state component (CXA-B173b) renders exactly these; the
/// first four are the common states every panel shares, the last two are the
/// panel-specific terminal states the Overview actually uses. `Loading` is the
/// only *non-terminal* state — a panel that can never leave it is the B131
/// bug class this contract exists to kill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TerminalState {
    /// Data requested, nothing to paint yet. Must always be replaceable.
    Loading,
    /// Legitimately nothing to show — paints the zero-state hint that says
    /// what would fill the panel (never a bare blank).
    Empty,
    /// The data dependency failed (network, upstream, parse). Must say WHY.
    Error,
    /// Normal content — the caller supplies the whole panel body.
    Ready,
    /// Panel-specific terminal state: the operator must act before work
    /// resumes (e.g. clean-base drain pause, budget cap). Says WHY + the way out.
    Attention,
    /// Panel-specific terminal state: content exists but every value is at
    /// its floor (zero history) — ready-shaped, with a zero-state hint.
    Zero,
}

impl TerminalState {
    /// `Loading` is not terminal; everything else is a state a panel may rest in.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Loading)
    }

    /// States that owe the operator a reason (attribution: why this, since when).
    #[must_use]
    pub fn requires_reason(self) -> bool {
        matches!(self, Self::Error | Self::Attention)
    }

    /// Machine-readable name — also the key B173c will use in the DOM wiring.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Loading => "loading",
            Self::Empty => "empty",
            Self::Error => "error",
            Self::Ready => "ready",
            Self::Attention => "attention",
            Self::Zero => "zero",
        }
    }
}

/// A named slot of the shared terminal-state component.
///
/// The component owns layout; the caller fills slots per state (see
/// [`required_slots`] for what each state demands and [`optional_slots`] for
/// what it accepts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    /// Icon or illustration — the visual anchor of a terminal state.
    Icon,
    /// Title — one line, noun-first ("No releases yet").
    Title,
    /// Body copy — the why / what-would-fill-this sentence.
    Body,
    /// Primary action — the single forward affordance for this state.
    PrimaryAction,
    /// Secondary action — escape hatch (drill down, dismiss, configure).
    SecondaryAction,
}

/// What a state REQUIRES from the caller — the compile-checked core of the
/// component contract.
///
/// Contract summary for CXA-B173b/B173c:
///
/// * `Loading` — title only (a spinner is the component's own affair). A
///   loading panel must never be the resting state: whoever starts the request
///   owns the transition out.
/// * `Empty` — icon + title + body; the body says what fills the panel. No
///   action is required (an optional secondary "configure" is allowed).
/// * `Error` — icon + title + body + primary action; the body is the reason
///   ([`TerminalState::requires_reason`]) and the primary action is the retry.
/// * `Ready` — no slots: the caller renders the panel body itself; the shared
///   component is only the terminal-state shell for the other states.
/// * `Attention` — icon + title + body + primary + secondary: why the operator
///   is needed, the forward action, and the escape hatch.
/// * `Zero` — icon + title + body: same shape as `Empty` but the panel is
///   healthy; the body carries the zero-state hint.
#[must_use]
pub fn required_slots(state: TerminalState) -> &'static [Slot] {
    match state {
        TerminalState::Loading => &[Slot::Title],
        TerminalState::Empty | TerminalState::Zero => &[Slot::Icon, Slot::Title, Slot::Body],
        TerminalState::Error => &[Slot::Icon, Slot::Title, Slot::Body, Slot::PrimaryAction],
        TerminalState::Ready => &[],
        TerminalState::Attention => &[
            Slot::Icon,
            Slot::Title,
            Slot::Body,
            Slot::PrimaryAction,
            Slot::SecondaryAction,
        ],
    }
}

/// What a state additionally ACCEPTS from the caller. Always a subset the
/// component renders after the required slots.
#[must_use]
pub fn optional_slots(state: TerminalState) -> &'static [Slot] {
    match state {
        TerminalState::Loading => &[Slot::Body],
        TerminalState::Empty | TerminalState::Error => &[Slot::SecondaryAction],
        TerminalState::Ready => &[Slot::Title, Slot::PrimaryAction, Slot::SecondaryAction],
        TerminalState::Attention | TerminalState::Zero => &[],
    }
}

/// Where a panel's data comes from — the port the panel consumes, expressed as
/// the hub endpoint (the web client's inbound adapter) plus the owning code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelDataDependency {
    /// One-line description of the use case / read path feeding the panel.
    pub feeds: &'static str,
    /// Hub endpoint (relative to `/api/projects/:pid`) the client fetches.
    pub endpoint: &'static str,
    /// Client module that owns the fetch + paint today (pre-B173c reality).
    pub owner: &'static str,
}

impl PanelDataDependency {
    /// Construct a dependency from the values every entry must state.
    #[must_use]
    pub const fn new(feeds: &'static str, endpoint: &'static str, owner: &'static str) -> Self {
        Self {
            feeds,
            endpoint,
            owner,
        }
    }
}

/// One above-the-fold Overview panel, fully described.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverviewPanel {
    /// Stable id — reference this, never the DOM id, in code.
    pub id: OverviewPanelId,
    /// The element id the panel paints into inside `view-overview`.
    pub dom_id: &'static str,
    /// One-line purpose — why this panel is above the fold.
    pub purpose: &'static str,
    /// The data dependency feeding it.
    pub dependency: PanelDataDependency,
    /// The full state set the panel can reach. Always includes a terminal
    /// ready/zero state; never `Loading` alone.
    pub states: &'static [TerminalState],
}

/// Shorthand for the shared state sets, kept next to the manifest for review.
const ALERT_STATES: &[TerminalState] = &[
    TerminalState::Loading,
    TerminalState::Ready,
    TerminalState::Attention,
];
const FEED_STATES: &[TerminalState] = &[
    TerminalState::Loading,
    TerminalState::Empty,
    TerminalState::Ready,
];
const WORKING_STATES: &[TerminalState] = &[TerminalState::Empty, TerminalState::Ready];
const KPI_STATES: &[TerminalState] = &[
    TerminalState::Loading,
    TerminalState::Zero,
    TerminalState::Ready,
];
const DRAIN_STATES: &[TerminalState] = &[
    TerminalState::Empty,
    TerminalState::Ready,
    TerminalState::Attention,
    TerminalState::Error,
];

/// The manifest: every above-the-fold Overview panel, in DOM order.
///
/// Frozen by CXA-B195; CXA-B173b implements the shared terminal-state
/// component against [`TerminalState`] / [`required_slots`], and CXA-B173c
/// wires exactly one of these panels end-to-end. Entries are DOM-ordered so
/// review of the manifest reads top-to-bottom like the screen.
pub const OVERVIEW_PANELS: &[OverviewPanel] = &[
    OverviewPanel {
        id: OverviewPanelId::Drain,
        dom_id: "ov-drain",
        purpose: "Clean-base drain banner — in refactor mode all agents pause new work until the open green PRs are merged.",
        dependency: PanelDataDependency::new(
            "open merge queue + refactor-goal flag",
            "/prs",
            "shell.js drainBanner()",
        ),
        states: DRAIN_STATES,
    },
    OverviewPanel {
        id: OverviewPanelId::Alerts,
        dom_id: "ov-alerts",
        purpose: "Operator alert stack — last deployment failed, budget cap reached, open bugs piling up, reverted work awaiting review.",
        dependency: PanelDataDependency::new(
            "1 Hz state snapshot: deploy, budget, tickets, reverted_work",
            "/state",
            "core.js alertsHtml() via renderActive()",
        ),
        states: ALERT_STATES,
    },
    OverviewPanel {
        id: OverviewPanelId::Working,
        dom_id: "ov-working",
        purpose: "Live busy-agent strip — each working agent, its ticket, click-through to the live log (Overview breathes while agents work).",
        dependency: PanelDataDependency::new(
            "activity trail + per-agent worker status",
            "/state · /workers",
            "core.js renderOvWorking()",
        ),
        states: WORKING_STATES,
    },
    OverviewPanel {
        id: OverviewPanelId::Kpis,
        dom_id: "kpis",
        purpose: "KPI tiles with depth — 14-day sparkline and signed delta per tile, zero-state hint where there is no history.",
        dependency: PanelDataDependency::new(
            "state history: activity, tickets, releases, cost (client-side day bucketing)",
            "/state",
            "kpis.js overviewKpis()",
        ),
        states: KPI_STATES,
    },
    OverviewPanel {
        id: OverviewPanelId::Health,
        dom_id: "ov-health",
        purpose: "Team health cards — sprint velocity, WIP, unfixed bugs, PR reject rate; each card drill-downs to its board/review tab.",
        dependency: PanelDataDependency::new(
            "state snapshot: tickets, reviews, sprint commitments",
            "/state",
            "chat.js renderHealth()",
        ),
        states: KPI_STATES,
    },
    OverviewPanel {
        id: OverviewPanelId::RecentActivity,
        dom_id: "ov-activity",
        purpose: "Recent activity — the last agent actions with 'view all' into the full activity tab.",
        dependency: PanelDataDependency::new(
            "activity trail (last 7 entries, newest first)",
            "/state",
            "core.js renderActive() activity branch",
        ),
        states: FEED_STATES,
    },
    OverviewPanel {
        id: OverviewPanelId::Releases,
        dom_id: "ov-changelog",
        purpose: "Releases — shipped changelog derived from deploy history, newest first.",
        dependency: PanelDataDependency::new(
            "deploy history (zero-token changelog)",
            "/state",
            "core.js renderActive() releases branch",
        ),
        states: FEED_STATES,
    },
];

/// Look a panel up by its stable id.
#[must_use]
pub fn panel(id: OverviewPanelId) -> Option<&'static OverviewPanel> {
    OVERVIEW_PANELS.iter().find(|p| p.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The manifest itself must be internally coherent even without the DOM.
    #[test]
    fn manifest_ids_are_unique_and_dom_ordered() {
        let ids: Vec<_> = OVERVIEW_PANELS.iter().map(|p| p.id).collect();
        let unique = ids.iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            unique.len(),
            ids.len(),
            "duplicate OverviewPanelId in the manifest"
        );
        assert_eq!(
            ids,
            vec![
                OverviewPanelId::Drain,
                OverviewPanelId::Alerts,
                OverviewPanelId::Working,
                OverviewPanelId::Kpis,
                OverviewPanelId::Health,
                OverviewPanelId::RecentActivity,
                OverviewPanelId::Releases,
            ],
            "the manifest must stay in above-the-fold DOM order"
        );
    }

    /// No entry may rest on `Loading`: every panel lists at least one terminal
    /// ready/zero state (the B131 bug class).
    #[test]
    fn every_panel_reaches_a_ready_like_terminal_state() {
        for p in OVERVIEW_PANELS {
            assert!(!p.states.is_empty(), "{}: empty state set", p.dom_id);
            assert!(
                p.states
                    .iter()
                    .any(|s| matches!(s, TerminalState::Ready | TerminalState::Zero)),
                "{}: no ready/zero terminal state",
                p.dom_id
            );
            assert!(
                p.states.iter().all(|s| s.is_terminal())
                    || p.states.contains(&TerminalState::Loading),
                "{}: state set mixes non-terminal states other than Loading",
                p.dom_id
            );
        }
    }

    /// The contract's slot requirements must be self-consistent: required and
    /// optional slots never overlap, `Ready` demands nothing, error/attention
    /// always owe a reason and an action.
    #[test]
    fn terminal_state_contract_is_self_consistent() {
        for state in [
            TerminalState::Loading,
            TerminalState::Empty,
            TerminalState::Error,
            TerminalState::Ready,
            TerminalState::Attention,
            TerminalState::Zero,
        ] {
            let req = required_slots(state);
            let opt = optional_slots(state);
            for slot in req {
                assert!(
                    !opt.contains(slot),
                    "{state:?}: {slot:?} both required and optional"
                );
            }
            assert_eq!(
                req.contains(&Slot::Body),
                state.requires_reason()
                    || matches!(state, TerminalState::Empty | TerminalState::Zero),
                "{state:?}: body-copy slot must track whether the state owes a reason or a hint"
            );
        }
        assert!(
            required_slots(TerminalState::Ready).is_empty(),
            "Ready is caller-rendered"
        );
        assert!(
            required_slots(TerminalState::Error).contains(&Slot::PrimaryAction),
            "Error needs the retry action"
        );
        assert!(required_slots(TerminalState::Attention).contains(&Slot::SecondaryAction));
    }

    /// The lookup helper stays in sync with the manifest.
    #[test]
    fn lookup_by_id_resolves_every_manifest_entry() {
        for p in OVERVIEW_PANELS {
            assert_eq!(panel(p.id).map(|found| found.dom_id), Some(p.dom_id));
        }
        assert!(panel(OverviewPanelId::Drain).is_some());
    }
}
