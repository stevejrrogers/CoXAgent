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

/// A note inside a merge-request discussion (`.../discussions` API).
#[derive(serde::Deserialize)]
struct RawNote {
    #[serde(default)]
    body: String,
    #[serde(default)]
    author: RawAuthor,
    #[serde(default)]
    created_at: String,
    /// Only review-thread notes are resolvable; plain comments and system
    /// notes (branch pushed, assigned, …) are not.
    #[serde(default)]
    resolvable: bool,
    #[serde(default)]
    resolved: bool,
    #[serde(default)]
    system: bool,
}

#[derive(serde::Deserialize)]
struct RawDiscussion {
    #[serde(default)]
    notes: Vec<RawNote>,
}

/// Map GitLab MR discussions to outstanding review feedback: the first note of
/// every thread that is resolvable, not resolved, and not a system note — i.e.
/// a human review comment still awaiting a fix. GitLab's `resolved` flag is a
/// truer "already addressed" signal than GitHub's commit-time heuristic, so
/// there is no need to compare against the last push.
fn feedback_from_discussions(json: &str) -> Result<Vec<PrFeedback>, PortError> {
    let discussions: Vec<RawDiscussion> = serde_json::from_str(json)
        .map_err(|e| PortError::Backend(format!("glab discussions parse: {e}")))?;
    Ok(discussions
        .into_iter()
        .filter_map(|d| {
            d.notes
                .into_iter()
                .find(|n| n.resolvable && !n.resolved && !n.system && !n.body.trim().is_empty())
                .map(|n| PrFeedback {
                    author: n.author.username,
                    body: n.body,
                    at: n.created_at,
                })
        })
        .collect())
}

/// The `:id` path segment for a `group/name` repo in a `glab api` URL — the
/// GitLab API takes the project path URL-encoded (only `/` needs escaping in a
/// `group/subgroup/name` path).
fn encoded_project(repo: &str) -> String {
    repo.replace('/', "%2F")
}

/// One issue from `glab issue list -F json` (the GitLab API issue object).
#[derive(serde::Deserialize)]
struct RawIssue {
    iid: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    labels: Vec<serde_json::Value>,
    #[serde(default)]
    web_url: String,
}

impl From<RawIssue> for IssueDraft {
    fn from(i: RawIssue) -> Self {
        IssueDraft {
            number: i.iid,
            title: i.title,
            body: i.description,
            labels: super::label_names(&i.labels),
            url: i.web_url,
        }
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
    fn maps_gitlab_issue_json_to_issue_draft() {
        let json = r#"{
            "iid": 7, "title": "Login is slow", "description": "p99 2s",
            "labels": ["bug", "priority:high"],
            "web_url": "https://gitlab.com/g/p/-/issues/7"
        }"#;
        let raw: RawIssue = serde_json::from_str(json).unwrap();
        let d: IssueDraft = raw.into();
        assert_eq!(d.number, 7);
        assert_eq!(d.title, "Login is slow");
        assert_eq!(d.body, "p99 2s");
        assert_eq!(d.labels, vec!["bug".to_owned(), "priority:high".to_owned()]);
        assert_eq!(d.url, "https://gitlab.com/g/p/-/issues/7");
    }

    #[test]
    fn issue_labels_accept_object_shape_too() {
        // GitLab API label details can arrive as objects with a `name` field.
        let json = r#"{"iid": 8, "title": "x", "labels": [{"name": "bug"}]}"#;
        let raw: RawIssue = serde_json::from_str(json).unwrap();
        let d: IssueDraft = raw.into();
        assert_eq!(d.labels, vec!["bug".to_owned()]);
    }

    #[test]
    fn feedback_surfaces_only_unresolved_review_threads() {
        // One open review thread, one resolved, one plain comment, one system
        // note — only the first is outstanding feedback.
        let json = r#"[
          {"notes": [{"body": "fix the null deref", "author": {"username": "rev"},
                      "created_at": "2026-07-14T10:00:00Z",
                      "resolvable": true, "resolved": false, "system": false}]},
          {"notes": [{"body": "ok now", "author": {"username": "rev"},
                      "created_at": "2026-07-14T11:00:00Z",
                      "resolvable": true, "resolved": true, "system": false}]},
          {"notes": [{"body": "nice work", "author": {"username": "rev"},
                      "created_at": "2026-07-14T12:00:00Z",
                      "resolvable": false, "resolved": false, "system": false}]},
          {"notes": [{"body": "changed target branch", "author": {"username": "system"},
                      "created_at": "2026-07-14T13:00:00Z",
                      "resolvable": true, "resolved": false, "system": true}]}
        ]"#;
        let fb = feedback_from_discussions(json).unwrap();
        assert_eq!(fb.len(), 1);
        assert_eq!(fb[0].author, "rev");
        assert_eq!(fb[0].body, "fix the null deref");
        assert_eq!(fb[0].at, "2026-07-14T10:00:00Z");
    }

    #[test]
    fn empty_discussions_yield_no_feedback() {
        assert!(feedback_from_discussions("[]").unwrap().is_empty());
    }

    #[test]
    fn project_path_is_url_encoded_for_the_api() {
        assert_eq!(encoded_project("group/sub/name"), "group%2Fsub%2Fname");
        assert_eq!(encoded_project("flat"), "flat");
    }

    #[test]
    fn merged_list_maps_to_number_and_branch() {
        let json = r#"[
            {"iid": 3, "source_branch": "feat/A"},
            {"iid": 4, "source_branch": "fix/B"}
        ]"#;
        let raws: Vec<RawMr> = serde_json::from_str(json).unwrap();
        let pairs: Vec<(u64, String)> =
            raws.into_iter().map(|m| (m.iid, m.source_branch)).collect();
        assert_eq!(pairs, vec![(3, "feat/A".to_owned()), (4, "fix/B".to_owned())]);
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
