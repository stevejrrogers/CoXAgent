//! CXA-F312 — Verify and merge CXA-F248 as a single slice off feat/CXA-B121.
//! Acceptance gate.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "After the merge, on a shipped ticket opened in the reviewer panel,
//!    'api'-kind and screenshot-kind evidence entries that have a QA verdict
//!    route display a clickable reproduction link that resolves against the
//!    live hub deploy base and opens the recorded reproduction (per criterion,
//!    wherever a route exists)."
//! 2. "Evidence whose route cannot be mapped onto the known live deploy base
//!    shows NO hyperlink at all — it is omitted or flagged rather than rendered
//!    as a guessed or dead link (checkable by opening a ticket with an
//!    unresolvable route and asserting no anchor is emitted)."
//! 3. "The full e2e gate is green on the merged result: `cd e2e && npx
//!    playwright test` passes against the ephemeral hub with the committed
//!    golden screenshots (no snapshot updates) and zero console errors,
//!    including a spec that covers the evidence repro-link rendering."
//! 4. "The complete CXA-F248 delta lands on main as exactly ONE merge off
//!    feat/CXA-B121 (no slice split out or left behind), and the landed
//!    commit/PR metadata names CXA-F248 truthfully instead of the unrelated
//!    'loading skeletons' title, so the landed history matches the shipped
//!    behaviour."
//! 5. "Post-merge verification leaves main's local gate suite green (cargo fmt
//!    --check, clippy --workspace --all-targets, cargo test --workspace,
//!    hexagonal_gate) apart only from the documented pre-existing red list
//!    (CXA-B037 engine-spawn timeouts), with any other failure triaged before
//!    the merge is declared done."
//!
//! WHERE THE SUBJECTS LIVE (this tree): the per-criterion evidence record is
//! `CaseEvidence.repro` (`domain/src/test_case.rs`), written by the verdict
//! consumption site (`use_cases/coverage.rs::record_verdicts` → `apply_verdict`
//! → `live_repro_url`) from the TEST agent's `TestVerdict.route` fed the
//! deploy base by `run_test.rs`; the reviewer panel renders it
//! (`web/js/chat.js` `tc.evidence…tcrepro` anchor); the e2e suite covers it
//! (`e2e/specs/evidence-repro.spec.ts` + committed golden, run against the
//! ephemeral hub booted by `e2e/playwright.config.ts`).
//!
//! GUARD STYLE: pure functions over the real domain/application types where
//! the behaviour is Rust (no server, no harness, no port), and repo-state
//! scans (file contents, git history) for the surfaces without an executable
//! seam — the established convention of this tree's acceptance gates. The git
//! queries are read-only (`log`, `rev-parse`, `merge-base --is-ancestor`,
//! `ls-files`), the same house pattern as `committed_secrets_gate.rs`.
//!
//! AC → test map:
//! - AC1: [`ac1_api_and_screenshot_evidence_with_a_verdict_route_show_the_clickable_repro_link_per_criterion`],
//!   [`ac1_the_reviewer_panel_anchor_opens_the_recorded_reproduction`]
//! - AC2: [`ac2_an_unresolvable_route_is_omitted_never_a_guessed_or_dead_link`],
//!   [`ac2_opening_a_ticket_with_an_unresolvable_route_emits_no_anchor`]
//! - AC3: [`ac3_the_e2e_gate_covers_the_repro_link_with_a_committed_golden_and_no_snapshot_updates`]
//! - AC4: [`ac4_the_complete_f248_delta_lands_as_exactly_one_truthful_merge_off_feat_cxa_b121`]
//! - AC5: [`ac5_post_merge_gates_stay_green_apart_only_from_the_documented_b037_red_list`]

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

use coxagent_application::parsing::TestVerdict;
use coxagent_application::state::ProjectState;
use coxagent_application::use_cases::coverage::{live_repro_url, record_verdicts};
use coxagent_domain::{Complexity, Priority, Ticket, TicketId, TicketType};

/// The ticket under test.
const TICKET: &str = "CXA-F248";

/// The resolved repro field AC1 names on the per-criterion evidence record.
const FIELD: &str = "repro";

/// The unrelated title the F248 delta must stop riding under (AC4): the
/// 'loading skeletons' fix whose commit/PR metadata today carries the F248
/// behaviour.
const MISLABELED_TITLE_FRAGMENT: &str = "skeleton";

/// The branch the single slice must be cut off (AC4).
const BASE_BRANCH: &str = "feat/CXA-B121";

/// The canonical F248 delta footprint: every file whose content IS the
/// behaviour, with one marker each. AC4's "no slice left behind" = all of it
/// present on main.
const F248_FOOTPRINT: &[(&str, &str)] = &[
    (
        "crates/domain/src/test_case.rs",
        "pub repro: Option<String>",
    ),
    (
        "crates/application/src/use_cases/coverage.rs",
        "pub fn live_repro_url",
    ),
    (
        "crates/application/src/use_cases/run_test.rs",
        "config.deploy.host_port",
    ),
    (
        "crates/presentation/src/web/js/chat.js",
        "class=\"tcrepro\"",
    ),
    ("crates/presentation/src/web/app.css", "tcrepro"),
    ("e2e/specs/evidence-repro.spec.ts", FIELD),
    ("crates/app/tests/evidence_repro_routes_f248_tdd.rs", TICKET),
];

/// The documented pre-existing red list (AC5): the CXA-B037 engine-spawn
/// timeout pair, its wiki page, and the only failures the post-merge gate
/// suite may show without triage.
const RED_LIST_DOC: &str = "docs/wiki/engineering/testing/cxa-b037-tests-failing.md";
const RED_LIST_TESTS: &[(&str, &str)] = &[
    (
        "crates/infrastructure/src/engine/claude.rs",
        "run_passes_mcp_config_flag_to_the_real_spawn",
    ),
    (
        "crates/infrastructure/src/engine/opencode.rs",
        "run_writes_cox_config_and_exports_opencode_config_env",
    ),
];

// --- repo-state scan helpers --------------------------------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The source with ALL whitespace removed, so a guard survives the view code's
/// dense one-line formatting style without pinning its line breaks.
fn flat(src: &str) -> String {
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Read-only git query against the repo under test, failing loudly.
fn git(args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo_root())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("git output is utf8")
}

/// Boolean git query (`merge-base --is-ancestor` uses the exit code).
fn git_ok(args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(repo_root())
        .status()
        .is_ok_and(|s| s.success())
}

/// One merge commit reachable from HEAD.
struct Landing {
    sha: String,
    parents: Vec<String>,
    subject: String,
    body: String,
}

/// Every merge commit on the HEAD lineage, newest first, parsed from
/// `%H%x1f%P%x1f%B%x1e` records.
fn merges() -> Vec<Landing> {
    let raw = git(&["log", "--merges", "--format=%H%x1f%P%x1f%B%x1e", "HEAD"]);
    raw.split('\u{1e}')
        .filter(|entry| !entry.trim().is_empty())
        .map(|entry| {
            let mut parts = entry.splitn(3, '\u{1f}');
            let sha = parts.next().unwrap_or("").trim().to_owned();
            let parents = parts
                .next()
                .unwrap_or("")
                .split_whitespace()
                .map(str::to_owned)
                .collect();
            let body = parts.next().unwrap_or("").to_owned();
            let subject = body.lines().next().unwrap_or("").to_owned();
            Landing {
                sha,
                parents,
                subject,
                body,
            }
        })
        .collect()
}

// --- AC1 ---------------------------------------------------------------------

/// AC1: "'api'-kind and screenshot-kind evidence entries that have a QA
/// verdict route display a clickable reproduction link that resolves against
/// the live hub deploy base ... (per criterion, wherever a route exists)."
///
/// Pure half over the real types: a shipped ticket's criteria, two TEST
/// verdicts — one proven by an API request/response (api-kind), one by a
/// captured screenshot (screenshot-kind) — each carrying its route, recorded
/// through the verdict consumption site with the hub deploy base
/// (`config.deploy.host_port`). Each criterion's evidence must end up with
/// its OWN resolved repro on the deployed app's base.
#[test]
fn ac1_api_and_screenshot_evidence_with_a_verdict_route_show_the_clickable_repro_link_per_criterion(
) {
    const AC_API: &str = "Toggling autosave shows a saved indicator";
    const AC_SHOT: &str = "Reloading keeps the toggle state";
    const AC_NEITHER: &str = "Autosave survives a slow network";

    let mut state = ProjectState::default();
    let mut ticket = Ticket::new(
        TicketId::new("F248").expect("id"),
        TicketType::Feature,
        "Settings autosave indicator",
        "autosave",
        Priority::Medium,
        Complexity::Small,
        false,
    )
    .expect("ticket");
    ticket.set_acceptance_criteria(vec![
        AC_API.to_owned(),
        AC_SHOT.to_owned(),
        AC_NEITHER.to_owned(),
    ]);
    state.tickets.push(ticket);

    let api_verdict = TestVerdict {
        ac: AC_API.to_owned(),
        passed: true,
        note: "GET /settings 200 {\"autosave\":true}".to_owned(),
        route: "/settings".to_owned(),
        tests: vec![],
    };
    let screenshot_verdict = TestVerdict {
        ac: AC_SHOT.to_owned(),
        passed: true,
        note: "captured /settings after reload — saved indicator persists".to_owned(),
        route: "/settings#autosave".to_owned(),
        tests: vec![],
    };

    assert!(
        record_verdicts(
            &mut state,
            &[api_verdict, screenshot_verdict],
            "2026-09-02T00:00:00Z",
            Some(8101),
        ),
        "recording the two routed verdicts must change the ticket"
    );

    let cases = state.tickets[0].test_cases();
    let repro_of = |i: usize| cases[i].evidence.as_ref().and_then(|e| e.repro.as_deref());
    assert_eq!(
        repro_of(0),
        Some("http://127.0.0.1:8101/settings"),
        "api-kind evidence with a QA verdict route must carry its own \
         reproduction link resolved against the live hub deploy base"
    );
    assert_eq!(
        repro_of(1),
        Some("http://127.0.0.1:8101/settings#autosave"),
        "screenshot-kind evidence with a QA verdict route must carry its own \
         reproduction link resolved against the live hub deploy base"
    );
    assert_eq!(
        repro_of(2),
        None,
        "a criterion with no route shows no reproduction link (AC1: only \
         'wherever a route exists')"
    );
}

/// AC1: "...display a clickable reproduction link that ... opens the recorded
/// reproduction". The reviewer panel's test-case render must emit an anchor
/// whose href IS the recorded repro (escaped), opening it in a new tab — and
/// only where the field is present, per criterion.
#[test]
fn ac1_the_reviewer_panel_anchor_opens_the_recorded_reproduction() {
    let chat = flat(&read("crates/presentation/src/web/js/chat.js"));
    let at = chat.find("class=\"tcrepro\"").unwrap_or_else(|| {
        panic!(
            "the reviewer panel no longer renders the evidence \
             reproduction link as a `tcrepro` anchor — point this guard at the \
             review panel's test-case render"
        )
    });
    // A context window for the assert messages, byte-sliced safely: clamp to
    // the end and walk both offsets onto char boundaries (the flat source may
    // contain multi-byte characters), or the guard itself panics before it can
    // report the drift it exists to catch.
    let mut start = at.saturating_sub(200);
    while !chat.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = at.saturating_add(400).min(chat.len());
    while !chat.is_char_boundary(end) {
        end -= 1;
    }
    let anchor = &chat[start..end];
    assert!(
        anchor.contains("tc.evidence&&tc.evidence.repro?"),
        "the reproduction link must be driven per criterion by the evidence's \
         resolved `{FIELD}` — window: {anchor}"
    );
    assert!(
        anchor.contains("href=\"${esc(tc.evidence.repro)}\""),
        "the anchor must open the RECORDED reproduction (the evidence's \
         resolved url, escaped) as its href — window: {anchor}"
    );
    assert!(
        anchor.contains("target=\"_blank\"") && anchor.contains("rel=\"noopener\""),
        "the reproduction link must open in a new tab without leaking the \
         opener — window: {anchor}"
    );
}

// --- AC2 ---------------------------------------------------------------------

/// AC2: "Evidence whose route cannot be mapped onto the known live deploy base
/// shows NO hyperlink at all — it is omitted ... rather than rendered as a
/// guessed or dead link". Pure half over the real types: the resolver refuses
/// every unmappable shape, the record never carries a guessed link, and the
/// domain refuses a blank write outright.
#[test]
fn ac2_an_unresolvable_route_is_omitted_never_a_guessed_or_dead_link() {
    const AC: &str = "Toggling autosave shows a saved indicator";

    // The resolver: no base, no path, no link. Never a guess.
    assert_eq!(
        live_repro_url("/settings", None),
        None,
        "no configured live deploy base — nothing to resolve onto"
    );
    assert_eq!(live_repro_url("", Some(8101)), None, "no route at all");
    assert_eq!(
        live_repro_url("   ", Some(8101)),
        None,
        "a blank route is not a path"
    );
    assert_eq!(
        live_repro_url("settings", Some(8101)),
        None,
        "a bare word is not an app path"
    );
    assert_eq!(
        live_repro_url("http://evil.example/settings", Some(8101)),
        None,
        "an agent-invented absolute URL is never turned into a hyperlink"
    );
    assert_eq!(
        live_repro_url("/settings\"><script>", Some(8101)),
        None,
        "a route that cannot survive inside a quoted attribute never resolves"
    );

    // The record: a verdict whose route cannot be mapped leaves the case's
    // evidence link-free — omitted, not fabricated.
    let mut state = ProjectState::default();
    let mut ticket = Ticket::new(
        TicketId::new("F248").expect("id"),
        TicketType::Feature,
        "Settings autosave indicator",
        "autosave",
        Priority::Medium,
        Complexity::Small,
        false,
    )
    .expect("ticket");
    ticket.set_acceptance_criteria(vec![AC.to_owned()]);
    state.tickets.push(ticket);
    let unroutable = TestVerdict {
        ac: AC.to_owned(),
        passed: true,
        note: "verified, but the walked URL is off the deployed app".to_owned(),
        route: "http://evil.example/settings".to_owned(),
        tests: vec![],
    };
    record_verdicts(
        &mut state,
        &[unroutable],
        "2026-09-02T00:00:00Z",
        Some(8101),
    );
    let evidence = state.tickets[0].test_cases()[0]
        .evidence
        .as_ref()
        .expect("the verdict's note still records evidence");
    assert!(
        evidence.repro.is_none(),
        "an unresolvable route must leave `{FIELD}` absent on the record — got {:?}",
        evidence.repro
    );

    // The domain: even a buggy caller cannot materialize a placeholder link.
    let mut ticket = Ticket::new(
        TicketId::new("F248").expect("id"),
        TicketType::Feature,
        "t",
        "d",
        Priority::Medium,
        Complexity::Small,
        false,
    )
    .expect("ticket");
    ticket.set_acceptance_criteria(vec![AC.to_owned()]);
    ticket.ensure_test_cases_from_acceptance();
    assert!(
        !ticket.set_test_case_repro(AC, "   ".into()),
        "a blank write is refused outright"
    );
    assert!(
        ticket.test_cases()[0]
            .evidence
            .as_ref()
            .is_none_or(|e| e.repro.is_none()),
        "a refused blank write leaves the field absent — no placeholder link"
    );
}

/// AC2: "...(checkable by opening a ticket with an unresolvable route and
/// asserting no anchor is emitted)". The checkable path must exist in the
/// shipped fixture data — a case whose evidence is present but whose resolved
/// route is absent — and the reviewer panel must emit NO anchor for it: the
/// render is guarded by the field's presence, and the covering e2e spec
/// asserts the zero-anchor case.
#[test]
fn ac2_opening_a_ticket_with_an_unresolvable_route_emits_no_anchor() {
    // The fixture data the reviewer opens: at least one evidenced case with no
    // resolved repro (the unresolvable-route shape on the wire).
    let fixture = read("e2e/fixtures/state/state.json");
    let shape_exists = serde_json::from_str::<serde_json::Value>(&fixture)
        .expect("the e2e state fixture is valid JSON")
        .get("tickets")
        .and_then(|t| t.as_array())
        .is_some_and(|tickets| {
            tickets.iter().any(|t| {
                t.get("test_cases")
                    .and_then(|c| c.as_array())
                    .is_some_and(|cases| {
                        cases.iter().any(|c| {
                            c.get("evidence")
                                .and_then(|e| e.as_object())
                                .is_some_and(|ev| ev.get(FIELD).is_none())
                        })
                    })
            })
        });
    assert!(
        shape_exists,
        "no fixture ticket carries evidence WITHOUT a resolved `{FIELD}` — the \
         'open a ticket with an unresolvable route and assert no anchor' check \
         has nothing to open; point this guard at the fixture that demonstrates it"
    );

    // The render: no anchor unless the field is present.
    let chat = flat(&read("crates/presentation/src/web/js/chat.js"));
    assert!(
        chat.contains("tc.evidence&&tc.evidence.repro?"),
        "the reproduction anchor must be emitted only when the resolved \
         `{FIELD}` is present — an unresolvable route must produce NO \
         hyperlink at all"
    );

    // The e2e proof: the covering spec asserts a zero-anchor case.
    let spec = read("e2e/specs/evidence-repro.spec.ts");
    assert!(
        spec.contains("toHaveCount(0)"),
        "the evidence repro spec must assert a case renders NO anchor (the \
         no-fabrication half is only proven when the absence is asserted)"
    );
}

// --- AC3 ---------------------------------------------------------------------

/// AC3: "The full e2e gate is green on the merged result: `cd e2e && npx
/// playwright test` passes against the ephemeral hub with the committed golden
/// screenshots (no snapshot updates) and zero console errors, including a spec
/// that covers the evidence repro-link rendering."
///
/// The suite itself runs only against the ephemeral hub (`e2e/
/// playwright.config.ts` boots one); this guard pins everything an in-process
/// test can pin: an unparked spec covers the repro-link rendering, asserts
/// zero console errors, pins its golden, the golden is COMMITTED (never
/// regenerated), and the CI compare never rewrites snapshots.
#[test]
fn ac3_the_e2e_gate_covers_the_repro_link_with_a_committed_golden_and_no_snapshot_updates() {
    let spec = read("e2e/specs/evidence-repro.spec.ts");
    assert!(
        spec.contains(FIELD) && spec.to_lowercase().contains("evidence"),
        "no e2e spec covers the evidence repro-link rendering — AC3's run \
         cannot demonstrate the merged review-panel behaviour without one"
    );
    assert!(
        !spec.contains(".skip") && !spec.contains(".fixme") && !spec.contains(".only("),
        "the evidence repro spec must run unparked against the ephemeral hub"
    );
    assert!(
        spec.contains("armConsoleGate") && spec.contains("assertNoConsoleErrors"),
        "the evidence repro spec must assert zero console errors (the \
         armConsoleGate + assertNoConsoleErrors pair)"
    );
    assert!(
        spec.contains("toHaveScreenshot"),
        "the evidence repro spec must pin the repro-link visuals to a golden \
         screenshot"
    );

    // The golden is COMMITTED — the suite runs against it, never regenerates it.
    let tracked = git(&["ls-files", "--", "e2e/specs"]);
    assert!(
        tracked
            .lines()
            .any(|f| f.contains("evidence-repro.spec.ts-snapshots/")),
        "the evidence repro golden must be committed (tracked in git) so the \
         gate compares against it instead of updating snapshots"
    );

    // "no snapshot updates": the CI compare never rewrites committed goldens.
    let ci = read(".github/workflows/visual-qa.yml");
    assert!(
        ci.contains("--update-snapshots=off"),
        "the visual QA compare must keep goldens immutable \
         (--update-snapshots=off) so a green e2e run never ships snapshot \
         updates"
    );
    let cfg = read("e2e/playwright.config.ts");
    assert!(
        !cfg.contains("updateSnapshots"),
        "the e2e config must not opt into snapshot rewrites"
    );
}

// --- AC4 ---------------------------------------------------------------------

/// AC4: "The complete CXA-F248 delta lands on main as exactly ONE merge off
/// feat/CXA-B121 (no slice split out or left behind), and the landed commit/PR
/// metadata names CXA-F248 truthfully instead of the unrelated 'loading
/// skeletons' title, so the landed history matches the shipped behaviour."
///
/// Pure git-history scan (read-only): the complete footprint is on main,
/// exactly ONE merge on the lineage names CXA-F248, that merge is cut off
/// feat/CXA-B121, its metadata is the F248 landing (not the skeletons title),
/// and the delta's introducing commit rides in its branch line.
#[test]
fn ac4_the_complete_f248_delta_lands_as_exactly_one_truthful_merge_off_feat_cxa_b121() {
    // "no slice left behind": the complete delta is on main.
    for (rel, marker) in F248_FOOTPRINT {
        assert!(
            read(rel).contains(marker),
            "the CXA-F248 delta is incomplete on main: {rel} is missing its \
             `{marker}` marker — a slice was left behind"
        );
    }
    let golden_dir = repo_root().join("e2e/specs/evidence-repro.spec.ts-snapshots");
    assert!(
        golden_dir.is_dir()
            && std::fs::read_dir(&golden_dir)
                .expect("list the golden dir")
                .next()
                .is_some(),
        "the CXA-F248 delta is incomplete on main: the committed golden \
             screenshots are missing"
    );

    // "exactly ONE merge ... the landed commit/PR metadata names CXA-F248
    // truthfully instead of the unrelated 'loading skeletons' title".
    let named: Vec<Landing> = merges()
        .into_iter()
        .filter(|m| m.body.contains(TICKET))
        .collect();
    assert_eq!(
        named.len(),
        1,
        "the complete CXA-F248 delta must land on main as EXACTLY ONE merge \
         whose metadata names {TICKET} — found {}: {}. Today the delta rides \
         inside the unrelated 'loading skeletons' landing instead of a \
         truthfully named slice.",
        named.len(),
        named
            .iter()
            .map(|m| m.subject.as_str())
            .collect::<Vec<_>>()
            .join(" | ")
    );
    let landing = &named[0];
    assert_eq!(
        landing.parents.len(),
        2,
        "the {TICKET} landing must be a two-parent merge (main × the slice) — \
             got {} parents",
        landing.parents.len()
    );
    assert!(
        landing.subject.contains(TICKET),
        "the landed merge's subject must name {TICKET} truthfully — subject: {}",
        landing.subject
    );
    assert!(
        !landing
            .subject
            .to_lowercase()
            .contains(MISLABELED_TITLE_FRAGMENT),
        "the landed merge's subject must not carry the unrelated 'loading \
             skeletons' title — subject: {}",
        landing.subject
    );

    // "...as exactly ONE merge OFF feat/CXA-B121": the slice is cut from the
    // B121 line. The branch is a LIVE ref that gets deleted once its line is
    // fully landed — after that the ancestry below is unverifiable, while the
    // merge-truth this test exists for stays pinned by the assertions above.
    // Without this guard the deleted branch turned the whole workspace suite
    // red for every fresh clone and every agent worktree (CXA-B166: a full
    // evening of "tests red" gate bounces traced back here).
    if !git_ok(&["rev-parse", "--verify", &format!("{BASE_BRANCH}^{{commit}}")]) {
        eprintln!(
            "skipping ancestry check: {BASE_BRANCH} no longer resolves \
             (deleted after landing); the exactly-one-truthful-merge \
             assertions above still hold"
        );
        return;
    }
    let base_tip = git(&["rev-parse", BASE_BRANCH]);
    let base_tip = base_tip.trim();
    assert!(
        !base_tip.is_empty(),
        "{BASE_BRANCH} must resolve — point this guard at the branch the slice is cut from"
    );
    assert!(
        git_ok(&["merge-base", "--is-ancestor", base_tip, &landing.parents[1]]),
        "the {TICKET} merge must be cut off {BASE_BRANCH}: its tip {base_tip} \
             must be an ancestor of the merge's branch parent {}",
        landing.parents[1]
    );

    // "(no slice split out ...)": the delta's introducing commit rides in the
    // landing's branch line, not in a second slice.
    let introductions = git(&[
        "log",
        "--diff-filter=A",
        "--format=%H",
        "--",
        "crates/app/tests/evidence_repro_routes_f248_tdd.rs",
    ]);
    assert!(
        introductions
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .any(|sha| git_ok(&["merge-base", "--is-ancestor", sha, &landing.sha])),
        "the commit introducing the CXA-F248 gate must be reachable through \
             the truthfully named {TICKET} merge — the delta must not be split \
             out or left behind elsewhere"
    );
}

// --- AC5 ---------------------------------------------------------------------

/// AC5: "Post-merge verification leaves main's local gate suite green (cargo
/// fmt --check, clippy --workspace --all-targets, cargo test --workspace,
/// hexagonal_gate) apart only from the documented pre-existing red list
/// (CXA-B037 engine-spawn timeouts), with any other failure triaged before the
/// merge is declared done."
///
/// The suite run itself is the implementer's verification act; this guard pins
/// the contract that makes "apart only from" auditable: the red list is
/// DOCUMENTED as exactly the CXA-B037 engine-spawn pair, the documented tests
/// still exist where the list says they do (a rotted list cannot scope real
/// reds), and the named hexagonal gate is part of the suite.
#[test]
fn ac5_post_merge_gates_stay_green_apart_only_from_the_documented_b037_red_list() {
    let doc = read(RED_LIST_DOC);
    assert!(
        doc.contains("CXA-B037") && doc.to_lowercase().contains("engine spawn"),
        "the pre-existing red list must be the documented CXA-B037 \
             engine-spawn timeouts — {RED_LIST_DOC}"
    );
    for (file, test_fn) in RED_LIST_TESTS {
        assert!(
            doc.contains(test_fn),
            "the documented red list must name {test_fn} — an unnamed red \
                 test cannot be the one failure the gate suite is allowed"
        );
        assert!(
            read(file).contains(&format!("fn {test_fn}")),
            "the documented red test {test_fn} must still be defined in {file} \
                 — a rotted red list silently widens what 'apart only from' covers"
        );
    }
    assert!(
        doc.contains("only these two infrastructure crate tests"),
        "the red list must stay scoped to exactly the CXA-B037 engine-spawn \
             pair — any other red failure is NEW and must be triaged before \
             the merge is declared done"
    );
    read("crates/app/tests/hexagonal_gate.rs");
}
