// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Pull request listing endpoint and forge-account parsing helpers.

use super::*;

/// List open pull/merge requests for a project's repository.
///
/// The list is supplied by the runner over HTTP (the runner holds the forge
/// credentials), so this reads what was reported and persisted rather than
/// asking a forge the hub may not be able to reach (a container serving the
/// dashboard has no token). Falls back to an empty list when no PRs have been
/// reported yet.
pub(super) async fn list_prs_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(state) = p.store.load().await else {
        return Json(serde_json::json!({ "configured": false, "prs": [] })).into_response();
    };
    let auto_merge = std::fs::read_to_string(&p.config_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Config>(&t).ok())
        .is_some_and(|c| c.git.auto_merge);
    let enriched: Vec<serde_json::Value> = state
        .open_prs
        .iter()
        .map(|pr| {
            let mut v = serde_json::to_value(pr).unwrap_or_default();
            if let Some(r) = state.reviews.iter().find(|r| r.number == pr.number) {
                v["review"] = serde_json::json!({
                    "decision": r.decision, "summary": r.summary, "at": r.at,
                });
            }
            v
        })
        .collect();
    let configured = p.forge.is_some();
    Json(serde_json::json!({
        "configured": configured, "auto_merge": auto_merge, "prs": enriched
    }))
    .into_response()
}

/// (project id, PR number) pairs with a force-merge currently running — the
/// hub-wide guard against concurrent resolutions in one work_dir.
pub(super) fn force_inflight(
) -> &'static tokio::sync::Mutex<std::collections::HashSet<(String, u64)>> {
    static SET: std::sync::OnceLock<
        tokio::sync::Mutex<std::collections::HashSet<(String, u64)>>,
    > = std::sync::OnceLock::new();
    SET.get_or_init(|| tokio::sync::Mutex::new(std::collections::HashSet::new()))
}

/// All signed-in accounts parsed from `gh`/`glab auth status` output, with the
/// active one flagged. Returns `(name, is_active)` pairs in the order the CLI
/// lists them. Empty when no account can be parsed.
///
/// `gh auth status` (multi-account) looks like:
/// ```text
/// github.com
///   ✓ Logged in to github.com account alice (keyring)
///   - Active account: true
///   ✓ Logged in to github.com account bob (keyring)
///   - Active account: false
/// ```
/// `glab auth status` (single account) looks like:
/// ```text
/// - Logged in to gitlab.com as alice using token
/// ```
/// — no "Active account" line, so the lone entry is marked active here.
pub(super) fn parse_accounts(out: &str) -> Vec<(String, bool)> {
    let mut accounts: Vec<(String, bool)> = Vec::new();
    for line in out.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed
            .find("Logged in to ")
            .map(|i| &trimmed[i + "Logged in to ".len()..])
        else {
            // The line right after a "Logged in" entry tells us if it's the
            // active account (gh multi-account form only).
            if trimmed.starts_with("- Active account:") || trimmed.starts_with("Active account:") {
                if let Some(idx) = accounts.len().checked_sub(1) {
                    accounts[idx].1 = trimmed.contains("true");
                }
            }
            continue;
        };
        // Skip the host token, then the marker (`account` or `as`), then the
        // username is the next whitespace-separated word.
        let mut parts = rest.split_whitespace();
        let _host = parts.next();
        let marker_or_user = parts.next().unwrap_or("");
        let user = if marker_or_user == "account" || marker_or_user == "as" {
            parts.next().unwrap_or("")
        } else {
            marker_or_user
        };
        let name = user
            .trim_matches(|c: char| c == '@' || c == '(' || c == ')' || c == '.')
            .to_owned();
        if !name.is_empty() {
            // Dedupe: `gh auth status` may list the same login twice across
            // hosts; keep the first occurrence.
            if !accounts.iter().any(|(n, _)| n == &name) {
                accounts.push((name, false));
            }
        }
    }
    // Single-account output (notably glab) has no "Active account" line — the
    // one account is the active one.
    if accounts.len() == 1 && !accounts[0].1 {
        accounts[0].1 = true;
    }
    // If none was flagged active (single-section multi-account edge case),
    // fall back to the first — matching the historical `parse_account` pick.
    if !accounts.is_empty() && !accounts.iter().any(|(_, a)| *a) {
        accounts[0].1 = true;
    }
    accounts
}

/// Pull the signed-in account out of `gh`/`glab auth status` output — the
/// active one, falling back to the first listed. Used by callers that only
/// need one account (e.g. the post-`connect` verifier).
pub(super) fn parse_account(out: &str) -> Option<String> {
    let all = parse_accounts(out);
    all.iter()
        .find(|(_, a)| *a)
        .or_else(|| all.first())
        .map(|(n, _)| n.clone())
}

#[cfg(test)]
mod parse_accounts_tests {
    use super::{parse_account, parse_accounts};

    #[test]
    fn gh_multi_account_picks_active() {
        let out = "\
github.com
  ✓ Logged in to github.com account stevejrrogers (keyring)
  - Active account: true
  - Git operations protocol: ssh

  ✓ Logged in to github.com account kyroc3 (keyring)
  - Active account: false
  - Git operations protocol: ssh
";
        let all = parse_accounts(out);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0], ("stevejrrogers".to_owned(), true));
        assert_eq!(all[1], ("kyroc3".to_owned(), false));
        assert_eq!(parse_account(out).as_deref(), Some("stevejrrogers"));
    }

    #[test]
    fn gh_single_account_legacy_as_form() {
        let out = "github.com\n  ✓ Logged in to github.com as alice (oauth_token)\n";
        let all = parse_accounts(out);
        assert_eq!(all, vec![("alice".to_owned(), true)]);
        assert_eq!(parse_account(out).as_deref(), Some("alice"));
    }

    #[test]
    fn glab_single_account_no_active_line_is_active() {
        let out = "- Logged in to gitlab.com as alice using token\n";
        let all = parse_accounts(out);
        assert_eq!(all, vec![("alice".to_owned(), true)]);
        assert_eq!(parse_account(out).as_deref(), Some("alice"));
    }

    #[test]
    fn empty_output_yields_empty() {
        assert!(parse_accounts("").is_empty());
        assert!(parse_account("").is_none());
    }

    #[test]
    fn duplicate_account_across_hosts_is_deduped() {
        let out = "\
github.com
  ✓ Logged in to github.com account alice (keyring)
  - Active account: true
ghe.example.com
  ✓ Logged in to ghe.example.com account alice (keyring)
  - Active account: false
";
        let all = parse_accounts(out);
        assert_eq!(all, vec![("alice".to_owned(), true)]);
    }

    #[test]
    fn no_active_marker_falls_back_to_first() {
        let out = "\
github.com
  ✓ Logged in to github.com account alice (keyring)
  ✓ Logged in to github.com account bob (keyring)
";
        let all = parse_accounts(out);
        assert_eq!(all[0], ("alice".to_owned(), true));
        assert_eq!(all[1], ("bob".to_owned(), false));
        assert_eq!(parse_account(out).as_deref(), Some("alice"));
    }
}
