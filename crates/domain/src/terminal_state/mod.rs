//! Per-panel terminal-state contract — the shared, framework-free core value
//! types (CXA-B252, subtask 1/3 of the CXA-B195b split of CXA-B229).
//!
//! The B195 lineage so far froze the above-the-fold panel enumeration
//! (CXA-B195a, [`crate::overview_panel`]) and the terminal-state variant set
//! with the shared component contract in presentation
//! (`crates/presentation/src/overview_panels.rs`); the parent CXA-B229
//! composes the per-panel terminal states on top. This module is that
//! composition's FOUNDATION: the pure value types, lifted into the domain
//! crate so every later part and every surface composes on the same
//! framework-free core.
//!
//! One file per cohesive unit:
//! * [`core`] — the [`TerminalState`] enum (the five terminal states part 1
//!   froze, payloads moved into the variants) and its pure semantics;
//! * [`failure`] — the validated payload value objects ([`FailureReason`],
//!   [`FillHint`]);
//! * [`panels`] — the per-panel terminal states (CXA-B253, part 2/3): one
//!   file per panel, each embedding the core enum plus that panel's payload,
//!   under the shared [`panels::PanelTerminalState`] contract.
//!
//! Purity contract for the whole module: std-only at runtime, zero IO; the
//! ONLY external-crate touchpoints are `serde` derive attributes applied to
//! the value types following the codebase's existing serde-derive convention
//! (serialization, CXA-B254 part 3/3). Constructors remain pure functions
//! returning `Result` on invariant violation. The guard tests in
//! `tests/terminal_state_contract_b252.rs` keep the module that pure.

pub mod core;
pub mod failure;
pub mod panels;

pub use core::TerminalState;
pub use failure::{FailureReason, FillHint};
