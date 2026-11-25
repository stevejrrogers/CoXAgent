//! CXA-F258 — Brownfield adoption: import an existing GitHub issue backlog
//! into Pending. The TDD suite the SA design specifies: pure mapper cases +
//! gh-JSON fixture parse, NO network, NO server, NO harness.
//!
//! SHIPPED MECHANISM (per the SA design — "reuse ForgePort; no new endpoint,
//! auth, or storage"):
//!   1. `ForgePort::list_open_issues(limit)` (+ `IssueDraft`), default empty —
//!      `ports/outbound/forge.rs`; GitLab adapter untouched (defaults cover it).
//!   2. `GhForge` implements it via `gh issue list --repo --state open --limit
//!      N --json number,title,body,labels,url`; `GhApiForge` (the token-only
//!      fallback the `github_forge` factory can select) implements it over the
//!      REST issues endpoint, PR rows filtered out.
//!   3. `application::backlog_import::merge_pending(&mut ProjectState,
//!      &[IssueDraft], cap)` — PURE: dedupe + map + cap, inline unit tests.
//!   4. `onboard::brownfield` wires it: when the adopted repo is connected on
//!      GitHub, fetch OPEN issues, merge, one store.save, report the counts.
//!
//! AC COVERAGE AND WHAT IS DELIBERATELY NOT HERE:
//!   * AC2 (Pending tickets with title/body/back-link) — pinned purely below.
//!   * AC3 (idempotent re-run, imported vs skipped report) — pinned purely
//!     below, deduped on the source issue's URL, not just the title.
//!   * AC4 (closed excluded by default) — the brownfield path fetches OPEN
//!     issues only (source guard below); the port's `list_closed_issues`
//!     exists so an opt-in surface can pass closed drafts through the same
//!     pure mapper (they land Pending, outside the claim pipeline — the
//!     `imports_stay_outside_the_agent_claim_pipeline` guard covers that
//!     landing for ANY draft).
//!   * AC1 (preview as a selectable list) and AC5 (write-capable-only
//!     surface): the SA design says "no new endpoint", which leaves no
//!     preview/import surface — that placement question is with the SA (asked,
//!     unanswered). No UI/endpoint guards are pinned here until it is answered;
//!     inventing a surface would contradict the design.
//!
//! Design deviation flagged to the SA (implemented the only way the domain
//! allows): the design's `bug label → TicketType::Bug` would land imports
//! `Open` (domain invariant — no `Open -> Pending` edge exists for any actor)
//! and `Open` bugs sit in `selection::open_bug_candidates`, the agent claim
//! queue. That would violate the design's own "Status::Pending", AC2's
//! "creates Pending tickets", and the never-auto-enter-the-claim-pipeline
//! edge case. The `bug` label is preserved in the ticket body for triage
//! instead.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_application::backlog_import::{merge_pending, ImportReport, IMPORT_CAP};
use coxagent_application::ports::outbound::IssueDraft;
use coxagent_application::selection::{design_candidates, ready_feature_candidates};
use coxagent_application::state::ProjectState;
use coxagent_domain::{Role, Status};

/// Fixed RFC3339 instant for the claim attempt (deterministic fixtures).
const NOW: &str = "2026-09-05T00:00:00Z";

/// `gh issue list --repo acme/widgets --state open --limit 2 --json
/// number,title,body,labels,url` — the fixture the SA design calls for. Note
/// the two label shapes gh has shipped across versions: objects and strings.
const GH_ISSUE_LIST_JSON: &str = r#"[
  {
    "number": 41,
    "title": "Add CSV export to reports",
    "body": "Reports need CSV output.\n\nSteps:\n1. open reports\n2. click export",
    "labels": [{"id": 1, "name": "enhancement"}],
    "url": "https://github.com/acme/widgets/issues/41"
  },
  {
    "number": 42,
    "title": "Login page 500s on empty password",
    "body": "",
    "labels": ["bug"],
    "url": "https://github.com/acme/widgets/issues/42"
  }
]"#;

/// AC1's data source, parsed from the gh fixture: the adapter's JSON half
/// yields drafts carrying number/title/body/labels/url — everything the
/// preview list and the mapper need — with no network and no `gh` binary.
#[test]
fn gh_issue_list_json_parses_into_issue_drafts() {
    let drafts = coxagent_infrastructure::forge::parse_issue_json(GH_ISSUE_LIST_JSON)
        .expect("gh issue list output parses");

    assert_eq!(drafts.len(), 2);
    assert_eq!(drafts[0].number, 41);
    assert_eq!(drafts[0].title, "Add CSV export to reports");
    assert!(
        drafts[0].body.contains("Reports need CSV"),
        "the issue body is carried for the ticket"
    );
    assert_eq!(drafts[0].labels, vec!["enhancement"], "object labels parse");
    assert_eq!(
        drafts[0].url, "https://github.com/acme/widgets/issues/41",
        "the back-link identity is carried"
    );
    assert_eq!(drafts[1].labels, vec!["bug"], "string labels parse too");
    assert_eq!(
        drafts[1].body, "",
        "an empty body stays empty, not an error"
    );
}

/// The port→mapper slice, purely: the fixture drafts from `gh` merge into a
/// project state as Pending feature tickets carrying the issue title, body
/// and the clickable back-link (AC2).
#[test]
fn the_gh_fixture_backlog_imports_as_pending_tickets_with_backlinks() {
    let drafts =
        coxagent_infrastructure::forge::parse_issue_json(GH_ISSUE_LIST_JSON).expect("parse");
    let mut state = ProjectState {
        alias: "ACME".to_owned(),
        ..ProjectState::default()
    };

    let report = merge_pending(&mut state, &drafts, IMPORT_CAP);

    assert_eq!(
        report,
        ImportReport {
            imported: 2,
            skipped: 0
        }
    );
    assert_eq!(state.tickets.len(), 2);
    let t = &state.tickets[0];
    assert_eq!(t.status(), Status::Pending, "AC2: imports land Pending");
    assert_eq!(t.title(), "Add CSV export to reports");
    assert!(
        t.description().contains("Reports need CSV"),
        "the issue body is carried"
    );
    assert!(
        t.description()
            .contains("https://github.com/acme/widgets/issues/41"),
        "AC2: a clickable link back to the source issue"
    );
}

/// AC3, end to end over the fixture: re-running the import for the same repo
/// creates no duplicate tickets and reports imported vs skipped.
#[test]
fn rerunning_the_same_repo_import_is_idempotent_with_counts() {
    let drafts =
        coxagent_infrastructure::forge::parse_issue_json(GH_ISSUE_LIST_JSON).expect("parse");
    let mut state = ProjectState::default();

    let first = merge_pending(&mut state, &drafts, IMPORT_CAP);
    assert_eq!(first.imported, 2);

    let second = merge_pending(&mut state, &drafts, IMPORT_CAP);
    assert_eq!(
        second,
        ImportReport {
            imported: 0,
            skipped: 2
        },
        "AC3: no duplicates, and the report says how many were skipped"
    );
    assert_eq!(state.tickets.len(), 2);
}

/// AC4's landing guarantee for ANY draft (open today, closed via a future
/// opt-in surface): the imported ticket stands outside the agent claim
/// pipeline — unclaimed, absent from the DEV claim queue, and the claim
/// itself is refused (no `Pending -> InProgress` edge in the transition
/// table). It surfaces in the SA design queue, the ordinary Pending flow.
#[test]
fn imports_stay_outside_the_agent_claim_pipeline() {
    let mut state = ProjectState::default();
    let report = merge_pending(
        &mut state,
        // A closed-issue draft exactly as an opt-in surface would pass it.
        &[IssueDraft {
            number: 33,
            title: "Closed: dark mode for the dashboard".to_owned(),
            body: "closed when v1 shipped the theme toggle".to_owned(),
            labels: vec!["closed-as-done".to_owned()],
            url: "https://github.com/acme/widgets/issues/33".to_owned(),
        }],
        IMPORT_CAP,
    );
    assert_eq!(report.imported, 1);

    let t = state.tickets.first().expect("imported ticket").clone();
    assert_eq!(t.status(), Status::Pending);
    assert_eq!(t.claimed_by(), None, "nothing claimed the import");
    assert!(
        !ready_feature_candidates(&state).contains(t.id()),
        "the DEV claim queue admits Ready only — an import is not claimable"
    );
    assert!(
        design_candidates(&state).contains(t.id()),
        "the SA design queue is the ordinary Pending flow imports feed"
    );
    let mut claim_attempt = t.clone();
    assert!(
        claim_attempt.claim(Role::System, "qa@host", NOW).is_err(),
        "the claim itself is refused: no Pending -> InProgress edge"
    );
}

/// AC4's default half, at the wiring the design specifies: the brownfield
/// adoption path fetches OPEN issues only — closed issues are excluded by
/// default because the default path never asks for them. (`list_closed_issues`
/// exists on the port for an opt-in surface; nothing calls it until the SA
/// answers where that surface lives — see the header.)
#[test]
fn the_brownfield_wiring_fetches_open_issues_only_by_default() {
    let onboard = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/onboard.rs"))
        .expect("onboard.rs is served from the app crate");

    assert!(
        onboard.contains("list_open_issues"),
        "brownfield adoption must fetch the connected repo's open issues \
         through the port (CXA-F258 design step 4)"
    );
    assert!(
        !onboard.contains("list_closed_issues"),
        "the adoption path must NOT fetch closed issues — AC4 excludes them \
         by default; an opt-in needs an operator surface (with the SA)"
    );
    assert!(
        onboard.contains("merge_pending"),
        "the import must merge through the pure mapper, not ad-hoc state edits"
    );
    assert!(
        onboard.contains("import_backlog(store"),
        "brownfield() must wire the import so adoption pulls the real backlog"
    );
}
