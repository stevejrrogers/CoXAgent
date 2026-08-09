//! CXA-F002 - SqlAuthService::auto_issue_personal_token mints exactly one
//! personal bearer token per user at login time, bound to that user's OWN role
//! (never an elevation), and then goes idempotent - later calls return None
//! rather than re-minting or re-issuing the plaintext secret.
//!
//! Runs against a real Postgres; skipped unless COXAGENT_TEST_PG_DSN is set
//! (mirrors sql_store_contract.rs - no database in ordinary CI).
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use coxagent_application::auth::{AuthPort, AuthRole, TokenInfo};
use coxagent_infrastructure::SqlAuthService;

#[tokio::test]
async fn auto_issue_mints_once_and_stays_idempotent() {
    let Ok(dsn) = std::env::var("COXAGENT_TEST_PG_DSN") else {
        eprintln!("COXAGENT_TEST_PG_DSN unset - skipping Postgres auth harvest test");
        return;
    };
    let svc = SqlAuthService::connect(&dsn)
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
