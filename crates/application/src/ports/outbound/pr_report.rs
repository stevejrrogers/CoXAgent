//! `PrReporterPort` — how a runner tells the hub (via HTTP, never via the
//! database directly) about the pull requests it has opened and the reviews
//! the SA agent rendered on them.
//!
//! The runner holds the forge credentials (the `gh` CLI + its keychain token on
//! the runner's own machine) and is therefore the only process that can list
//! open PRs and produce review verdicts. The web dashboard, by contrast, runs
//! anywhere (possibly in a container with no `gh` and no token). So the truth
//! must travel from the runner to the hub over HTTP — the runner POSTs PR and
//! review events, and reads back the reviews it needs to avoid re-reviewing
//! the same head. The hub is the single writer to persistent state.
//!
//! The adapter is constructed per project and already knows which project it
//! reports for, so no project id is passed per call.
//!
//! Implementations are HTTP adapters (see `crates/infrastructure/src/pr_report.rs`).
//! This port lives in `application` and never performs I/O itself.

use crate::state::PrReview;
use async_trait::async_trait;

/// A pull request as reported by the runner, for persistence on the hub.
///
/// Mirrors the `forge::PullRequest` shape so the runner can hand it over
/// without a second mapping, but lives in the state layer because the hub may
/// persist it for any number of requesting dashboards.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PrOpen {
    pub number: u64,
    pub title: String,
    pub head: String,
    pub base: String,
    pub url: String,
    pub author: String,
    pub ci: String,
    pub mergeable: bool,
    pub created: String,
}

impl From<crate::ports::outbound::PullRequest> for PrOpen {
    fn from(pr: crate::ports::outbound::PullRequest) -> Self {
        Self {
            number: pr.number,
            title: pr.title,
            head: pr.head,
            base: pr.base,
            url: pr.url,
            author: pr.author,
            ci: pr.ci,
            mergeable: pr.mergeable,
            created: pr.created,
        }
    }
}

/// Outbound boundary for reporting PR activity from a runner to the hub.
#[async_trait]
pub trait PrReporterPort: Send + Sync {
    /// The runner has opened (or refreshed) `pr` — persist it.
    async fn report_pr(&self, pr: PrOpen);

    /// The runner rendered a review verdict on a PR — persist it.
    async fn report_review(&self, number: u64, decision: &str, summary: &str, head_sha: &str);

    /// Fetch the reviews currently persisted, so the runner can avoid
    /// re-reviewing a head it already marked `request_changes`.
    async fn fetch_reviews(&self) -> Vec<PrReview>;
}

/// A reporter that drops everything — used when no hub is configured or in
/// tests that do not care. Keeps the runner working with git disabled.
pub struct NullPrReporter;

#[async_trait]
impl PrReporterPort for NullPrReporter {
    async fn report_pr(&self, _pr: PrOpen) {}
    async fn report_review(&self, _number: u64, _decision: &str, _summary: &str, _head_sha: &str) {}
    async fn fetch_reviews(&self) -> Vec<PrReview> {
        Vec::new()
    }
}
