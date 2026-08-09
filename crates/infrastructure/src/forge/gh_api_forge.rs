//! `GhApiForge` — a [`ForgePort`] over the GitHub REST API with a personal
//! access token, needing NO `gh` binary. This is the fallback for a host (a
//! container, a CI box) that has a token but not the CLI: `GhForge` shells out
//! to `gh`, this one talks HTTPS directly.
//!
//! Selection lives in the composition root: `gh` on PATH → `GhForge`; otherwise
//! a configured token → this. Every call targets an explicit `owner/repo`, so
//! it is independent of the working directory.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{ForgePort, PrFeedback, PullRequest};
use coxagent_application::PortError;

/// GitHub REST forge for one repository, authenticated by a token.
pub struct GhApiForge {
    repo: String,
    /// API base: `https://api.github.com`, or `https://<host>/api/v3` for
    /// GitHub Enterprise.
    api_base: String,
    token: String,
    client: reqwest::Client,
}

impl GhApiForge {
    /// Build for `owner/repo`. `base_url` empty = github.com, else an Enterprise
    /// host. Returns `None` when no token is available — the caller then keeps
    /// whatever forge (or none) it had.
    #[must_use]
    pub fn new(repo: impl Into<String>, base_url: &str, token: impl Into<String>) -> Option<Self> {
        let token = token.into();
        if token.trim().is_empty() {
            return None;
        }
        let host = base_url
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/');
        let api_base = if host.is_empty() || host == "github.com" {
            "https://api.github.com".to_owned()
        } else {
            format!("https://{host}/api/v3")
        };
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("coxagent")
            .build()
            .ok()?;
        Some(Self {
            repo: repo.into(),
            api_base,
            token,
            client,
        })
    }

    /// A token from the environment: `COXAGENT_GH_TOKEN` wins, then the
    /// conventional `GITHUB_TOKEN` / `GH_TOKEN`. `None` when none is set.
    #[must_use]
    pub fn token_from_env() -> Option<String> {
        for k in ["COXAGENT_GH_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"] {
            if let Ok(v) = std::env::var(k) {
                if !v.trim().is_empty() {
                    return Some(v.trim().to_owned());
                }
            }
        }
        None
    }

    fn url(&self, path: &str) -> String {
        format!("{}/repos/{}/{path}", self.api_base, self.repo)
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, self.url(path))
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    /// GET a path as parsed JSON.
    async fn get_json(&self, path: &str) -> Result<serde_json::Value, PortError> {
        let resp = self
            .req(reqwest::Method::GET, path)
            .send()
            .await
            .map_err(|e| PortError::Backend(format!("github GET {path}: {e}")))?;
        json_or_err(resp, path).await
    }

    /// One open PR enriched with `mergeable` + CI rollup — the list endpoint
    /// omits both, so each is fetched once (open PR counts are WIP-bounded, so
    /// this stays a handful of calls).
    async fn enrich(&self, raw: &serde_json::Value) -> PullRequest {
        let number = raw.get("number").and_then(serde_json::Value::as_u64).unwrap_or(0);
        let head_ref = raw
            .pointer("/head/ref")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let head_sha = raw
            .pointer("/head/sha")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        // mergeable is computed asynchronously by GitHub; the single-PR GET
        // triggers and usually returns it. Absent (null, still computing) reads
        // as mergeable — the merge itself is the real gate, and blocking on an
        // unknown would stall a fine PR.
        let mergeable = self
            .get_json(&format!("pulls/{number}"))
            .await
            .ok()
            .and_then(|v| v.get("mergeable").and_then(serde_json::Value::as_bool))
            .unwrap_or(true);
        let ci = self.ci_rollup(&head_sha).await;
        PullRequest {
            number,
            title: raw.get("title").and_then(serde_json::Value::as_str).unwrap_or_default().to_owned(),
            head: head_ref,
            base: raw
                .pointer("/base/ref")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            url: raw.get("html_url").and_then(serde_json::Value::as_str).unwrap_or_default().to_owned(),
            author: raw
                .pointer("/user/login")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            ci,
            mergeable,
            created: raw.get("created_at").and_then(serde_json::Value::as_str).unwrap_or_default().to_owned(),
        }
    }

    /// The combined check/status rollup for a commit → `passing`/`failing`/
    /// `pending`/`none`, matching what `GhForge` reports.
    async fn ci_rollup(&self, sha: &str) -> String {
        if sha.is_empty() {
            return "none".to_owned();
        }
        let Ok(v) = self.get_json(&format!("commits/{sha}/check-runs")).await else {
            return "none".to_owned();
        };
        let runs = v.get("check_runs").and_then(serde_json::Value::as_array);
        let Some(runs) = runs else { return "none".to_owned() };
        if runs.is_empty() {
            return "none".to_owned();
        }
        let mut any_pending = false;
        let mut any_fail = false;
        for r in runs {
            match r.get("status").and_then(serde_json::Value::as_str) {
                Some("completed") => {
                    if !matches!(
                        r.get("conclusion").and_then(serde_json::Value::as_str),
                        Some("success" | "neutral" | "skipped")
                    ) {
                        any_fail = true;
                    }
                }
                _ => any_pending = true,
            }
        }
        if any_fail {
            "failing"
        } else if any_pending {
            "pending"
        } else {
            "passing"
        }
        .to_owned()
    }
}

/// Return the JSON body on 2xx, else a `Backend` error carrying the status and
/// a snippet — so a 401/404/422 reads as itself, not a parse failure.
async fn json_or_err(resp: reqwest::Response, what: &str) -> Result<serde_json::Value, PortError> {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if status.is_success() {
        serde_json::from_str(&text)
            .map_err(|e| PortError::Backend(format!("github {what}: parse {e}")))
    } else {
        let snip: String = text.chars().take(200).collect();
        Err(PortError::Backend(format!("github {what}: {status} {snip}")))
    }
}

/// PRs from a `state=` listing as `(number, head_ref)`, optionally requiring a
/// merged/unmerged status — shared by `recently_merged` / `closed_unmerged`.
fn number_and_head(list: &serde_json::Value, want_merged: Option<bool>) -> Vec<(u64, String)> {
    list.as_array()
        .map(|arr| {
            arr.iter()
                .filter(|p| match want_merged {
                    None => true,
                    Some(true) => !p.get("merged_at").is_none_or(serde_json::Value::is_null),
                    Some(false) => p.get("merged_at").is_none_or(serde_json::Value::is_null),
                })
                .filter_map(|p| {
                    Some((
                        p.get("number")?.as_u64()?,
                        p.pointer("/head/ref")?.as_str()?.to_owned(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[async_trait]
impl ForgePort for GhApiForge {
    async fn open_pr(
        &self,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PullRequest, PortError> {
        let resp = self
            .req(reqwest::Method::POST, "pulls")
            .json(&serde_json::json!({ "head": head, "base": base, "title": title, "body": body }))
            .send()
            .await
            .map_err(|e| PortError::Backend(format!("github open_pr: {e}")))?;
        let v = json_or_err(resp, "open_pr").await?;
        Ok(self.enrich(&v).await)
    }

    async fn list_open_prs(&self) -> Result<Vec<PullRequest>, PortError> {
        let v = self.get_json("pulls?state=open&per_page=100").await?;
        let Some(arr) = v.as_array() else {
            return Ok(Vec::new());
        };
        let mut out = Vec::with_capacity(arr.len());
        for raw in arr {
            out.push(self.enrich(raw).await);
        }
        Ok(out)
    }

    async fn pr_diff(&self, number: u64) -> Result<String, PortError> {
        let resp = self
            .req(reqwest::Method::GET, &format!("pulls/{number}"))
            .header("Accept", "application/vnd.github.v3.diff")
            .send()
            .await
            .map_err(|e| PortError::Backend(format!("github pr_diff: {e}")))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if status.is_success() {
            Ok(text)
        } else {
            Err(PortError::Backend(format!("github pr_diff: {status}")))
        }
    }

    async fn merge_pr(&self, number: u64) -> Result<(), PortError> {
        let resp = self
            .req(reqwest::Method::PUT, &format!("pulls/{number}/merge"))
            .json(&serde_json::json!({ "merge_method": "squash" }))
            .send()
            .await
            .map_err(|e| PortError::Backend(format!("github merge_pr: {e}")))?;
        json_or_err(resp, "merge_pr").await.map(|_| ())
    }

    async fn request_changes(&self, number: u64, comment: &str) -> Result<(), PortError> {
        let resp = self
            .req(reqwest::Method::POST, &format!("pulls/{number}/reviews"))
            .json(&serde_json::json!({ "event": "REQUEST_CHANGES", "body": comment }))
            .send()
            .await
            .map_err(|e| PortError::Backend(format!("github request_changes: {e}")))?;
        json_or_err(resp, "request_changes").await.map(|_| ())
    }

    async fn close_pr(&self, number: u64) -> Result<(), PortError> {
        let resp = self
            .req(reqwest::Method::PATCH, &format!("pulls/{number}"))
            .json(&serde_json::json!({ "state": "closed" }))
            .send()
            .await
            .map_err(|e| PortError::Backend(format!("github close_pr: {e}")))?;
        json_or_err(resp, "close_pr").await.map(|_| ())
    }

    async fn recently_merged(&self) -> Result<Vec<(u64, String)>, PortError> {
        let v = self
            .get_json("pulls?state=closed&sort=updated&direction=desc&per_page=30")
            .await?;
        Ok(number_and_head(&v, Some(true)))
    }

    async fn closed_unmerged(&self) -> Result<Vec<(u64, String)>, PortError> {
        let v = self
            .get_json("pulls?state=closed&sort=updated&direction=desc&per_page=50")
            .await?;
        Ok(number_and_head(&v, Some(false)))
    }

    async fn pr_feedback(&self, number: u64) -> Result<Vec<PrFeedback>, PortError> {
        // Formal reviews (approve / request-changes with a body) plus issue
        // comments — the same "what to change" a DEV agent reads back.
        let mut out = Vec::new();
        if let Ok(v) = self.get_json(&format!("pulls/{number}/reviews?per_page=100")).await {
            if let Some(arr) = v.as_array() {
                for r in arr {
                    let body = r.get("body").and_then(serde_json::Value::as_str).unwrap_or_default();
                    if body.trim().is_empty() {
                        continue;
                    }
                    out.push(PrFeedback {
                        author: r.pointer("/user/login").and_then(serde_json::Value::as_str).unwrap_or_default().to_owned(),
                        body: body.to_owned(),
                        at: r.get("submitted_at").and_then(serde_json::Value::as_str).unwrap_or_default().to_owned(),
                    });
                }
            }
        }
        if let Ok(v) = self.get_json(&format!("issues/{number}/comments?per_page=100")).await {
            if let Some(arr) = v.as_array() {
                for c in arr {
                    let body = c.get("body").and_then(serde_json::Value::as_str).unwrap_or_default();
                    if body.trim().is_empty() {
                        continue;
                    }
                    out.push(PrFeedback {
                        author: c.pointer("/user/login").and_then(serde_json::Value::as_str).unwrap_or_default().to_owned(),
                        body: body.to_owned(),
                        at: c.get("created_at").and_then(serde_json::Value::as_str).unwrap_or_default().to_owned(),
                    });
                }
            }
        }
        Ok(out)
    }

    async fn comment_pr(&self, number: u64, body: &str) -> Result<(), PortError> {
        let resp = self
            .req(reqwest::Method::POST, &format!("issues/{number}/comments"))
            .json(&serde_json::json!({ "body": body }))
            .send()
            .await
            .map_err(|e| PortError::Backend(format!("github comment_pr: {e}")))?;
        json_or_err(resp, "comment_pr").await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_base_defaults_to_dotcom_and_maps_enterprise() {
        let a = GhApiForge::new("o/r", "", "tok").expect("forge");
        assert_eq!(a.api_base, "https://api.github.com");
        assert_eq!(a.url("pulls"), "https://api.github.com/repos/o/r/pulls");
        let e = GhApiForge::new("o/r", "https://ghe.corp/", "tok").expect("forge");
        assert_eq!(e.api_base, "https://ghe.corp/api/v3");
    }

    #[test]
    fn no_token_no_forge() {
        assert!(GhApiForge::new("o/r", "", "").is_none());
        assert!(GhApiForge::new("o/r", "", "   ").is_none());
    }

    #[test]
    fn merged_vs_unmerged_filter() {
        let list = serde_json::json!([
            {"number":1,"merged_at":"2026-01-01T00:00:00Z","head":{"ref":"a"}},
            {"number":2,"merged_at":null,"head":{"ref":"b"}},
        ]);
        assert_eq!(number_and_head(&list, Some(true)), vec![(1, "a".to_owned())]);
        assert_eq!(number_and_head(&list, Some(false)), vec![(2, "b".to_owned())]);
        assert_eq!(number_and_head(&list, None).len(), 2);
    }
}
