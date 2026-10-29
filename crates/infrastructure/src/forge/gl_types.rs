//! Wire types and pure mapping for [`super::gl_forge`] — the GitLab API JSON
//! shapes (`glab … -F json` / `glab api`) and the functions that turn them into
//! the domain's [`PullRequest`] / [`IssueDraft`] / [`PrFeedback`]. Kept apart
//! from the adapter so the mapping is testable with plain struct literals and
//! the adapter file stays about I/O.

use coxagent_application::ports::outbound::{IssueDraft, PrFeedback, PullRequest};
use coxagent_application::PortError;

/// One merge request from `glab mr list -F json` (the GitLab API MR object).
#[derive(serde::Deserialize)]
pub(super) struct RawMr {
    pub(super) iid: u64,
    #[serde(default)]
    pub(super) title: String,
    #[serde(default)]
    pub(super) source_branch: String,
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
pub(super) fn feedback_from_discussions(json: &str) -> Result<Vec<PrFeedback>, PortError> {
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
pub(super) fn encoded_project(repo: &str) -> String {
    repo.replace('/', "%2F")
}

/// One issue from `glab issue list -F json` (the GitLab API issue object).
#[derive(serde::Deserialize)]
pub(super) struct RawIssue {
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
