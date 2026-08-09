//! Inbound-adapter middleware for the axum HTTP server.
//!
//! Two independent layers are provided:
//!
//! * **`cors`** — env-var driven CORS allowlist via `tower_http::cors::CorsLayer`.
//! * **`rate_limit`** — per-IP sliding-window limiter applied to `/api/auth/` routes.

pub mod cors;
pub mod rate_limit;

pub use cors::cors_layer;
pub use rate_limit::{rate_limit_mw, RateLimiter, AUTH_RATE_MAX, AUTH_RATE_WINDOW};
