//! Single source of truth for "was that an INFRASTRUCTURE fault?".
//!
//! Revoked auth, quota walls, rate limits and network outages are not any
//! ticket's fault: counting them as ticket failures parked innocent tickets
//! during a real 401 outage, and a network drop would do the same. Both the
//! DEV failure counter and the runner's circuit breaker consult this one
//! predicate so the two can never drift apart.

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
    ]
    .iter()
    .any(|p| low.contains(p))
}

#[cfg(test)]
mod tests {
    use super::is_infra_fault;

    #[test]
    fn auth_capacity_and_network_faults_are_infra() {
        for why in [
            "",
            "   ",
            "API Error: 401 OAuth access token has been revoked.",
            "quota exceeded, retry later",
            "error sending request: connection refused (os error 61)",
            "getaddrinfo ENOTFOUND api.anthropic.com",
            "fetch failed: network error",
            "upstream overloaded (529)",
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
            // Our own engine timeout: the run genuinely took too long — the
            // ticket may simply be too big, which IS attributable.
            "claude timed out",
        ] {
            assert!(!is_infra_fault(why), "{why:?} must NOT be infra");
        }
    }
}
