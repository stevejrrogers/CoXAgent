// Split from server/mod.rs — auth_mw's write-gate policy for PR review
// actions (COX-B038).
#![allow(clippy::wildcard_imports)]
use super::*;
use coxagent_application::auth::AuthRole;

/// Every action `pr_action_ep` dispatches on, as the middleware sees it.
const PR_ACTIONS: &[&str] = &[
    "merge",
    "request-changes",
    "close",
    "preview",
    "preview-stop",
    "force-merge",
];

fn pr_path(action: &str) -> String {
    format!("/api/projects/acme/prs/42/{action}")
}

/// Product/analysis/QA roles: may write project data, may NOT review a PR.
/// Per the gate map "PR thì dev và SA duyệt" these four are the ones held back;
/// developers review (below).
const MEMBER_TIER: &[AuthRole] = &[AuthRole::Ba, AuthRole::Po, AuthRole::Qa, AuthRole::Sm];

/// Roles that review PRs: Super/Admin, the lead tier, the legacy Reviewer, the
/// SA, and every developer.
const REVIEWER_TIER: &[AuthRole] = &[
    AuthRole::Super,
    AuthRole::Admin,
    AuthRole::Reviewer,
    AuthRole::Sa,
    AuthRole::Director,
    AuthRole::Manager,
    AuthRole::TechLead,
    AuthRole::DsLead,
    AuthRole::DaLead,
    AuthRole::Fe,
    AuthRole::Be,
    AuthRole::Aie,
    AuthRole::Ds,
    AuthRole::Da,
    AuthRole::De,
];

/// The ticket's repro: a Member-tier user who IS a member of the project
/// POSTs a PR action. Before the fix `write_gate_ok` resolved true for every
/// non-Viewer, so force-merge — which bypasses CI and merges without review
/// sign-off — ran with a 200.
#[test]
fn member_tier_cannot_run_any_pr_review_action() {
    for role in MEMBER_TIER {
        for action in PR_ACTIONS {
            assert!(
                !write_gate_ok(*role, &pr_path(action)),
                "{} must be refused /prs/{action} with 'insufficient role'",
                role.as_str()
            );
        }
    }
}

#[test]
fn admin_leads_and_reviewer_keep_every_pr_review_action() {
    for role in REVIEWER_TIER {
        for action in PR_ACTIONS {
            assert!(
                write_gate_ok(*role, &pr_path(action)),
                "{} must keep access to /prs/{action}",
                role.as_str()
            );
        }
    }
}

/// The gate must not become a blanket lockout: member-tier roles still write
/// everything that is not a PR review action. This is what keeps the fix from
/// over-correcting into "members can't work".
#[test]
fn member_tier_still_writes_everything_that_is_not_a_pr_action() {
    let ordinary_writes = [
        "/api/projects/acme/tickets",
        "/api/projects/acme/tickets/CXC-B001/transition",
        "/api/projects/acme/run",
        "/api/projects/acme/deploy",
    ];
    for role in MEMBER_TIER {
        for path in ordinary_writes {
            assert!(
                write_gate_ok(*role, path),
                "{} must still write {path}",
                role.as_str()
            );
        }
    }
}

/// Viewer is read-only everywhere — the pre-existing guarantee this change
/// must not disturb.
#[test]
fn viewer_is_refused_both_review_actions_and_ordinary_writes() {
    assert!(!write_gate_ok(AuthRole::Viewer, &pr_path("merge")));
    assert!(!write_gate_ok(
        AuthRole::Viewer,
        "/api/projects/acme/tickets"
    ));
}

/// Every role is on exactly one side of the PR gate, and the two sides
/// together cover `AuthRole::all()`. A newly added role therefore cannot slip
/// through unclassified — this test fails until it is placed deliberately.
#[test]
fn every_role_is_classified_by_the_pr_gate() {
    for role in AuthRole::all() {
        let allowed = write_gate_ok(*role, &pr_path("force-merge"));
        let expected = REVIEWER_TIER.contains(role);
        assert!(
            !(allowed && MEMBER_TIER.contains(role)),
            "{} is member tier but passed the PR gate",
            role.as_str()
        );
        assert_eq!(
            allowed,
            expected,
            "{} landed on the wrong side of the PR review gate",
            role.as_str()
        );
    }
}

#[test]
fn only_the_pr_route_is_treated_as_a_review_action() {
    assert!(is_pr_review_path("/api/projects/acme/prs/42/merge"));
    assert!(is_pr_review_path("/api/projects/acme/prs/42/diff"));
    // Nothing else on the project surface inherits review gating.
    assert!(!is_pr_review_path("/api/projects/acme/tickets"));
    assert!(!is_pr_review_path("/api/projects/acme/prs"));
    assert!(!is_pr_review_path("/api/health"));
}

#[test]
fn the_raw_store_rpc_is_not_an_ordinary_write() {
    use coxagent_application::auth::AuthRole;
    let store = "/api/projects/cox/store";
    assert!(is_store_rpc_path(store));
    assert!(
        !is_store_rpc_path("/api/projects/cox/store/x"),
        "only the exact RPC path gets the stricter gate"
    );
    // A member may write project data through the shaped endpoints, but a raw
    // whole-state save would let them forge any gate decision.
    for role in [AuthRole::Fe, AuthRole::Be, AuthRole::Ba, AuthRole::De] {
        assert!(role.can_write(), "{role:?} still writes normally");
        assert!(
            !write_gate_ok(role, store),
            "{role:?} must not save raw state"
        );
    }
    for role in [AuthRole::Super, AuthRole::Admin, AuthRole::TechLead] {
        assert!(write_gate_ok(role, store));
    }
    // Same bar for the runner's PR-report sibling, whose project id travels in
    // the body where the URL membership check cannot see it.
    assert!(!write_gate_ok(AuthRole::Fe, "/api/pr-report"));
    assert!(write_gate_ok(AuthRole::Admin, "/api/pr-report"));
}
