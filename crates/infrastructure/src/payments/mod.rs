//! Payments adapters (CXA-F451) — two implementations of
//! `application::ports::outbound::PaymentsPort`:
//!
//! - `stub`: deterministic in-process fake for the PR-CI suite. No network,
//!   no credentials, controllable latency and scripted fault sequences.
//! - `sandbox_client`: live-sandbox HTTP client with token caching,
//!   exponential backoff + jitter honouring `retry_after`, and a hard
//!   rate-limit ceiling. Runs only from the nightly workflow.

pub mod sandbox_client;
pub mod stub;
