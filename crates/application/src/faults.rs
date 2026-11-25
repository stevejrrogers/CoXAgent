//! Single source of truth for "was that an INFRASTRUCTURE fault?".
//!
//! Revoked auth, quota walls, rate limits and network outages are not any
//! ticket's fault: counting them as ticket failures parked innocent tickets
//! during a real 401 outage, and a network drop would do the same. Neither
//! are the engine's own deaths (CXA-F370): the adapters' wall-clock kill
//! (`claude/opencode/copilot timed out`), a dead provider's `UnknownError`
//! error event, and an `unavailable` outage line say the ENGINE failed, not
//! the work — burning a ticket's 3 fail-attempts on them auto-held good
//! tickets (F341, F354, B159) that a human then had to un-hold by hand.
//! Both the DEV failure counter and the runner's circuit breaker consult
//! this one predicate so the two can never drift apart.

/// An AUTH-class death: revoked/expired credentials. Unlike a transient blip
/// this never heals on its own — a person must re-login — so the alarm fires
/// on the FIRST sighting instead of waiting for the breaker to count cycles
/// (the 2026-08-17 OAuth death burned an hour before anything shouted).
#[must_use]
pub fn is_auth_death(why: &str) -> bool {
    let low = why.to_lowercase();
    [
        "oauth",
        "authenticate",
        "401",
        "unauthorized",
        "revoked",
        "invalid api key",
        "api key not",
    ]
    .iter()
    .any(|m| low.contains(m))
}

/// Whether `why` looks like an infrastructure fault rather than a genuine
/// task failure. An EMPTY message is treated as infra too: every observed
/// engine-side outage (401, network drop) surfaced with empty stderr, while
/// real task failures carry compiler/test output.
#[must_use]
pub fn is_infra_fault(why: &str) -> bool {
    if why.trim().is_empty() {
        return true;
    }
    let low = why.to_lowercase();
    [
        // Auth / account.
        "401",
        "authenticate",
        "revoked",
        "unauthorized",
        // Capacity.
        "quota",
        // The Claude CLI's monthly-cap message carries neither "quota" nor
        // "rate limit" — it slipped past this list once and 39 innocent
        // tickets burned three attempts each and were parked in minutes.
        "spend limit",
        "usage limit",
        "spending cap",
        "rate limit",
        "overloaded",
        "529",
        // Network.
        "connection refused",
        "connection reset",
        "connection closed",
        "econnrefused",
        "econnreset",
        "etimedout",
        "enotfound",
        "dns",
        "network is unreachable",
        "network error",
        "fetch failed",
        "socket hang up",
        "no route to host",
        "temporary failure in name resolution",
        // The host OS, not the agent: macOS Seatbelt refuses `sandbox_apply()`
        // in bursts (COX-B013/COX-B016), so `sandbox-exec` exits 71 before the
        // agent CLI ever runs and the only trace is this stderr line. Without
        // it the ticket is charged for a failure whose work never started.
        "sandbox_apply",
        // The engine itself died (CXA-F370): the adapters' wall-clock kill
        // surfaces as `PortError::Backend("<cli> timed out")` (opencode.rs,
        // claude.rs, copilot.rs), a dead provider reaches the outcome's
        // stderr as opencode's error event (`UnknownError: Unexpected server
        // error…`), and an outage answers `service unavailable`. None of
        // these is the ticket's work failing — the run never produced a
        // verdict to grade.
        "timed out",
        "unknownerror",
        "unavailable",
    ]
    .iter()
    .any(|p| low.contains(p))
}

#[cfg(test)]
mod tests {
    use super::is_infra_fault;

    #[test]
    fn auth_capacity_network_and_engine_deaths_are_infra() {
        for why in [
            "",
            "   ",
            "API Error: 401 OAuth access token has been revoked.",
            "quota exceeded, retry later",
            "error sending request: connection refused (os error 61)",
            "getaddrinfo ENOTFOUND api.anthropic.com",
            "fetch failed: network error",
            "upstream overloaded (529)",
            // The Claude CLI's monthly-cap message, verbatim — it parked 39
            // tickets before this entry existed.
            "You've hit your monthly spend limit · raise it at claude.ai/settings/usage",
            // The exact line the claude CLI prints on an expired session —
            // it reaches us through stdout, not stderr.
            "PO milestones engine failed: Failed to authenticate: OAuth session expired and \
             could not be refreshed",
            // COX-B016: macOS refused to apply the Seatbelt profile, so the
            // agent never ran. Verbatim from `sandbox-exec` on this repo's own
            // dev hosts, where it hits ~40% of runs in bursts.
            "sandbox-exec: sandbox_apply: Operation not permitted",
            // CXA-F370 — the engine's own deaths, verbatim from the adapters:
            // the wall-clock kill as `PortError::Backend`'s Display renders it,
            // a dead provider's error event folded into stderr, and an outage
            // line. None of them graded the ticket's work.
            "backend failure: opencode timed out",
            "claude timed out",
            "opencode UnknownError: Unexpected server error while generating",
            "service unavailable",
        ] {
            assert!(is_infra_fault(why), "{why:?} must be infra");
        }
    }

    #[test]
    fn genuine_task_failures_are_not_infra() {
        for why in [
            "error[E0308]: mismatched types",
            "test result: FAILED. 3 passed; 1 failed",
            "left tests red on COX-B009",
            "added clippy errors (37 -> 40)",
        ] {
            assert!(!is_infra_fault(why), "{why:?} must NOT be infra");
        }
    }
}
