//! CXA-F002 - SqlAuthService::auto_issue_personal_token mints exactly one
//! personal bearer token per user at login time, bound to that user's OWN role
//! (never an elevation), and then goes idempotent - later calls return None
//! rather than re-minting or re-issuing the plaintext secret.
//!
//! The test is `#[ignore]`d (ordinary CI without a database skips it
//! explicitly) and runs only through the fail-closed guard in
//! `tests/common/mod.rs`, which refuses any target but an ephemeral
//! `cxa_test*` Postgres - never a live hub, never an unverified one
//! (CXA-F326). Teardown deletes the harvest user at test end.
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use coxagent_application::auth::{AuthPort, AuthRole, TokenInfo};

mod common;
use coxagent_infrastructure::SqlAuthService;

#[tokio::test]
#[ignore = "skipped: needs an ephemeral cxa_test* Postgres — see README (Integration test environment)"]
async fn auto_issue_mints_once_and_stays_idempotent() {
    let pg = common::pg("sql_auth_token_harvest").await;
    let svc = SqlAuthService::connect(&pg.dsn)
        .await
        .expect("connect + migrate");

    // Namespace the username per run so repeated runs against the same DB
    // are clean.
    let username = pg.project("harvest");
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

    // Teardown: the namespaced harvest user must not outlive the test.
    assert!(
        svc.delete_user(&username).await,
        "teardown must delete the harvest user"
    );
}
