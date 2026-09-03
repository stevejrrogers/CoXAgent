//! CXA-F002 - SqlAuthService::auto_issue_personal_token mints exactly one
//! personal bearer token per user at login time, bound to that user's OWN role
//! (never an elevation), and then goes idempotent - later calls return None
//! rather than re-minting or re-issuing the plaintext secret.
//!
//! Runs against its own ephemeral Postgres claimed from the shared compose
//! fixture (`common::TestDb`, CXA-F327) - no exported DSN is honored, an
//! unprovisionable database fails red naming the fixture, and a docker-less
//! environment skips explicitly.
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use coxagent_application::auth::{AuthPort, AuthRole, TokenInfo};

mod common;
use coxagent_infrastructure::SqlAuthService;

#[tokio::test]
async fn auto_issue_mints_once_and_stays_idempotent() {
    // `None` is the docker-absent explicit skip — the only lawful green
    // non-run, with its reason already printed by the fixture.
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let svc = SqlAuthService::connect(&db.dsn())
        .await
        .expect("connect + migrate");

    // Unique username per run so repeated runs against the same DB are clean.
    let pid = std::process::id();
    let username = format!("harvest-{pid}");
    assert!(
        svc.create_user(&username, "ChangeMe12345!", AuthRole::Admin)
            .await
    );

    // First harvest mints a secret for this user.
    let first = svc
        .auto_issue_personal_token(&username)
        .await
        .expect("first harvest must mint a token");
    assert_eq!(first.len(), 64, "minted secret must look like an API token");

    // Exactly one personal token exists for this user under the remote-store label,
    // carrying the caller's OWN role - never an elevation.
    let mine: Vec<TokenInfo> = svc
        .list_tokens()
        .await
        .into_iter()
        .filter(|t| {
            t.label
                .starts_with(&format!("user:{}:", username.to_lowercase()))
        })
        .collect();
    assert_eq!(mine.len(), 1, "must mint exactly one token for this user");
    assert_eq!(mine[0].role, AuthRole::Admin);
    assert!(
        mine[0].label.ends_with(":remote-store"),
        "label: {}",
        mine[0].label
    );

    // Idempotent: a second harvest neither re-mints nor re-issues a new secret.
    let second = svc.auto_issue_personal_token(&username).await;
    assert_eq!(second, None, "repeat harvest must not re-issue the secret");

    // Still exactly one token after the repeat call.
    let still_mine: Vec<TokenInfo> = svc
        .list_tokens()
        .await
        .into_iter()
        .filter(|t| {
            t.label
                .starts_with(&format!("user:{}:", username.to_lowercase()))
        })
        .collect();
    assert_eq!(still_mine.len(), 1);
}

/// CXA-F350 self-heal: a personal token minted BEFORE the owner column
/// existed (owner '') is backfilled on the next harvest — still idempotent,
/// still no re-mint — so its bearer inherits the member's project reach too.
#[tokio::test]
async fn auto_issue_backfills_the_owner_on_a_pre_migration_personal_token() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let svc = SqlAuthService::connect(&db.dsn())
        .await
        .expect("connect + migrate");

    let pid = std::process::id();
    let username = format!("harvest-backfill-{pid}");
    assert!(
        svc.create_user(&username, "ChangeMe12345!", AuthRole::TechLead)
            .await
    );
    assert!(svc.assign_project(&username, "proj-a").await);

    // Pre-seed the pre-migration shape: an owner-less personal token under
    // the user-prefixed label (create_token records no owner).
    let label = format!("user:{}:remote-store", username.to_lowercase());
    let secret = svc
        .create_token(&label, AuthRole::TechLead)
        .await
        .expect("pre-seed the owner-less personal token");
    // Before any harvest it resolves with no projects (the old behaviour).
    let before = svc.principal_for_bearer(&secret).await.expect("resolve");
    assert!(
        before.projects.is_empty(),
        "owner-less resolves projectless"
    );

    // The next harvest neither re-mints nor re-issues…
    let harvested = svc.auto_issue_personal_token(&username).await;
    assert_eq!(harvested, None, "an existing token must not be re-issued");
    let mine: Vec<TokenInfo> = svc
        .list_tokens()
        .await
        .into_iter()
        .filter(|t| {
            t.label
                .starts_with(&format!("user:{}:", username.to_lowercase()))
        })
        .collect();
    assert_eq!(mine.len(), 1, "still exactly one personal token");

    // …but the owner is backfilled: the SAME secret now carries the
    // member's live project memberships.
    let after = svc.principal_for_bearer(&secret).await.expect("resolve");
    assert_eq!(
        after.projects,
        vec!["proj-a"],
        "owner backfilled from the label"
    );
}

/// CXA-F350 review fix: usernames may contain `_` or `%` (`create_user` only
/// rejects empty names), so the harvest's token lookup must never be a LIKE
/// pattern built from the name — `a_b` would match `aXb` and the owner
/// backfill would write THE WRONG MEMBER's identity onto another user's
/// token, leaking that member's project reach to a stranger's bearer. Pin:
/// harvesting `f350u<pid>_1` mints ITS OWN token and leaves the colliding
/// `f350u<pid>x1` account's owner-less token exactly as it was.
#[tokio::test]
async fn a_username_with_like_wildcards_never_backfills_another_users_token() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let svc = SqlAuthService::connect(&db.dsn())
        .await
        .expect("connect + migrate");

    let pid = std::process::id();
    let underscored = format!("f350u{pid}_1");
    let colliding = format!("f350u{pid}x1");
    // The underscore user owns proj-a; the colliding user owns nothing — a
    // leaked owner would surface as proj-a on the colliding bearer.
    assert!(
        svc.create_user(&underscored, "ChangeMe12345!", AuthRole::TechLead)
            .await
    );
    assert!(svc.assign_project(&underscored, "proj-a").await);
    assert!(
        svc.create_user(&colliding, "ChangeMe12345!", AuthRole::TechLead)
            .await
    );

    // Pre-seed the colliding user's personal token the pre-migration way:
    // minted by an admin, no owner recorded.
    let colliding_secret = svc
        .create_token(
            &format!("user:{}:remote-store", colliding.to_lowercase()),
            AuthRole::TechLead,
        )
        .await
        .expect("pre-seed the colliding owner-less token");

    // Harvest the underscore user: the old LIKE lookup matched the colliding
    // label (`_` matches `x`) and suppressed the mint; prefix equality does
    // not, so this user gets their own token.
    let minted = svc
        .auto_issue_personal_token(&underscored)
        .await
        .expect("the underscore user's harvest must mint their own token");
    let mine = svc
        .principal_for_bearer(&minted)
        .await
        .expect("resolve the fresh personal token");
    assert_eq!(mine.projects, vec!["proj-a"], "own token inherits own reach");

    // And the colliding token is untouched: still owner-less, so its bearer
    // gains nothing from the underscore user's memberships.
    let stranger = svc
        .principal_for_bearer(&colliding_secret)
        .await
        .expect("resolve the colliding token");
    assert!(
        stranger.projects.is_empty(),
        "a wildcard username must never backfill another user's token: {:?}",
        stranger.projects
    );
}
