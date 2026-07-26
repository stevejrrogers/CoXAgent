//! `GhForge` — a [`ForgePort`] backed by the GitHub CLI (`gh`). Uses the CLI's
//! existing authentication on the host, so no token is stored by CoXAgent when
//! it runs on a developer machine. Every call targets an explicit `--repo`
//! slug, so it is independent of the process working directory.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{ForgePort, PullRequest};
use coxagent_application::PortError;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

/// GitHub forge for one repository (`owner/name`).
pub struct GhForge {
    repo: String,
    /// GitHub Enterprise host (empty = github.com), passed via `GH_HOST`.
    host: String,
    /// The codebase directory — `gh` runs here so its remote checks resolve.
    work_dir: PathBuf,
}

impl GhForge {
    #[must_use]
    pub fn new(repo: impl Into<String>, base_url: impl Into<String>, work_dir: PathBuf) -> Self {
        // gh takes a bare hostname; strip any scheme from a configured base URL.
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

    async fn gh(&self, args: &[&str]) -> Result<String, PortError> {
        gh(&self.host, &self.work_dir, args).await
    }
}

/// Run `gh <args>` in `work_dir`, returning trimmed stdout, or a `Backend`
/// error with stderr.
async fn gh(host: &str, work_dir: &std::path::Path, args: &[&str]) -> Result<String, PortError> {
    let mut cmd = Command::new("gh");
    cmd.args(args).stdin(Stdio::null());
    if work_dir.is_dir() {
        cmd.current_dir(work_dir);
    }
    if !host.is_empty() {
        cmd.env("GH_HOST", host);
    }
    let out = cmd
        .output()
        .await
        .map_err(|e| PortError::Backend(format!("gh spawn (is gh installed?): {e}")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(PortError::Backend(format!(
            "gh {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// gh's `pr list --json` row.
#[derive(serde::Deserialize)]
struct RawPr {
    number: u64,
    title: String,
    #[serde(default, rename = "headRefName")]
    head_ref_name: String,
    #[serde(default, rename = "baseRefName")]
    base_ref_name: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    author: RawAuthor,
    #[serde(default, rename = "createdAt")]
    created_at: String,
    #[serde(default)]
    mergeable: String,
    #[serde(default, rename = "statusCheckRollup")]
    checks: Vec<RawCheck>,
}

#[derive(serde::Deserialize, Default)]
struct RawAuthor {
    #[serde(default)]
    login: String,
}

#[derive(serde::Deserialize)]
struct RawCheck {
    #[serde(default)]
    state: String,
    #[serde(default)]
    conclusion: String,
    #[serde(default)]
    status: String,
}

/// Roll a list of individual checks up to one word for the dashboard.
fn ci_rollup(checks: &[RawCheck]) -> String {
    if checks.is_empty() {
        return "none".to_owned();
    }
    let norm = |c: &RawCheck| {
        // GitHub reports either state (CheckRun) or conclusion (StatusContext).
        let s = format!("{} {} {}", c.state, c.conclusion, c.status).to_uppercase();
        if s.contains("FAILURE") || s.contains("ERROR") || s.contains("CANCELLED") {
            "fail"
        } else if s.contains("PENDING") || s.contains("IN_PROGRESS") || s.contains("QUEUED") {
            "pending"
        } else if s.contains("SUCCESS") {
            "pass"
        } else {
            "pending"
        }
    };
    if checks.iter().any(|c| norm(c) == "fail") {
        "failing".to_owned()
    } else if checks.iter().any(|c| norm(c) == "pending") {
        "pending".to_owned()
    } else {
        "passing".to_owned()
    }
}

impl From<RawPr> for PullRequest {
    fn from(r: RawPr) -> Self {
        let ci = ci_rollup(&r.checks);
        PullRequest {
            number: r.number,
            title: r.title,
            head: r.head_ref_name,
            base: r.base_ref_name,
            url: r.url,
            author: r.author.login,
            ci,
            mergeable: r.mergeable.eq_ignore_ascii_case("MERGEABLE"),
            created: r.created_at,
        }
    }
}

const PR_FIELDS: &str =
    "number,title,headRefName,baseRefName,url,author,createdAt,mergeable,statusCheckRollup";

#[async_trait]
impl ForgePort for GhForge {
    async fn open_pr(
        &self,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PullRequest, PortError> {
        self.gh(&[
            "pr", "create", "--repo", &self.repo, "--head", head, "--base", base, "--title", title,
            "--body", body,
        ])
        .await?;
        // Fetch the freshly-created PR for the head branch to return its details.
        let json = self
            .gh(&[
                "pr", "view", head, "--repo", &self.repo, "--json", PR_FIELDS,
            ])
            .await?;
        let raw: RawPr = serde_json::from_str(&json)
            .map_err(|e| PortError::Backend(format!("gh pr view parse: {e}")))?;
        Ok(raw.into())
    }

    async fn list_open_prs(&self) -> Result<Vec<PullRequest>, PortError> {
        let json = self
            .gh(&[
                // gh caps at 30 without --limit — with a long queue the sweep
                // and WIP gate would silently see only the newest PRs.
                "pr", "list", "--repo", &self.repo, "--state", "open", "--limit", "200", "--json",
                PR_FIELDS,
            ])
            .await?;
        let raws: Vec<RawPr> = serde_json::from_str(&json)
            .map_err(|e| PortError::Backend(format!("gh pr list parse: {e}")))?;
        Ok(raws.into_iter().map(Into::into).collect())
    }

    async fn recently_merged(&self) -> Result<Vec<(u64, String)>, PortError> {
        let json = self
            .gh(&[
                "pr",
                "list",
                "--repo",
                &self.repo,
                "--state",
                "merged",
                "--limit",
                "30",
                "--json",
                "number,headRefName",
            ])
            .await?;
        let raws: Vec<serde_json::Value> = serde_json::from_str(&json)
            .map_err(|e| PortError::Backend(format!("gh merged list parse: {e}")))?;
        Ok(raws
            .into_iter()
            .filter_map(|v| {
                Some((
                    v.get("number")?.as_u64()?,
                    v.get("headRefName")?.as_str()?.to_owned(),
                ))
            })
            .collect())
    }

    async fn closed_unmerged(&self) -> Result<Vec<(u64, String)>, PortError> {
        let json = self
            .gh(&[
                "pr",
                "list",
                "--repo",
                &self.repo,
                "--state",
                "closed",
                "--limit",
                "50",
                "--json",
                "number,mergedAt,headRefName",
            ])
            .await?;
        let raws: Vec<serde_json::Value> = serde_json::from_str(&json)
            .map_err(|e| PortError::Backend(format!("gh closed list parse: {e}")))?;
        Ok(raws
            .into_iter()
            .filter(|v| v.get("mergedAt").map_or(true, serde_json::Value::is_null))
            .filter_map(|v| {
                Some((
                    v.get("number")?.as_u64()?,
                    v.get("headRefName")?.as_str()?.to_owned(),
                ))
            })
            .collect())
    }

    async fn pr_diff(&self, number: u64) -> Result<String, PortError> {
        let n = number.to_string();
        self.gh(&["pr", "diff", &n, "--repo", &self.repo]).await
    }

    async fn merge_pr(&self, number: u64) -> Result<(), PortError> {
        let n = number.to_string();
        self.gh(&[
            "pr",
            "merge",
            &n,
            "--repo",
            &self.repo,
            "--squash",
            "--delete-branch",
        ])
        .await
        .map(|_| ())
    }

    async fn request_changes(&self, number: u64, comment: &str) -> Result<(), PortError> {
        let n = number.to_string();
        self.gh(&[
            "pr",
            "review",
            &n,
            "--repo",
            &self.repo,
            "--request-changes",
            "--body",
            comment,
        ])
        .await
        .map(|_| ())
    }

    async fn close_pr(&self, number: u64) -> Result<(), PortError> {
        let n = number.to_string();
        self.gh(&["pr", "close", &n, "--repo", &self.repo])
            .await
            .map(|_| ())
    }

    async fn pr_feedback(
        &self,
        number: u64,
    ) -> Result<Vec<coxagent_application::ports::outbound::PrFeedback>, PortError> {
        let n = number.to_string();
        let raw = self
            .gh(&[
                "pr",
                "view",
                &n,
                "--repo",
                &self.repo,
                "--json",
                "reviews,commits",
            ])
            .await?;
        let v: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| PortError::Backend(format!("pr view parse: {e}")))?;
        // The branch's newest commit time: any change-request review submitted
        // AFTER it has not been addressed by a push yet.
        let last_commit = v["commits"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["committedDate"].as_str())
            .max()
            .unwrap_or("")
            .to_owned();
        let out = v["reviews"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| r["state"].as_str() == Some("CHANGES_REQUESTED"))
            .filter(|r| r["submittedAt"].as_str().unwrap_or("") > last_commit.as_str())
            .map(|r| coxagent_application::ports::outbound::PrFeedback {
                author: r["author"]["login"].as_str().unwrap_or("").to_owned(),
                body: r["body"].as_str().unwrap_or("").to_owned(),
                at: r["submittedAt"].as_str().unwrap_or("").to_owned(),
            })
            .filter(|f| !f.body.trim().is_empty())
            .collect();
        Ok(out)
    }

    async fn comment_pr(&self, number: u64, body: &str) -> Result<(), PortError> {
        let n = number.to_string();
        self.gh(&["pr", "comment", &n, "--repo", &self.repo, "--body", body])
            .await
            .map(|_| ())
    }
}
