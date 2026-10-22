//! Per-panel terminal-state value types (CXA-B253, part 2/3 of the
//! CXA-B195b split of CXA-B229) — ONE FILE PER PANEL, each composing the
//! pure core enum from CXA-B252 with that panel's own payload value objects.
//!
//! The authoritative panel set is NOT enumerated from memory: each module
//! cites the exact selector + code line from the CXA-B228 evidence-backed
//! enumeration (`crates/domain/src/overview_panel.rs`) that freezes it. No
//! wiring — nothing renders these types yet (part 3 owns that).

pub mod activity_feed;
pub mod alert_strip;
pub mod contract;
pub mod drain_banner;
pub mod kpi_strip;
pub mod releases_feed;
pub mod team_health;
pub mod working_agents;

pub use activity_feed::{ActivityEntry, ActivityFeedTerminalState};
pub use alert_strip::{AlertStripTerminalState, OperatorAlert};
pub use contract::PanelTerminalState;
pub use drain_banner::{DrainBannerTerminalState, GreenPrCount};
pub use kpi_strip::{KpiStripTerminalState, KpiTile};
pub use releases_feed::{ReleaseEntry, ReleasesFeedTerminalState};
pub use team_health::{AgentHealthCard, TeamHealthTerminalState};
pub use working_agents::{BusyAgent, WorkingAgentsTerminalState};
