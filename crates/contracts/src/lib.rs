//! Wire contracts shared between CoXAgent services (gateway, runner, realtime,
//! knowledge). Everything crossing a service boundary is defined HERE, versioned
//! and serde-stable — services never share in-memory types across processes.
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Version tag stamped on every envelope so a rolling deploy can detect and
/// skip frames from an incompatible peer instead of misparsing them.
pub const CONTRACT_VERSION: u16 = 1;

/// One event on the hub-wide bus (Redis pub/sub channel `cox:events`).
/// `origin` is the emitting instance id so subscribers can drop their own
/// echoes; `payload` is the JSON the realtime layer fans out to clients.
///
/// `PartialEq` backs the wire round-trip contract test (`round_trip` module).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusEnvelope {
    pub v: u16,
    pub origin: String,
    /// Logical stream: "syschat" | "presence" | "project:<pid>" …
    pub stream: String,
    pub payload: serde_json::Value,
}

impl BusEnvelope {
    #[must_use]
    pub fn new(origin: &str, stream: &str, payload: serde_json::Value) -> Self {
        Self {
            v: CONTRACT_VERSION,
            origin: origin.to_owned(),
            stream: stream.to_owned(),
            payload,
        }
    }
}

/// A queued unit of execution the gateway hands to a runner (Postgres-backed
/// today; the shape is transport-agnostic so a queue can replace it).
///
/// `PartialEq` backs the wire round-trip contract test (`round_trip` module).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSpec {
    pub v: u16,
    pub project: String,
    /// "cycle" | "force_merge" | "terminal" …
    pub kind: String,
    #[serde(default)]
    pub args: serde_json::Value,
}

pub mod round_trip;

/// Reusable serde wire-contract testing harness (see `round_trip` module docs).
///
/// Public so every consumer crate's test suite can pin its DTOs with the same
/// one-call proof instead of hand-rolled per-type blocks. Test-only in spirit —
/// it performs no IO and links nothing beyond `serde`/`serde_json`.
pub use round_trip::{assert_symmetric_round_trip, assert_wire_shape, SymmetrySuite};
