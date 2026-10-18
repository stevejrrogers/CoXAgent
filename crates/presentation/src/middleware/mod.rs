//! Inbound-adapter middleware for the axum HTTP server.
//!
//! Three independent layers are provided:
//!
//! * **`cors`** — env-var driven CORS allowlist via `tower_http::cors::CorsLayer`.
//! * **`rate_limit`** — per-IP sliding-window limiter applied to `/api/auth/` routes.
//! * **`telemetry`** — per-request metrics (counter + latency histogram)
//!   recorded into the application-layer [`coxagent_application::MetricsRegistry`].

pub mod cors;
pub mod rate_limit;
pub mod telemetry;

pub use cors::cors_layer;
pub use rate_limit::{
    auth_rate_max, auth_rate_window, rate_limit_mw, RateLimiter, AUTH_RATE_MAX, AUTH_RATE_WINDOW,
};
pub use telemetry::telemetry_mw;
