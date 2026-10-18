//! CXA-F350 — a member's PERSONAL bearer token inherits the minting member's
//! project memberships, resolved LIVE from the account on every bearer
//! resolution (never a mint-time snapshot): unassigning a project or deleting
//! the owner's account revokes the token's project reach on the very next
//! call, with no re-mint. Service tokens minted by an admin (owner '') keep
//! resolving with no projects — the owner column is the only input, so a
//! personal-looking LABEL on a service token grants nothing.
//!
//! Runs against its own ephemeral Postgres claimed from the shared compose
//! fixture (`common::TestDb`, CXA-F327) — no exported DSN is honored, an
//! unprovisionable database fails red naming the fixture, and a docker-less
//! environment skips explicitly.
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

mod common;
use coxagent_application::auth::{AuthPort, AuthRole};
use coxagent_infrastructure::SqlAuthService;

/// Unique per run so repeated runs against the same database stay clean.
fn run_unique(name: &str) -> String {
    format!("{name}-{}", std::process::id())
}

#[tokio::test]
async fn a_personal_token_resolves_its_owners_projects_like_the_account_does() {
    // `None` is the docker-absent explicit skip — the only lawful green
    // non-run, with its reason already printed by the fixture.
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let svc = SqlAuthService::connect(&db.dsn())
        .await
        .expect("connect + migrate");

    let owner = run_unique("f350-member");
    assert!(
        svc.create_user(&owner, "ChangeMe12345!", AuthRole::TechLead)
            .await
    );
    assert!(svc.assign_project(&owner, "proj-a").await);
    assert!(svc.assign_project(&owner, "proj-b").await);

    let secret = svc
        .create_token_for(
            &format!("user:{owner}:remote-store"),
            AuthRole::TechLead,
            Some(&owner),
        )
        .await
        .expect("mint the personal token");

    let principal = svc.principal_for_bearer(&secret).await.expect("resolve");
    // The token's own identity and role are unchanged — only projects are
    // inherited (the account's, in assignment order).
    assert_eq!(principal.username, format!("svc:user:{owner}:remote-store"));
    assert_eq!(principal.role, AuthRole::TechLead);
    assert_eq!(principal.projects, vec!["proj-a", "proj-b"]);
    // Session/bearer parity: the bearer carries exactly what the member's
    // own account resolution carries.
    let account = svc
        .list_users()
        .await
        .into_iter()
        .find(|u| u.username == owner)
        .expect("the member exists");
    assert_eq!(principal.projects, account.projects);
}

#[tokio::test]
async fn unassigning_a_project_revokes_the_token_on_the_next_call_without_a_re_mint() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let svc = SqlAuthService::connect(&db.dsn())
        .await
        .expect("connect + migrate");

    let owner = run_unique("f350-revoke");
    assert!(
        svc.create_user(&owner, "ChangeMe12345!", AuthRole::Manager)
            .await
    );
    assert!(svc.assign_project(&owner, "proj-a").await);
    assert!(svc.assign_project(&owner, "proj-b").await);
    let secret = svc
        .create_token_for(
            &format!("user:{owner}:remote-store"),
            AuthRole::Manager,
            Some(&owner),
        )
        .await
        .expect("mint the personal token");

    // Live resolution: the SAME secret reflects the membership change with
    // no re-mint — revoking a membership revokes the token's reach.
    assert!(svc.unassign_project(&owner, "proj-a").await);
    let after = svc
        .principal_for_bearer(&secret)
        .await
        .expect("still valid");
    assert_eq!(after.projects, vec!["proj-b"]);
    assert!(
        !after.projects.contains(&"proj-a".to_owned()),
        "the unassigned project must be gone from the token's reach"
    );
}

#[tokio::test]
async fn deleting_the_owner_fails_closed_to_no_project_access() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let svc = SqlAuthService::connect(&db.dsn())
        .await
        .expect("connect + migrate");

    let owner = run_unique("f350-deleted");
    assert!(
        svc.create_user(&owner, "ChangeMe12345!", AuthRole::TechLead)
            .await
    );
    assert!(svc.assign_project(&owner, "proj-a").await);
    let secret = svc
        .create_token_for(
            &format!("user:{owner}:remote-store"),
            AuthRole::TechLead,
            Some(&owner),
        )
        .await
        .expect("mint the personal token");

    assert!(svc.delete_user(&owner).await);
    // The token still resolves (so the gate can answer with a principal)
    // but carries NO projects — a deleted member's token keeps no ghost
    // memberships, so every per-project route refuses it.
    let orphan = svc.principal_for_bearer(&secret).await.expect("resolve");
    assert!(
        orphan.projects.is_empty(),
        "a deleted owner's token must fail closed: {:?}",
        orphan.projects
    );
}

#[tokio::test]
async fn a_service_token_keeps_no_projects_even_with_a_personal_looking_label() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let svc = SqlAuthService::connect(&db.dsn())
        .await
        .expect("connect + migrate");

    let owner = run_unique("f350-service");
    assert!(
        svc.create_user(&owner, "ChangeMe12345!", AuthRole::TechLead)
            .await
    );
    assert!(svc.assign_project(&owner, "proj-a").await);

    // Admin-minted service token (create_token — owner '') carrying a label
    // that LOOKS like someone's personal token: the owner column is the only
    // input, so label-prefix spoofing grants nothing.
    let service = svc
        .create_token(&format!("user:{owner}:spoof"), AuthRole::TechLead)
        .await
        .expect("mint the service token");
    let principal = svc
        .principal_for_bearer(&service)
        .await
        .expect("resolve service token");
    assert!(
        principal.projects.is_empty(),
        "a service token must stay hub-wide-for-role only: {:?}",
        principal.projects
    );
}
