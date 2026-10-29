//! `GlForge` — a [`ForgePort`] backed by the GitLab CLI (`glab`), the GitLab
//! analogue of [`super::gh_forge`]. Uses `glab`'s host authentication, targets
//! an explicit `-R <repo>`, and points `GITLAB_HOST` at a self-hosted instance
//! when a base URL is configured. GitLab "merge requests" map onto the same
//! [`PullRequest`] shape the dashboard already speaks.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{ForgePort, IssueDraft, PrFeedback, PullRequest};
use coxagent_application::PortError;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

use super::gl_types::{encoded_project, feedback_from_discussions, RawIssue, RawMr};

/// GitLab forge for one project (`group/name`).
pub struct GlForge {
    repo: String,
    /// Self-hosted host (empty = gitlab.com), passed via `GITLAB_HOST`.
    host: String,
    /// The codebase directory — `glab` runs here so its remote checks resolve
    /// (e.g. `glab mr create` requires a remote matching the host).
    work_dir: PathBuf,
}

impl GlForge {
    #[must_use]
    pub fn new(repo: impl Into<String>, base_url: impl Into<String>, work_dir: PathBuf) -> Self {
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
            work_dir,
        }
    }

    async fn glab(&self, args: &[&str]) -> Result<String, PortError> {
        glab(&self.host, &self.work_dir, args).await
    }

    /// Shared issue lister for the open/closed variants. `state` is the glab
    /// state flag (`--opened` / `--closed`).
    async fn issues(&self, state: &str, limit: usize) -> Result<Vec<IssueDraft>, PortError> {
        let per_page = limit.to_string();
        let json = self
            .glab(&[
                "issue", "list", "-R", &self.repo, state, "-P", &per_page, "-F", "json",
            ])
            .await?;
        let raws: Vec<RawIssue> = serde_json::from_str(&json)
            .map_err(|e| PortError::Backend(format!("glab issue list parse: {e}")))?;
        Ok(raws.into_iter().map(Into::into).collect())
    }
}

async fn glab(host: &str, work_dir: &std::path::Path, args: &[&str]) -> Result<String, PortError> {
    let mut cmd = Command::new("glab");
    cmd.args(args).stdin(Stdio::null());
    if work_dir.is_dir() {
        cmd.current_dir(work_dir);
    }
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

#[async_trait]
impl ForgePort for GlForge {
    async fn open_pr(
        &self,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PullRequest, PortError> {
        self.glab(&[
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
        ])
        .await?;
        let json = self
            .glab(&["mr", "view", head, "-R", &self.repo, "-F", "json"])
            .await?;
        let raw: RawMr = serde_json::from_str(&json)
            .map_err(|e| PortError::Backend(format!("glab mr view parse: {e}")))?;
        Ok(raw.into())
    }

    async fn list_open_prs(&self) -> Result<Vec<PullRequest>, PortError> {
        let json = self
            .glab(&["mr", "list", "-R", &self.repo, "--opened", "-F", "json"])
            .await?;
        let raws: Vec<RawMr> = serde_json::from_str(&json)
            .map_err(|e| PortError::Backend(format!("glab mr list parse: {e}")))?;
        Ok(raws.into_iter().map(Into::into).collect())
    }

    async fn pr_diff(&self, number: u64) -> Result<String, PortError> {
        let n = number.to_string();
        self.glab(&["mr", "diff", &n, "-R", &self.repo]).await
    }

    async fn merge_pr(&self, number: u64) -> Result<(), PortError> {
        let n = number.to_string();
        self.glab(&["mr", "merge", &n, "-R", &self.repo, "--squash", "--yes"])
            .await
            .map(|_| ())
    }

    async fn request_changes(&self, number: u64, comment: &str) -> Result<(), PortError> {
        // GitLab has no "request changes" review verb; a note is the equivalent.
        let n = number.to_string();
        self.glab(&["mr", "note", &n, "-R", &self.repo, "-m", comment])
            .await
            .map(|_| ())
    }

    async fn close_pr(&self, number: u64) -> Result<(), PortError> {
        let n = number.to_string();
        self.glab(&["mr", "close", &n, "-R", &self.repo])
            .await
            .map(|_| ())
    }

    async fn recently_merged(&self) -> Result<Vec<(u64, String)>, PortError> {
        let json = self
            .glab(&[
                "mr", "list", "-R", &self.repo, "--merged", "-P", "30", "-F", "json",
            ])
            .await?;
        let raws: Vec<RawMr> = serde_json::from_str(&json)
            .map_err(|e| PortError::Backend(format!("glab merged list parse: {e}")))?;
        Ok(raws.into_iter().map(|m| (m.iid, m.source_branch)).collect())
    }

    async fn closed_unmerged(&self) -> Result<Vec<(u64, String)>, PortError> {
        // GitLab keeps `merged` and `closed` as distinct states (unlike GitHub,
        // which lumps merged PRs under "closed"), so `--closed` is already the
        // unmerged-closed set — no mergedAt filter needed.
        let json = self
            .glab(&[
                "mr", "list", "-R", &self.repo, "--closed", "-P", "50", "-F", "json",
            ])
            .await?;
        let raws: Vec<RawMr> = serde_json::from_str(&json)
            .map_err(|e| PortError::Backend(format!("glab closed list parse: {e}")))?;
        Ok(raws.into_iter().map(|m| (m.iid, m.source_branch)).collect())
    }

    async fn comment_pr(&self, number: u64, body: &str) -> Result<(), PortError> {
        let n = number.to_string();
        self.glab(&["mr", "note", &n, "-R", &self.repo, "-m", body])
            .await
            .map(|_| ())
    }

    async fn list_open_issues(&self, limit: usize) -> Result<Vec<IssueDraft>, PortError> {
        self.issues("--opened", limit).await
    }

    async fn list_closed_issues(&self, limit: usize) -> Result<Vec<IssueDraft>, PortError> {
        self.issues("--closed", limit).await
    }

    async fn pr_feedback(&self, number: u64) -> Result<Vec<PrFeedback>, PortError> {
        // GitLab has no CHANGES_REQUESTED review verb; its equivalent is an
        // unresolved resolvable discussion thread. Pull the MR's discussions and
        // surface each still-open review thread as feedback to address.
        let path = format!(
            "projects/{}/merge_requests/{number}/discussions?per_page=100",
            encoded_project(&self.repo)
        );
        let json = self.glab(&["api", &path]).await?;
        feedback_from_discussions(&json)
    }
}
