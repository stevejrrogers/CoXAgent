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
//!   [`FillHint`]).
//!
//! Purity contract for the whole module: std-only, zero IO, zero external
//! crates; constructors are pure functions returning `Result` on invariant
//! violation; only std traits are derived (`Debug`, `Clone`, `PartialEq`,
//! `Eq`, `Hash`) plus `Display`. Serialization is deliberately deferred to
//! part 3. The guard tests in `tests/terminal_state_contract_b252.rs` keep
//! the module that pure.

pub mod core;
pub mod failure;

pub use core::TerminalState;
pub use failure::{FailureReason, FillHint};
