//! CXA-F306 — Lesson efficacy loop: track recurrence per failure-class lesson
//! and force repeaters into structural fixes. RED half of the TDD pair.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "When a post-mortem (rollback, deploy or test failure) is recorded and
//!    its summary matches an existing project or hub lesson above the
//!    similarity threshold, that lesson's recurrence count increments exactly
//!    once for the incident and the linked incident is visible on the Hub
//!    lessons page."
//! 2. "The Hub lessons page shows for every lesson when it was recorded, its
//!    recurrence count and most recent recurrence, and lists lessons with 2+
//!    recurrences in a 'repeating' section with a one-click action to file a
//!    prevention ticket."
//! 3. "A reviewer can dismiss a suggested incident-to-lesson match; dismissed
//!    matches never increment recurrence counts and stay dismissed."
//! 4. "A lesson evicted by the 30-entry hub cap that matches a later incident
//!    is re-surfaced with its prior recurrence history instead of being
//!    silently lost."
//! 5. "An incident with no plausible matching lesson creates no match, and a
//!    failure inside the matching step never delays or breaks the cycle."
//!
//! HOW THESE CRITERIA ARE ENCODED: pure fixtures over the real state/domain
//! types the codebase has today (`IncidentRecord` — the CXA-F012 post-mortem
//! record the matching keys on, `ProjectState.lessons` + `add_lesson`,
//! `prompts::record_hub_lesson` / `hub_lessons_block` over the in-memory
//! `WorkspaceFilesPort` double, `parsing::jaccard` + `title_tokens` — the
//! codebase's own similarity primitive, `RevertDecision::Dismissed` — the
//! shipped persisted-dismissal idiom) plus source-scan guards over the
//! surfaces that do not exist yet — the same no-harness discipline as
//! `approval_policy_f303_tdd.rs`, `live_repro_url_f246_tdd.rs` and
//! `evidence_repro_routes_f248_tdd.rs`: no fake HTTP server, no host harness,
//! no network port, no invented identifiers. A test that called a recurrence
//! type or a lesson matcher directly could not compile today (no such symbol
//! exists anywhere in the workspace — verified before writing this file:
//! `recurrence` matches only the unrelated `TicketRecurrence` metric), so the
//! red half pins the missing behaviour where it must live, and the green half
//! pins the executable semantics over the types that DO exist. Every failing
//! assertion below fails only because CXA-F306's behaviour is missing; if an
//! assertion's mechanism moves during implementation, move the guard with it
//! (the `preflight_f239_tdd.rs` convention).
//!
//! The data every fixture uses is real codebase data:
//!   * post-mortems are durable `IncidentRecord`s with a free-text `summary`
//!     (written by `post_mortem` in `use_cases/cycle/ops.rs`, one per
//!     rollback / rollback-skip — the "rollback, deploy or test failure"
//!     records AC1 names);
//!   * project lessons are `ProjectState.lessons` (deduped, capped at 12);
//!   * hub lessons are the `hub_lessons.md` bullet store (deduped, capped at
//!     30 — the "30-entry hub cap" AC4 names — read/written only through the
//!     `WorkspaceFilesPort`);
//!   * the similarity premise is `parsing::jaccard` over `title_tokens`, whose
//!     documented near-duplicate threshold in this codebase is 0.6
//!     (`duplicates_existing`). The AC says "above the similarity threshold"
//!     without a number; the green guard pins the 0.6 house convention as the
//!     premise the matcher must restate or deliberately change.
//!
//! NOTE FOR THE SA (flagged, not blocking): the hub store is a bare markdown
//! bullet list with no per-lesson metadata, and project lessons are bare
//! strings — so AC2's "when it was recorded / most recent recurrence" requires
//! BOTH stores to start recording per-lesson metadata. That is the feature
//! under ticket (no fixture fabrication is involved: the lesson TEXT and the
//! incident summaries exist today), but it means the store formats change.
//! There is no committed design document for CXA-F306 in `.coxagent/design/`
//! (F302–F305 and F307 are present, F306 is absent); the AC text above
//! governs, and the guards below accept the natural homes for the new state
//! rather than pinning one file.
//!
//! Red today, and why:
//!   * AC1 — no recurrence tracking exists for either store, and the
//!     post-mortem path (`post_mortem`) records a formulaic lesson
//!     unconditionally (`add_lesson`) without ever matching its summary
//!     against existing lessons.
//!   * AC2 — the only "Hub lessons page" is the wiki mirror (`hub-lessons`
//!     doc page, a plain bullet list re-rendered daily by the SM in
//!     `ceremonies.rs`); no surface anywhere renders recorded-at, recurrence
//!     counts, a most-recent recurrence, or a repeating section, and nothing
//!     files a prevention ticket from it.
//!   * AC3 — a suggested match does not exist, so neither does its dismissal;
//!     no match-state home mentions a persisted dismissal (the shipped
//!     `RevertDecision::Dismissed` idiom governs revert verdicts only).
//!   * AC4 — the 30-entry cap evicts by plain `drain` with nothing retained:
//!     an evicted lesson is silently lost (green guard pins the cap and the
//!     eviction; red guard pins that eviction must retain resurfacing
//!     history).
//!   * AC5 — no matching step exists to fail; the post-mortem path keeps its
//!     shipped best-effort discipline (returns `()`, swallows store errors),
//!     which the guard pins as binding while the matcher is added.
//!
//! AC → test map:
//! - AC1: [`ac1_project_lessons_track_recurrence`] (RED),
//!   [`ac1_hub_lessons_track_recurrence`] (RED),
//!   [`ac1_the_post_mortem_path_matches_its_summary_against_lessons`] (RED),
//!   [`ac1_recurrence_is_anchored_to_its_incident`] (RED), plus the green
//!   [`the_post_mortem_record_is_real_state_a_matcher_can_key_on`],
//!   [`project_lessons_are_deduped_and_capped_today`],
//!   [`jaccard_separates_a_recurring_summary_from_an_unrelated_one`]
//! - AC2: [`ac2_the_lessons_page_shows_when_each_lesson_was_recorded_and_its_recurrences`]
//!   (RED), [`ac2_repeating_lessons_get_a_section_with_a_prevention_ticket_action`]
//!   (RED)
//! - AC3: [`ac3_a_suggested_match_can_be_dismissed_and_stays_dismissed`]
//!   (RED), plus the green [`the_persisted_dismissal_idiom_exists_to_extend`]
//! - AC4: [`ac4_an_evicted_lesson_is_retained_for_resurfacing_with_its_history`]
//!   (RED), plus the green
//!   [`the_hub_store_caps_at_thirty_and_eviction_leaves_the_prompt_bounded`]
//! - AC5: [`ac5_the_matching_step_is_isolated_from_cycle_failures`] (RED),
//!   plus the green
//!   [`jaccard_separates_a_recurring_summary_from_an_unrelated_one`] (its
//!   unrelated-pair half is the no-match premise the matcher's own unit tests
//!   must reproduce)

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use coxagent_application::parsing::{jaccard, title_tokens};
use coxagent_application::ports::outbound::{FileMeta, WorkspaceFilesPort};
use coxagent_application::prompts;
use coxagent_application::state::{IncidentRecord, ProjectState, RevertDecision, MAX_INCIDENTS};

// --- accepted homes for the missing behaviour (the f246/f303 convention) -----

/// The project-lesson store: `ProjectState.lessons` lives in `state/mod.rs`;
/// a bounded-context split (`state/lessons.rs`) is the only plausible move.
const PROJECT_LESSON_HOMES: &[&str] = &[
    "crates/application/src/state/mod.rs",
    "crates/application/src/state/lessons.rs",
];

/// The hub-lesson store owner (`prompts.rs` holds `hub_lessons_path`,
/// `record_hub_lesson`, `hub_lessons_block` and the 30-entry cap) plus the
/// natural extraction homes for a lesson-matching/efficacy module.
const HUB_LESSON_HOMES: &[&str] = &[
    "crates/application/src/prompts.rs",
    "crates/application/src/hub_lessons.rs",
    "crates/application/src/use_cases/lesson_efficacy.rs",
    "crates/application/src/use_cases/lesson_match.rs",
];

/// The post-mortem path AC1 wires the matcher into (CXA-F012's
/// `post_mortem`, called for rollbacks and rollback-skips; deploy/test
/// failures reach it through the same funnel).
const OPS: &str = "crates/application/src/use_cases/cycle/ops.rs";

/// The wiki mirror that renders the only "Hub lessons page" today (the
/// `hub-lessons` doc page), plus the DOCS role's page guard beside it.
const CEREMONIES: &str = "crates/application/src/use_cases/cycle/ceremonies.rs";
const RUN_DOCS: &str = "crates/application/src/use_cases/run_docs.rs";
const INDEX_HTML: &str = "crates/presentation/src/web/index.html";

// --- repo-state scan helpers (the approval_policy_f303_tdd.rs pattern) -------

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

fn try_read(rel: &str) -> Option<String> {
    std::fs::read_to_string(repo_root().join(rel)).ok()
}

fn flat(src: &str) -> String {
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

fn low(src: &str) -> String {
    flat(src).to_ascii_lowercase()
}

fn any_of(hay: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| hay.contains(n))
}

/// Windows of `radius` chars around every occurrence of `needle` in
/// already-flattened source.
fn windows_around(hay_flat: &str, needle: &str, radius: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = hay_flat[from..].find(needle) {
        let at = from + at;
        let start = at.saturating_sub(radius);
        let end = (at + needle.len() + radius).min(hay_flat.len());
        out.push(hay_flat[start..end].to_owned());
        from = at + needle.len();
    }
    out
}

/// The source window of one top-level item: from its `header` to the next
/// item introduced by `terminator` (or end of file).
fn window_of<'a>(src: &'a str, header: &str, terminator: &str) -> &'a str {
    let Some(at) = src.find(header) else {
        return "";
    };
    let rest = &src[at..];
    let end = rest[header.len()..]
        .find(terminator)
        .map_or(rest.len(), |rel| header.len() + rel);
    &rest[..end]
}

/// Every presentation server source with its repo-relative path.
fn server_sources() -> Vec<(String, String)> {
    let dir = repo_root().join("crates/presentation/src/server");
    let mut out: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("list {}: {e}", dir.display()))
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if !p.extension().is_some_and(|ext| ext == "rs") {
                return None;
            }
            let name = p.file_name()?.to_str()?.to_owned();
            let rel = format!("crates/presentation/src/server/{name}");
            let src = std::fs::read_to_string(&p).ok()?;
            Some((rel, src))
        })
        .collect();
    out.sort();
    out
}

/// Every dashboard script (the vendored minified bundles excluded).
fn web_js_sources() -> Vec<(String, String)> {
    let dir = repo_root().join("crates/presentation/src/web/js");
    let mut out: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("list {}: {e}", dir.display()))
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if !p.extension().is_some_and(|ext| ext == "js") {
                return None;
            }
            let name = p.file_name()?.to_str()?.to_owned();
            if name.ends_with(".min.js") {
                return None;
            }
            let rel = format!("crates/presentation/src/web/js/{name}");
            let src = std::fs::read_to_string(&p).ok()?;
            Some((rel, src))
        })
        .collect();
    out.sort();
    out
}

/// Every surface that can render the Hub lessons page: the server (a
/// dedicated route or the docs payload), the dashboard scripts, the page
/// shell, and the wiki-mirror writers in the application layer.
fn page_sources() -> Vec<(String, String)> {
    let mut out = server_sources();
    out.extend(web_js_sources());
    for rel in [CEREMONIES, RUN_DOCS, INDEX_HTML] {
        if let Some(src) = try_read(rel) {
            out.push((rel.to_owned(), src));
        }
    }
    out
}

// --- fixtures over the real state/domain types -------------------------------

/// In-memory files port double (the brief_screening_f305_tdd.rs pattern): the
/// hub lesson store is read/written only through this port, so the store
/// fixtures below are pure.
struct MemFiles(Mutex<HashMap<PathBuf, String>>);

impl MemFiles {
    fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(HashMap::new())))
    }
    fn get(&self, path: &Path) -> String {
        self.0
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl WorkspaceFilesPort for MemFiles {
    async fn read(&self, path: &Path) -> Option<String> {
        self.0.lock().unwrap().get(path).cloned()
    }
    async fn read_bytes(&self, path: &Path) -> Option<Vec<u8>> {
        self.0
            .lock()
            .unwrap()
            .get(path)
            .map(String::as_bytes)
            .map(<[u8]>::to_vec)
    }
    async fn write(&self, p: &Path, c: &str) -> bool {
        self.0.lock().unwrap().insert(p.to_path_buf(), c.to_owned());
        true
    }
    async fn write_bytes(&self, _p: &Path, _b: &[u8]) -> bool {
        false
    }
    async fn delete(&self, p: &Path) -> bool {
        self.0.lock().unwrap().remove(p).is_some()
    }
    async fn list(&self, dir: &Path) -> Vec<FileMeta> {
        self.0
            .lock()
            .unwrap()
            .keys()
            .filter(|p| p.parent().is_some_and(|parent| parent == dir))
            .map(|p| FileMeta {
                path: p.clone(),
                modified_epoch: 0,
                size: 0,
            })
            .collect()
    }
    async fn stat(&self, path: &Path) -> Option<FileMeta> {
        self.0.lock().unwrap().contains_key(path).then(|| FileMeta {
            path: path.to_path_buf(),
            modified_epoch: 0,
            size: 0,
        })
    }
    async fn list_recursive(&self, dir: &Path) -> Vec<PathBuf> {
        self.0
            .lock()
            .unwrap()
            .keys()
            .filter(|p| p.starts_with(dir))
            .cloned()
            .collect()
    }
    async fn list_dirs(&self, _dir: &Path) -> Vec<PathBuf> {
        Vec::new()
    }
}

/// A real post-mortem summary (the free-text `summary` `post_mortem` stores
/// on every rollback / rollback-skip) and the kind of lesson a retro records
/// for it — paraphrases of each other, so the matcher premise is honest.
const LESSON_REPEATED: &str =
    "docker build fails when the base image tag moves — pin the base image version";
const SUMMARY_REPEATED: &str = "deploy failed: docker build fails when the base image tag moves";

/// An incident whose failure shares nothing with the lesson corpus — AC5's
/// no-plausible-match shape, built from the engine-failure vocabulary the
/// incident records actually carry.
const LESSON_UNRELATED: &str = "always pin the docker base image version";
const SUMMARY_UNRELATED: &str =
    "tests failed: oauth token expired for engine claude — re-authenticate the runner";

/// The CXA-F012 post-mortem record exactly as `post_mortem` writes it (one
/// per rollback / rollback-skip), carried in `ProjectState.incidents`.
fn incident_record(reason: &str, summary: &str) -> IncidentRecord {
    IncidentRecord {
        at: "2026-09-02T10:00:00Z".to_owned(),
        reason: reason.to_owned(),
        failed_sha: "abc1234".to_owned(),
        to_sha: "def5678".to_owned(),
        ok: true,
        summary: summary.to_owned(),
        root_cause_ticket: None,
        lesson: None,
    }
}

// --- green guards: fixture validity and binding contracts over today's types -

/// AC1's premise: the post-mortem record the matcher must consume is real,
/// durable state — a plain `IncidentRecord` literal (the exact shape
/// `post_mortem` pushes) round-trips through serde and lives in
/// `ProjectState.incidents` under the shipped `MAX_INCIDENTS` bound. THE
/// MATCHER'S CONTRACT, for the implementer's own unit tests to reproduce:
/// the incident identity (`at` + `failed_sha`) is what "exactly once for the
/// incident" keys on, and `summary` is the text matched against lessons.
#[test]
fn the_post_mortem_record_is_real_state_a_matcher_can_key_on() {
    let mut s = ProjectState::default();
    s.incidents
        .push(incident_record("tests failed", SUMMARY_REPEATED));
    s.incidents
        .push(incident_record("deploy failed", SUMMARY_UNRELATED));
    let overflow = s.incidents.len().saturating_sub(MAX_INCIDENTS);
    assert_eq!(overflow, 0, "two incidents sit far below the incident cap");

    let doc = serde_json::to_string(&s.incidents).expect("incidents serialize");
    let back: Vec<IncidentRecord> = serde_json::from_str(&doc).expect("incidents deserialize");
    assert_eq!(back, s.incidents, "the post-mortem record is durable state");
    assert_eq!(
        back[0].summary, SUMMARY_REPEATED,
        "the summary AC1 matches against is carried verbatim"
    );
    assert_eq!(
        back[0].reason, "tests failed",
        "the trigger reason is carried"
    );
}

/// AC1's premise on the project side: lessons are deduped, newest-last,
/// capped at 12 — so a recurrence increment must ADD counting to this store
/// without breaking the dedupe that makes one lesson one row.
#[test]
fn project_lessons_are_deduped_and_capped_today() {
    let mut s = ProjectState::default();
    s.add_lesson(LESSON_REPEATED);
    s.add_lesson(LESSON_REPEATED);
    assert_eq!(
        s.lessons.len(),
        1,
        "an exact-duplicate lesson is deduped, not stacked"
    );
    for i in 0..12 {
        s.add_lesson(&format!("distinct lesson {i}: pin the dependency"));
    }
    assert_eq!(s.lessons.len(), 12, "the project lesson store caps at 12");
    assert!(
        !s.lessons.contains(&LESSON_REPEATED.to_owned()),
        "the oldest lesson is evicted first — the same silent-loss shape AC4 \
         fixes on the hub store"
    );
}

/// AC1/AC5's similarity premise: the codebase's own similarity primitive
/// (`jaccard` over `title_tokens`, threshold 0.6 per `duplicates_existing`)
/// scores a post-mortem summary that repeats a lesson ABOVE the threshold and
/// an unrelated failure BELOW it. The matcher must restate exactly these two
/// verdicts — match above the threshold, no match below — over these same
/// primitives (or replace them deliberately, moving this guard with it).
#[test]
fn jaccard_separates_a_recurring_summary_from_an_unrelated_one() {
    let recurring = jaccard(
        &title_tokens(LESSON_REPEATED),
        &title_tokens(SUMMARY_REPEATED),
    );
    assert!(
        recurring >= 0.6,
        "a summary that repeats a lesson must score above the 0.6 \
         similarity threshold; got {recurring}"
    );
    let unrelated = jaccard(
        &title_tokens(LESSON_UNRELATED),
        &title_tokens(SUMMARY_UNRELATED),
    );
    assert!(
        unrelated < 0.6,
        "an incident with no plausible matching lesson must score below the \
         threshold — no match, no recurrence; got {unrelated}"
    );
}

/// AC4's premise, executable today: the hub store caps at 30 entries and
/// eviction drops the OLDEST lesson out of the prompt block — the exact
/// silent-loss shape AC4 exists to fix. This stays binding after CXA-F306:
/// the cap and the bounded prompt block are kept by the AC's own wording
/// ("evicted by the 30-entry hub cap"), and whatever retention/resurfacing
/// mechanism lands must not unbound either.
#[tokio::test]
async fn the_hub_store_caps_at_thirty_and_eviction_leaves_the_prompt_bounded() {
    let fs = MemFiles::new();
    let path = prompts::hub_lessons_path();
    for i in 0..31 {
        prompts::record_hub_lesson(
            Some(fs.as_ref()),
            &format!("lesson {i}: pin the dependency"),
        )
        .await;
    }
    let stored = fs.get(&path);
    let entries = stored.lines().filter(|l| l.starts_with("- ")).count();
    assert!(
        entries <= 30,
        "the 30-entry hub cap holds: {entries} entries in the store"
    );
    let block = prompts::hub_lessons_block(Some(fs.as_ref()), false).await;
    assert!(
        block.contains("lesson 30"),
        "the newest lesson is what the prompt block carries: {block}"
    );
    assert!(
        !block.contains("lesson 0:"),
        "the evicted oldest lesson is out of the prompt block today — the \
         silent loss AC4 must turn into a resurfacing path: {block}"
    );
}

/// AC3's premise: the codebase already owns a persisted human-dismissal idiom
/// — `RevertDecision::Dismissed`, the reviewer verdict that stops CXA-F047's
/// revert detector from re-flagging a decided commit. The incident-to-lesson
/// dismissal must be this same shape of thing: persisted, serde-stable, and
/// re-readable so a dismissed match STAYS dismissed.
#[test]
fn the_persisted_dismissal_idiom_exists_to_extend() {
    let doc = serde_json::to_string(&RevertDecision::Dismissed).expect("decision serializes");
    assert_eq!(
        doc, "\"dismissed\"",
        "the shipped dismissal verdict is a stable serde value"
    );
    let back: RevertDecision = serde_json::from_str(&doc).expect("decision deserializes");
    assert_eq!(back, RevertDecision::Dismissed, "and it round-trips");
}

// --- AC1 ---------------------------------------------------------------------

/// AC1: "its summary matches an existing project or hub lesson above the
/// similarity threshold, that lesson's recurrence count increments" — the
/// PROJECT lesson store must track per-lesson recurrence. RED: no recurrence
/// state exists anywhere in the state module (`recurrence` matches nothing in
/// `state/` today — the only workspace hit is the unrelated per-ticket
/// `TicketRecurrence` metric in `metrics_health.rs`). Accepted homes: the
/// `ProjectState` module where the lessons vector lives, or a
/// `state/lessons.rs` bounded-context split.
#[test]
fn ac1_project_lessons_track_recurrence() {
    let hit = PROJECT_LESSON_HOMES
        .iter()
        .find(|h| try_read(h).is_some_and(|src| low(&src).contains("recurrence")));
    assert!(
        hit.is_some(),
        "no project-lesson recurrence state exists in {PROJECT_LESSON_HOMES:?} — AC1's \
         recurrence count has nothing to increment for project lessons",
    );
}

/// AC1: "...an existing project or hub lesson..." — the HUB lesson store
/// (`hub_lessons.md`, owned by `prompts.rs`) must track recurrence too: the
/// efficacy loop covers the team's lessons wherever they live. RED: no
/// recurrence state exists in the hub-store owner. Accepted homes: the
/// `prompts.rs` store (its write/read paths own the format), a `hub_lessons.rs`
/// extraction, or the use-case module that performs the matching.
#[test]
fn ac1_hub_lessons_track_recurrence() {
    let hit = HUB_LESSON_HOMES
        .iter()
        .find(|h| try_read(h).is_some_and(|src| low(&src).contains("recurrence")));
    assert!(
        hit.is_some(),
        "no hub-lesson recurrence state exists in {HUB_LESSON_HOMES:?} — AC1's recurrence \
         count has nothing to increment for hub lessons",
    );
}

/// AC1: "When a post-mortem ... is recorded and its summary matches ..." —
/// the matching step must be wired INTO the post-mortem path, not bolted onto
/// a reader. RED: `post_mortem` records a formulaic lesson unconditionally
/// (`add_lesson`) and never consults any matcher — none of the matching
/// vocabulary appears anywhere in its body.
#[test]
fn ac1_the_post_mortem_path_matches_its_summary_against_lessons() {
    let ops = read(OPS);
    let window = window_of(
        &ops,
        "pub(super) async fn post_mortem",
        "\npub(super) async fn",
    );
    assert!(
        window.contains("pub(super) async fn post_mortem"),
        "post_mortem moved out of {OPS} — point this guard at the path that \
         records post-mortems"
    );
    assert!(
        any_of(
            &low(window),
            &[
                "recurrence",
                "similarity",
                "efficacy",
                "lesson_match",
                "match_lesson",
                "match_lessons",
                "match_summary",
                "match_incident",
            ]
        ),
        "the post-mortem path never matches its summary against existing \
         lessons — it appends a formulaic lesson unconditionally, so AC1's \
         recurrence loop never runs"
    );
}

/// AC1: "...increments exactly once for the incident and the linked incident
/// is visible on the Hub lessons page" — recurrence entries must be anchored
/// to their incident (identity to dedupe on, link to render). RED: there is
/// no recurrence state at all, so no recurrence declaration anywhere mentions
/// the incident it belongs to. Guard shape: within any recurrence-declaring
/// source, a `recurrence` occurrence must carry `incident` in its vicinity
/// (the linkage data, not prose distance-precision).
#[test]
fn ac1_recurrence_is_anchored_to_its_incident() {
    let homes = PROJECT_LESSON_HOMES
        .iter()
        .chain(HUB_LESSON_HOMES.iter())
        .filter_map(|h| try_read(h).map(|src| (*h, low(&src))))
        .collect::<Vec<_>>();
    let anchored = homes.iter().any(|(_, src)| {
        windows_around(src, "recurrence", 240)
            .iter()
            .any(|w| w.contains("incident"))
    });
    assert!(
        anchored,
        "no recurrence declaration is anchored to its incident — without the \
         incident linkage the count cannot be once-per-incident and the link \
         cannot render on the Hub lessons page"
    );
}

// --- AC2 ---------------------------------------------------------------------

/// AC2: "The Hub lessons page shows for every lesson when it was recorded,
/// its recurrence count and most recent recurrence" — SOME surface must
/// render all three metadata points. RED: the only lessons page today is the
/// wiki mirror (a plain bullet list), and no server route, dashboard script
/// or mirror writer emits any of this metadata (`recurrence`,
/// `recorded_at`, a most-recent-recurrence field all match nothing on any
/// page surface).
#[test]
fn ac2_the_lessons_page_shows_when_each_lesson_was_recorded_and_its_recurrences() {
    let pages = page_sources();
    let recurrence = pages.iter().any(|(_, src)| low(src).contains("recurrence"));
    let recorded_at = pages
        .iter()
        .any(|(_, src)| any_of(&low(src), &["recorded_at", "first_recorded"]));
    let most_recent = pages.iter().any(|(_, src)| {
        any_of(
            &low(src),
            &[
                "last_recurrence",
                "latest_recurrence",
                "most_recent_recurrence",
            ],
        )
    });
    assert!(
        recurrence && recorded_at && most_recent,
        "no page surface renders the lessons metadata AC2 requires \
         (recurrence={recurrence}, recorded-at={recorded_at}, \
         most-recent-recurrence={most_recent}) — the Hub lessons page is a \
         plain bullet list today"
    );
}

/// AC2: "...lists lessons with 2+ recurrences in a 'repeating' section with a
/// one-click action to file a prevention ticket" — the repeating section and
/// its prevention-ticket action must exist on a page surface. RED: no
/// dashboard script, server route or mirror writer has a repeating section,
/// and nothing on any page surface files a prevention ticket ("prevention"
/// appears only in an unrelated XSS comment in `security.rs`).
#[test]
fn ac2_repeating_lessons_get_a_section_with_a_prevention_ticket_action() {
    let pages = page_sources();
    let repeating_with_action = pages.iter().any(|(name, src)| {
        let l = low(src);
        l.contains("repeating")
            && (l.contains("prevention") || l.contains("prevention_ticket"))
            && !name.ends_with("core.js")
    });
    assert!(
        repeating_with_action,
        "no page surface carries a 'repeating' section whose action files a \
         prevention ticket — 2+ recurrences have nowhere to be escalated from"
    );
}

// --- AC3 ---------------------------------------------------------------------

/// AC3: "A reviewer can dismiss a suggested incident-to-lesson match;
/// dismissed matches never increment recurrence counts and stay dismissed" —
/// two halves, both missing. RED (state half): no match state exists to hang
/// a dismissal on, so no dismissal lives near the (absent) recurrence state.
/// RED (surface half): no server surface connects a dismiss action to the
/// lessons/match domain (the shipped dismiss actions in `inbox.rs` govern
/// holds and revert verdicts, and no server file mentions lessons at all).
/// The "never increment / stays dismissed" semantics are the contract the
/// implementer's unit tests must pin over the persisted record this guard
/// forces into existence — the `RevertDecision` precedent (green guard
/// above) is the shape to follow.
#[test]
fn ac3_a_suggested_match_can_be_dismissed_and_stays_dismissed() {
    let state_homes = PROJECT_LESSON_HOMES
        .iter()
        .chain(HUB_LESSON_HOMES.iter())
        .filter_map(|h| try_read(h).map(|src| (*h, low(&src))))
        .collect::<Vec<_>>();
    let persisted_dismissal = state_homes.iter().any(|(name, src)| {
        windows_around(src, "recurrence", 240)
            .iter()
            .any(|w| w.contains("dismiss"))
            || low(name).contains("lesson") && src.contains("dismissed_match")
    });
    assert!(
        persisted_dismissal,
        "no persisted dismissal exists for incident-to-lesson matches — a \
         dismissed match cannot stay dismissed across restarts, and nothing \
         stops it from incrementing recurrence later"
    );

    let surface = server_sources().iter().any(|(_, src)| {
        let l = low(src);
        l.contains("dismiss") && (l.contains("lesson") || l.contains("recurrence"))
    });
    assert!(
        surface,
        "no server surface exposes the reviewer's dismiss action for a \
         suggested match — the dismissal exists in state only"
    );
}

// --- AC4 ---------------------------------------------------------------------

/// AC4: "A lesson evicted by the 30-entry hub cap that matches a later
/// incident is re-surfaced with its prior recurrence history instead of being
/// silently lost" — the eviction path must retain what it drops. RED: the
/// cap evicts by a plain `drain` inside `record_hub_lesson` with nothing
/// recorded about the evicted entries (no eviction/resurfacing vocabulary in
/// the store's write path, and no recurrence history anywhere to re-surface).
/// The cap test above proves the eviction itself is real; this guard proves
/// the loss is not.
#[test]
fn ac4_an_evicted_lesson_is_retained_for_resurfacing_with_its_history() {
    let prompts_src =
        try_read("crates/application/src/prompts.rs").expect("the hub-store owner exists");
    let record_window = window_of(
        &prompts_src,
        "pub async fn record_hub_lesson",
        "\npub async fn",
    );
    let write_path_retains = any_of(
        &low(record_window),
        &[
            "evict",
            "eviction",
            "resurface",
            "re_surface",
            "resurfacing",
        ],
    );
    let homes_with_history = HUB_LESSON_HOMES
        .iter()
        .chain(PROJECT_LESSON_HOMES.iter())
        .filter_map(|h| try_read(h).map(|src| low(&src)))
        .any(|src| {
            windows_around(&src, "recurrence", 300).iter().any(|w| {
                any_of(
                    w,
                    &[
                        "evict",
                        "eviction",
                        "resurface",
                        "re_surface",
                        "resurfacing",
                        "prior recurrence",
                    ],
                )
            })
        });
    assert!(
        write_path_retains || homes_with_history,
        "the hub-store eviction drops lessons on the floor (a bare `drain` \
         past the 30-entry cap) — an evicted lesson that matches a later \
         incident is silently lost with no prior recurrence history to \
         re-surface"
    );
}

// --- AC5 ---------------------------------------------------------------------

/// AC5: "...a failure inside the matching step never delays or breaks the
/// cycle" — the post-mortem path's shipped best-effort discipline is the
/// contract: it returns `()` (a matching failure cannot become a caller-visible
/// error the cycle must handle) and its store writes are swallowed. RED half:
/// the matching step does not exist yet, so the path contains none of its
/// vocabulary. GREEN half (binding): the discipline must survive the matcher's
/// arrival — `post_mortem` must NOT grow a `Result` return, or a matching-step
/// failure becomes a cycle-delaying error path.
#[test]
fn ac5_the_matching_step_is_isolated_from_cycle_failures() {
    let ops = read(OPS);
    let window = window_of(
        &ops,
        "pub(super) async fn post_mortem",
        "\npub(super) async fn",
    );
    assert!(
        any_of(
            &low(window),
            &[
                "recurrence",
                "similarity",
                "efficacy",
                "lesson_match",
                "match_lesson",
                "match_lessons",
                "match_summary",
                "match_incident",
            ]
        ),
        "no matching step runs inside the post-mortem path — AC5's isolation \
         has nothing to isolate yet"
    );
    assert!(
        !window.contains("-> Result"),
        "post_mortem must stay best-effort (returns ()); a Result return \
         would let a failure inside the matching step delay or break the \
         cycle, violating AC5"
    );
}
