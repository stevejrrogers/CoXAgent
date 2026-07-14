//! `GlForge` — a [`ForgePort`] backed by the GitLab CLI (`glab`), the GitLab
//! analogue of [`super::gh_forge`]. Uses `glab`'s host authentication, targets
//! an explicit `-R <repo>`, and points `GITLAB_HOST` at a self-hosted instance
//! when a base URL is configured. GitLab "merge requests" map onto the same
//! [`PullRequest`] shape the dashboard already speaks.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{ForgePort, PullRequest};
use coxagent_application::PortError;
use std::process::Stdio;
use tokio::process::Command;

/// GitLab forge for one project (`group/name`).
pub struct GlForge {
    repo: String,
    /// Self-hosted host (empty = gitlab.com), passed via `GITLAB_HOST`.
    host: String,
}

impl GlForge {
    #[must_use]
    pub fn new(repo: impl Into<String>, base_url: impl Into<String>) -> Self {
        let base = base_url.into();
        let host = base
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_owned();
        Self {
            repo: repo.into(),
            host,
        }
    }
}

async fn glab(host: &str, args: &[&str]) -> Result<String, PortError> {
    let mut cmd = Command::new("glab");
    cmd.args(args).stdin(Stdio::null());
    if !host.is_empty() {
        cmd.env("GITLAB_HOST", host);
    }
    let out = cmd
        .output()
        .await
        .map_err(|e| PortError::Backend(format!("glab spawn (is glab installed?): {e}")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(PortError::Backend(format!(
            "glab {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// One merge request from `glab mr list -F json` (the GitLab API MR object).
#[derive(serde::Deserialize)]
struct RawMr {
    iid: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    source_branch: String,
    #[serde(default)]
    target_branch: String,
    #[serde(default)]
    web_url: String,
    #[serde(default)]
    author: RawAuthor,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    merge_status: String,
    #[serde(default)]
    pipeline: Option<RawPipeline>,
    #[serde(default)]
    head_pipeline: Option<RawPipeline>,
}

#[derive(serde::Deserialize, Default)]
struct RawAuthor {
    #[serde(default)]
    username: String,
}

#[derive(serde::Deserialize)]
struct RawPipeline {
    #[serde(default)]
    status: String,
}

/// Map a GitLab pipeline status to the dashboard's CI rollup word.
fn ci_from_pipeline(mr: &RawMr) -> String {
    let status = mr
        .head_pipeline
        .as_ref()
        .or(mr.pipeline.as_ref())
        .map(|p| p.status.to_lowercase());
    match status.as_deref() {
        None | Some("") => "none".to_owned(),
        Some("success" | "passed") => "passing".to_owned(),
        Some("failed" | "canceled" | "cancelled") => "failing".to_owned(),
        _ => "pending".to_owned(), // running / pending / created / manual / scheduled
    }
}

impl From<RawMr> for PullRequest {
    fn from(m: RawMr) -> Self {
        let ci = ci_from_pipeline(&m);
        PullRequest {
            number: m.iid,
            title: m.title,
            head: m.source_branch,
            base: m.target_branch,
            url: m.web_url,
            author: m.author.username,
            ci,
            mergeable: m.merge_status == "can_be_merged",
            created: m.created_at,
        }
    }
}

#[async_trait]
impl ForgePort for GlForge {
    async fn open_pr(
        &self,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PullRequest, PortError> {
        glab(
            &self.host,
            &[
                "mr",
                "create",
                "-R",
                &self.repo,
                "--source-branch",
                head,
                "--target-branch",
                base,
                "--title",
                title,
                "--description",
                body,
                "--yes",
            ],
        )
        .await?;
        let json = glab(
            &self.host,
            &["mr", "view", head, "-R", &self.repo, "-F", "json"],
        )
        .await?;
        let raw: RawMr = serde_json::from_str(&json)
            .map_err(|e| PortError::Backend(format!("glab mr view parse: {e}")))?;
        Ok(raw.into())
    }

    async fn list_open_prs(&self) -> Result<Vec<PullRequest>, PortError> {
        let json = glab(
            &self.host,
            &["mr", "list", "-R", &self.repo, "--opened", "-F", "json"],
        )
        .await?;
        let raws: Vec<RawMr> = serde_json::from_str(&json)
            .map_err(|e| PortError::Backend(format!("glab mr list parse: {e}")))?;
        Ok(raws.into_iter().map(Into::into).collect())
    }

    async fn pr_diff(&self, number: u64) -> Result<String, PortError> {
        let n = number.to_string();
        glab(&self.host, &["mr", "diff", &n, "-R", &self.repo]).await
    }

    async fn merge_pr(&self, number: u64) -> Result<(), PortError> {
        let n = number.to_string();
        glab(
            &self.host,
            &["mr", "merge", &n, "-R", &self.repo, "--squash", "--yes"],
        )
        .await
        .map(|_| ())
    }

    async fn request_changes(&self, number: u64, comment: &str) -> Result<(), PortError> {
        // GitLab has no "request changes" review verb; a note is the equivalent.
        let n = number.to_string();
        glab(
            &self.host,
            &["mr", "note", &n, "-R", &self.repo, "-m", comment],
        )
        .await
        .map(|_| ())
    }

    async fn close_pr(&self, number: u64) -> Result<(), PortError> {
        let n = number.to_string();
        glab(&self.host, &["mr", "close", &n, "-R", &self.repo])
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_gitlab_mr_json_to_pull_request() {
        let json = r#"{
            "iid": 12, "title": "feat(X-1): add", "source_branch": "feat/X-1",
            "target_branch": "develop", "web_url": "https://gitlab.com/g/p/-/merge_requests/12",
            "author": {"username": "coxagent-bot"}, "created_at": "2026-07-14T00:00:00Z",
            "merge_status": "can_be_merged", "head_pipeline": {"status": "success"}
        }"#;
        let raw: RawMr = serde_json::from_str(json).unwrap();
        let pr: PullRequest = raw.into();
        assert_eq!(pr.number, 12);
        assert_eq!(pr.head, "feat/X-1");
        assert_eq!(pr.base, "develop");
        assert_eq!(pr.author, "coxagent-bot");
        assert_eq!(pr.ci, "passing");
        assert!(pr.mergeable);
    }

    #[test]
    fn ci_rollup_covers_pipeline_states() {
        let mk = |s: &str| RawMr {
            iid: 1,
            title: String::new(),
            source_branch: String::new(),
            target_branch: String::new(),
            web_url: String::new(),
            author: RawAuthor::default(),
            created_at: String::new(),
            merge_status: String::new(),
            pipeline: None,
            head_pipeline: Some(RawPipeline {
                status: s.to_owned(),
            }),
        };
        assert_eq!(ci_from_pipeline(&mk("success")), "passing");
        assert_eq!(ci_from_pipeline(&mk("failed")), "failing");
        assert_eq!(ci_from_pipeline(&mk("running")), "pending");
        let mut none = mk("");
        none.head_pipeline = None;
        assert_eq!(ci_from_pipeline(&none), "none");
    }
}
