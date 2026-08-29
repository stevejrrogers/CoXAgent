// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Public share links (CXA-F069): the admin mint/list/revoke endpoints for
//! unguessable per-project URLs that unlock a read-only status page. The
//! token IS the credential — same trust model as `/join/:token` — so it is
//! minted by a CSPRNG, logged nowhere, and revocation kills it on the next
//! request. The page itself lives in `share_page.rs`.

use super::*;

/// Content-Security-Policy for the PUBLIC share page: the HTML is
/// server-rendered with zero scripts and zero external resources, so the
/// policy denies everything but inline CSS.
const SHARE_PAGE_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; img-src data:; \
     base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Mint an unguessable share token: a v4 UUID (122 CSPRNG bits) in simple
/// hex — URL-safe, constant length, no dashboard-internal id semantics.
fn mint_share_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[derive(serde::Deserialize)]
pub(super) struct ShareLinkCreateReq {
    /// Optional admin label, e.g. the stakeholder it was shared with.
    #[serde(default)]
    pub(super) name: Option<String>,
}

/// Mint a share link for a project (admin — manage tier, membership already
/// enforced by `auth_mw`). Returns the token once; only the settings API can
/// ever read it back.
pub(super) async fn share_link_create_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
    req: Option<Json<ShareLinkCreateReq>>,
) -> axum::response::Response {
    if !user_can_manage(&app, &headers).await {
        return (StatusCode::FORBIDDEN, "admin role required").into_response();
    }
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let name = req
        .and_then(|Json(r)| r.name)
        .unwrap_or_default()
        .trim()
        .chars()
        .take(80)
        .collect::<String>();
    let user = resolve_username(&app, &headers).await;
    let link = ShareLink {
        token: mint_share_token(),
        project_id: p.id.clone(),
        name,
        created_by: user.clone(),
        created_at: coxagent_application::state::now_rfc3339(),
        revoked: false,
    };
    let (token, created_at, name) = (
        link.token.clone(),
        link.created_at.clone(),
        link.name.clone(),
    );
    {
        let mut w = app.workspace.inner.lock().await;
        w.share_links.push(link);
    }
    app.workspace.save().await;
    audit_push(&app.audit, &user, "share link created".to_owned(), 200).await;
    Json(serde_json::json!({ "token": token, "name": name, "created_at": created_at }))
        .into_response()
}

/// List the hub's share links (admin). Management spans all projects because
/// the links live in the company-level workspace doc, like invites.
pub(super) async fn share_link_list_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if !user_can_manage(&app, &headers).await {
        return (StatusCode::FORBIDDEN, "admin role required").into_response();
    }
    let w = app.workspace.inner.lock().await.clone();
    let links: Vec<serde_json::Value> = w
        .share_links
        .iter()
        .map(|l| {
            serde_json::json!({
                "token": l.token,
                "name": l.name,
                "created_at": l.created_at,
                "revoked": l.revoked,
            })
        })
        .collect();
    Json(links).into_response()
}

/// Revoke a share link (admin). The record is kept for the audit trail but
/// the URL dies immediately: the page handler re-checks the flag per request.
pub(super) async fn share_link_revoke_ep(
    State(app): State<AppState>,
    Path((pid, token)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if !user_can_manage(&app, &headers).await {
        return (StatusCode::FORBIDDEN, "admin role required").into_response();
    }
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let revoked = {
        let mut w = app.workspace.inner.lock().await;
        match w
            .share_links
            .iter_mut()
            .find(|l| l.token == token && l.project_id == p.id)
        {
            Some(l) => {
                l.revoked = true;
                true
            }
            None => false,
        }
    };
    if !revoked {
        return not_found();
    }
    app.workspace.save().await;
    let user = resolve_username(&app, &headers).await;
    audit_push(&app.audit, &user, "share link revoked".to_owned(), 200).await;
    StatusCode::NO_CONTENT.into_response()
}

/// The public read-only status page. Unknown OR revoked token → 404 (never a
/// 403: probing must not be able to distinguish "exists" from "dead").
pub(super) async fn share_page_ep(
    State(app): State<AppState>,
    Path(token): Path<String>,
) -> axum::response::Response {
    let pid = {
        let w = app.workspace.inner.lock().await;
        w.share_links
            .iter()
            .find(|l| l.token == token && !l.revoked)
            .map(|l| l.project_id.clone())
    };
    let Some(pid) = pid else {
        return not_found();
    };
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(state) = p.store.load().await else {
        return internal_error("state unavailable");
    };
    let workers = p.store.workers().await.unwrap_or_default();
    let now = i64::try_from(now_unix_secs()).unwrap_or(0);
    let snapshot = share_snapshot(&state, &workers, now);
    // Hardening headers for a PUBLIC, unauthenticated page: no-store keeps a
    // secret URL out of shared caches; strict referrer policy keeps the token
    // out of any future Referer header; the CSP denies all but inline CSS.
    (
        [
            (header::CONTENT_SECURITY_POLICY, SHARE_PAGE_CSP),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::X_FRAME_OPTIONS, "DENY"),
            (header::REFERRER_POLICY, "strict-origin-when-cross-origin"),
        ],
        axum::response::Html(render_share_page(&state, &p, &snapshot)),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn share_tokens_are_cryptographically_random_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let t = mint_share_token();
            assert_eq!(t.len(), 32, "v4 uuid simple form is 32 hex chars");
            assert!(
                t.chars().all(|c| c.is_ascii_hexdigit()),
                "URL-safe hex only, got {t}"
            );
            assert!(seen.insert(t), "a repeated CSPRNG token is a collision");
        }
    }
}
