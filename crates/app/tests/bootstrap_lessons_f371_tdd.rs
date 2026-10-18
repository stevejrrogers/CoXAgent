//! CXA-F371 — Bootstrap lessons shipped with the binary: a fresh install
//! starts with the accumulated lesson base. RED half of the TDD pair.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "A fresh project's first boot seeds the shipped lesson set into
//!    state.lessons exactly once; a second boot seeds nothing (idempotence
//!    test)"
//! 2. "Runtime-learned and operator-edited lessons are never overwritten by
//!    seeding; upgrading to a binary with new shipped lessons adds only the
//!    new ids"
//! 3. "Shipped lessons are marked with a source tag visible in the lessons
//!    UI, distinguishable from locally learned ones"
//! 4. "The shipped set includes the depth bar and the engine-vs-task failure
//!    rule verbatim enough that role prompts composed on a fresh install
//!    carry them"
//! 5. "Unit tests cover seed-on-empty, seed-idempotence and merge-with-existing"
//!
//! HOW THESE CRITERIA ARE ENCODED: pure fixtures over the real state/domain
//! types the codebase has today (`ProjectState.lessons` — the prompt-facing
//! store AC1 names, `state::LessonRecord` — the efficacy ledger the lessons
//! UI reads, `prompts::team_memory_block` — the role-prompt composition path
//! (`run_sa.rs:178`, `run_pd.rs:159`, `run_dev/briefing.rs:255`), and the
//! two canonical rule texts the codebase already owns) plus source-scan
//! guards over the surfaces that do not exist yet — the same no-harness
//! discipline as `lesson_efficacy_f306_tdd.rs`: no fake HTTP server, no host
//! harness, no network port, no invented identifiers. A test that called a
//! seed function directly could not compile today (no lesson-seeding symbol
//! exists anywhere in the workspace — verified before writing this file:
//! `bootstrap_lesson|shipped_lesson|seed_lesson` match nothing in `crates/`),
//! so the red half pins the missing behaviour where it must live, and the
//! green half pins the executable semantics over the types that DO exist.
//! Every failing assertion below fails only because CXA-F371's behaviour is
//! missing.
//!
//! The real codebase data every fixture uses:
//!   * the fresh-install shape is `ProjectState::default()` — `lessons` and
//!     `lesson_records` both empty (the dogfood `state/state.json` today has
//!     `"lessons": []` — that IS a fresh project);
//!   * the depth bar exists VERBATIM in `prompts::ENGINEERING_STANDARDS`
//!     ("DEPTH BAR — no shallow features, however small (operator mandate)"
//!     … "Two or more missing = not done");
//!   * the engine-vs-task failure rule exists in code as
//!     `faults::is_infra_fault` + its doc ("Revoked auth, quota walls, rate
//!     limits and network outages are not any ticket's fault") — the single
//!     source of truth the DEV failure counter and the circuit breaker share;
//!   * role prompts carry `state.lessons` verbatim via
//!     `team_memory_block` under `- [lesson]` bullets — so seeding
//!     `state.lessons` on a fresh install is sufficient for the prompts to
//!     carry the rules, with zero new plumbing.
//!
//! THE BOOT PATH: a project boots in `crates/app/src/builders.rs`
//! (`build_project` — `make_store` then `store.load()`); projects are created
//! in `crates/app/src/onboard.rs` (`greenfield`/`brownfield`, which already
//! seeds smart tickets — `seed_smart_tickets` — the house precedent for
//! first-boot seeding). Neither mentions lessons today.
//!
//! NOTES FOR THE SA (flagged, not blocking — the F306 convention):
//! * AC2 says "new ids" and AC3 says "source tag", but `LessonRecord`
//!   (`crates/application/src/state/lessons.rs`) has NEITHER an id NOR a
//!   source field — lessons are deduped by bare text, and the prompt-facing
//!   `state.lessons` is `Vec<String>`, which cannot carry a tag at all. The
//!   tag therefore necessarily rides `LessonRecord` + the efficacy read model
//!   (`lesson_efficacy::LessonEfficacyRow`) + the lessons UI
//!   (`web/js/lessons.js` renders rows from that read model), and "ids" must
//!   either become real fields or rest on the verbatim-text identity. The
//!   guards below accept any home that satisfies the semantics; no fixture
//!   fabrication is involved (the lesson TEXTS and the rule sources exist
//!   today).
//! * AC4's "engine-vs-task failure rule" has no lesson-form text anywhere —
//!   only the `faults.rs` predicate and doc. The shipped-set module must
//!   declare it; the guard pins the content by the canonical `faults.rs`
//!   vocabulary, not by a fabricated sentence.
//! * `prompts.rs` already contains the depth bar (the engineering-standards
//!   prompt block) — it is deliberately NOT an accepted home for the shipped
//!   SET, or the AC4 guard would pass today without a set existing.
//!
//! Red today, and why:
//!   * AC1 — nothing seeds lessons anywhere: not `ProjectState::default()`,
//!     not `build_project`, not `onboard`; no shipped-set declaration exists.
//!   * AC2 — no merge/seeding semantics exist to protect or add anything.
//!   * AC3 — no source tag exists on `LessonRecord`, on the efficacy row, or
//!     in any lessons-UI surface (`source` matches nothing in all four).
//!   * AC4 — no shipped-set module exists to carry either rule.
//!   * AC5 — no seed function exists, so no unit tests cover its cases.
//!
//! AC → test map:
//! - AC1: [`ac1_a_fresh_projects_first_boot_seeds_the_shipped_lesson_set_into_state_lessons`]
//!   (RED), [`ac1_a_second_boot_seeds_nothing`] (RED), plus the green
//!   [`a_fresh_project_state_starts_with_no_lessons`]
//! - AC2: [`ac2_seeding_never_overwrites_runtime_learned_or_operator_edited_lessons`]
//!   (RED), [`ac2_an_upgraded_binary_adds_only_the_new_shipped_lessons`] (RED)
//! - AC3: [`ac3_shipped_lessons_carry_a_source_tag_the_lessons_ui_distinguishes_from_locally_learned`]
//!   (RED), plus the green
//!   [`the_lesson_record_is_the_durable_ledger_the_ui_rows_read`]
//! - AC4: [`ac4_the_shipped_set_carries_the_depth_bar_and_the_engine_vs_task_failure_rule`]
//!   (RED), plus the green [`the_depth_bar_exists_verbatim_in_the_house_standards_to_ship`],
//!   [`the_engine_vs_task_failure_rule_exists_in_code_to_ship`],
//!   [`lessons_in_state_reach_composed_role_prompts_verbatim`]
//! - AC5: [`ac5_unit_tests_cover_seed_on_empty_seed_idempotence_and_merge_with_existing`]
//!   (RED)

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use coxagent_application::faults::is_infra_fault;
use coxagent_application::lesson_efficacy::lesson_efficacy;
use coxagent_application::prompts;
use coxagent_application::state::{LessonRecord, ProjectState};

// --- accepted homes for the missing behaviour (the f306/f246 convention) -----

/// Where the shipped lesson SET can be declared: a dedicated
/// `bootstrap_lessons.rs` module (named for what it IS), or the lessons
/// bounded context it belongs to (`state/lessons.rs`, `state/mod.rs`).
/// `prompts.rs` is excluded on purpose — it already carries the depth bar in
/// the engineering-standards prompt block, so counting it would let the AC4
/// guard pass with no shipped set in existence.
const SHIPPED_SET_HOMES: &[&str] = &[
    "crates/application/src/bootstrap_lessons.rs",
    "crates/application/src/state/lessons.rs",
    "crates/application/src/state/mod.rs",
];

/// Where the boot wiring can call the seeding: the project boot
/// (`build_project` — store, then load), the project creation flow
/// (`greenfield`/`brownfield`), or the hub wiring around them.
const BOOT_HOMES: &[&str] = &[
    "crates/app/src/builders.rs",
    "crates/app/src/onboard.rs",
    "crates/app/src/lib.rs",
];

/// Where the source tag must become visible: the ledger record, the efficacy
/// read model the lessons UI consumes, the UI script itself, and the server
/// module owning the lessons routes.
const SOURCE_TAG_HOMES: &[&str] = &[
    "crates/application/src/state/lessons.rs",
    "crates/application/src/lesson_efficacy.rs",
    "crates/presentation/src/web/js/lessons.js",
    "crates/presentation/src/server/lessons.rs",
];

// --- repo-state scan helpers (the lesson_efficacy_f306_tdd.rs pattern) -------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn try_read(rel: &str) -> Option<String> {
    std::fs::read_to_string(repo_root().join(rel)).ok()
}

/// Whitespace-flattened lowercase source, so multi-word needles are matched
/// without line-wrap sensitivity (`"depth bar"` → `"depthbar"`).
fn low(src: &str) -> String {
    src.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase()
}

fn any_of(hay: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| hay.contains(n))
}

/// Nearest char boundary at or below `i` (windows must not slice through a
/// multi-byte char — the house-rule texts carry em-dashes).
fn floor_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Nearest char boundary at or above `i`.
fn ceil_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Windows of `radius` chars around every occurrence of `needle` in
/// already-flattened source.
fn windows_around(hay_flat: &str, needle: &str, radius: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = hay_flat[from..].find(needle) {
        let at = from + at;
        let start = floor_boundary(hay_flat, at.saturating_sub(radius));
        let end = ceil_boundary(hay_flat, (at + needle.len() + radius).min(hay_flat.len()));
        out.push(hay_flat[start..end].to_owned());
        from = at + needle.len();
    }
    out
}

/// Test-function identifiers declared in a source file: only fns carrying a
/// `#[test]` / `#[tokio::test]` attribute, so the AC5 guard counts the seed
/// module's OWN unit tests, not production fns that happen to say "seed".
fn test_fn_names(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut last_attr = false;
    for line in src.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("#[test]") || trimmed.starts_with("#[tokio::test]") {
            last_attr = true;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("fn ") {
            if last_attr {
                let name = rest
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .next()
                    .unwrap_or("")
                    .to_owned();
                if !name.is_empty() {
                    out.push(name);
                }
            }
        }
        if !trimmed.is_empty() {
            last_attr = false;
        }
    }
    out
}

// --- green guards: fixture validity and binding contracts over today's types -

/// AC1's premise: the fresh-install shape is a `ProjectState` with NO lessons
/// and NO records — exactly what a brand-new project's first boot loads (the
/// dogfood state today is `"lessons": []`). Whatever seeds must land in THIS
/// store, idempotent over THIS starting point.
#[test]
fn a_fresh_project_state_starts_with_no_lessons() {
    let s = ProjectState::default();
    assert!(
        s.lessons.is_empty(),
        "a fresh project has no prompt-facing lessons"
    );
    assert!(
        s.lesson_records.is_empty(),
        "a fresh project has no efficacy ledger records"
    );
    // And the store is the durable state the boot path persists (serde field,
    // not derived): a state with lessons round-trips unchanged.
    let mut s = ProjectState::default();
    s.record_lesson("pin the base image version");
    let doc = serde_json::to_string(&s).expect("state serializes");
    let back: ProjectState = serde_json::from_str(&doc).expect("state deserializes");
    assert_eq!(
        back.lessons,
        vec!["pin the base image version".to_owned()],
        "state.lessons is durable state a seed can merge into"
    );
}

/// AC4's premise, first half: the depth bar exists VERBATIM in the codebase's
/// engineering standards — the text there is to ship, not a fabrication.
#[test]
fn the_depth_bar_exists_verbatim_in_the_house_standards_to_ship() {
    let standards = prompts::ENGINEERING_STANDARDS;
    assert!(
        standards.contains("DEPTH BAR — no shallow features, however small (operator mandate)"),
        "the depth bar's opening is verbatim in ENGINEERING_STANDARDS"
    );
    assert!(
        standards.contains("Two or more missing = not done."),
        "the depth bar's closing checklist is verbatim too"
    );
}

/// AC4's premise, second half: the engine-vs-task failure rule exists in code
/// — `faults::is_infra_fault`, the single source of truth both the DEV failure
/// counter and the runner's circuit breaker consult. A shipped lesson carrying
/// the rule must agree with THIS predicate's verdicts (the canonical anchors
/// the AC4 guard's vocabulary comes from).
#[test]
fn the_engine_vs_task_failure_rule_exists_in_code_to_ship() {
    for why in [
        "API Error: 401 OAuth access token has been revoked.",
        "quota exceeded, retry later",
        "error sending request: connection refused (os error 61)",
    ] {
        assert!(
            is_infra_fault(why),
            "{why:?} is an engine/infra fault, not the ticket's failure"
        );
    }
    for why in [
        "error[E0308]: mismatched types",
        "test result: FAILED. 3 passed; 1 failed",
    ] {
        assert!(
            !is_infra_fault(why),
            "{why:?} is a genuine task failure — never excuse it as infra"
        );
    }
}

/// AC4's transport, executable today: lessons present in `ProjectState.lessons`
/// reach every composed role prompt VERBATIM via `team_memory_block`
/// (`run_sa.rs:178`, `run_pd.rs:159`, `run_dev/briefing.rs:255` …). The first
/// lesson here is the depth-bar bullet SLICED from `ENGINEERING_STANDARDS`
/// itself (no retyped copy); the second restates the faults.rs rule to prove
/// an arbitrary rule text rides the same way. Seeding `state.lessons` on a
/// fresh install is therefore SUFFICIENT for the AC4 "role prompts carry
/// them" half — the missing half is only the shipped set itself (red guard
/// below).
#[test]
fn lessons_in_state_reach_composed_role_prompts_verbatim() {
    let standards = prompts::ENGINEERING_STANDARDS;
    let start = standards
        .find("- DEPTH BAR")
        .expect("the depth bar bullet is in ENGINEERING_STANDARDS");
    let end = standards[start..]
        .find("\n- An unclear ticket")
        .expect("the depth bar bullet ends at the next house rule");
    let depth_bar = standards[start..start + end].trim().to_owned();
    assert!(depth_bar.contains("Two or more missing = not done."));

    let engine_vs_task =
        "an engine/infra fault (401, quota wall, network drop) is not the ticket's \
         fault — never charge it to the ticket"
            .to_owned();
    let block = prompts::team_memory_block(&[], &[depth_bar.clone(), engine_vs_task]);
    assert!(block.contains("- [lesson] "), "lessons render as lessons");
    assert!(
        block.contains(&depth_bar),
        "the depth bar reaches the composed prompt VERBATIM: {block}"
    );
    assert!(
        block.contains("not the ticket's fault"),
        "the engine-vs-task rule reaches the composed prompt: {block}"
    );
}

/// AC3's premise: `LessonRecord` is the durable per-lesson ledger, and the
/// efficacy read model maps it 1:1 into the rows the lessons UI renders
/// (`web/js/lessons.js` reads `lesson_efficacy.lessons`). A source tag has a
/// real home on BOTH ends — it must ride the record and the row; the red
/// guard below forces it into existence.
#[test]
fn the_lesson_record_is_the_durable_ledger_the_ui_rows_read() {
    let record = LessonRecord {
        text: "pin the base image version".to_owned(),
        at: "2026-09-01T10:00:00Z".to_owned(),
        ..LessonRecord::default()
    };
    let doc = serde_json::to_string(&record).expect("record serializes");
    let back: LessonRecord = serde_json::from_str(&doc).expect("record deserializes");
    assert_eq!(back, record, "the ledger record is durable state");

    let mut s = ProjectState::default();
    s.lesson_records.push(record);
    let eff = lesson_efficacy(&s);
    assert_eq!(eff.lessons.len(), 1, "every record becomes a UI row");
    assert_eq!(eff.lessons[0].text, "pin the base image version");
    assert_eq!(eff.lessons[0].recorded_at, "2026-09-01T10:00:00Z");
}

// --- AC1 ---------------------------------------------------------------------

/// AC1: "A fresh project's first boot seeds the shipped lesson set into
/// state.lessons exactly once". Two halves, both missing:
/// (a) no shipped lesson set is DECLARED anywhere in the application layer;
/// (b) no boot path (project boot `build_project`, project creation
/// `greenfield`/`brownfield`) wires any lesson seeding. Accepted homes for
/// the set: a `bootstrap_lessons.rs` module or the `state/lessons.rs`
/// bounded context; for the wiring: the boot homes. RED: `lesson` and
/// `seed|bootstrap|shipped` never co-occur in any boot home, and no home
/// declares a shipped/bootstrap lesson set.
#[test]
fn ac1_a_fresh_projects_first_boot_seeds_the_shipped_lesson_set_into_state_lessons() {
    let seed_words = ["seed", "bootstrap", "shipped"];
    let wired = BOOT_HOMES.iter().any(|h| {
        try_read(h).is_some_and(|src| {
            let flat = low(&src);
            windows_around(&flat, "lesson", 240)
                .iter()
                .any(|w| any_of(w, &seed_words))
        })
    });
    assert!(
        wired,
        "no boot path wires lesson seeding ({BOOT_HOMES:?}) — a fresh install \
         boots with an empty lesson base and stays empty"
    );

    let declared = SHIPPED_SET_HOMES.iter().any(|h| {
        try_read(h).is_some_and(|src| {
            let flat = low(&src);
            any_of(
                &flat,
                &[
                    "shippedlessons",
                    "bootstraplessons",
                    "shippedlesson",
                    "bootstraplesson",
                    "lessonbase",
                    "shipped_lessons",
                    "bootstrap_lessons",
                ],
            )
        })
    });
    assert!(
        declared,
        "no shipped lesson set is declared in {SHIPPED_SET_HOMES:?} — there is \
         no accumulated lesson base to seed from"
    );
}

/// AC1's idempotence half: "a second boot seeds nothing". The seed's
/// once-only contract must be pinned where the seed lives — as a documented
/// contract or (better) its own unit test (AC5 pins that test separately).
/// The anchor is seed/bootstrap vocabulary — NOT bare "lesson": the state
/// files are lesson-dense and mention "idempotent" in unrelated contexts (a
/// sprint-queue doc: "idempotent in truth … a shipped ticket is shipped
/// once"), which must not read as the contract. RED: `seed`/`bootstrap`
/// appear NOWHERE in the accepted homes today, so no once-only contract for
/// lesson seeding exists.
#[test]
fn ac1_a_second_boot_seeds_nothing() {
    let once_words = [
        "idempotent",
        "exactlyonce",
        "exactly once",
        "alreadyseeded",
        "already seeded",
        "secondboot",
        "second boot",
        "onceonly",
        "no-op",
    ];
    let pinned = SHIPPED_SET_HOMES.iter().any(|h| {
        try_read(h).is_some_and(|src| {
            let flat = low(&src);
            ["seed", "bootstrap"].iter().any(|anchor| {
                windows_around(&flat, anchor, 400)
                    .iter()
                    .any(|w| any_of(w, &once_words))
            })
        })
    });
    assert!(
        pinned,
        "no idempotence contract for lesson seeding exists — a second boot \
         could double-seed the shipped set with no guard anywhere"
    );
}

// --- AC2 ---------------------------------------------------------------------

/// AC2, first half: "Runtime-learned and operator-edited lessons are never
/// overwritten by seeding". BOTH lesson kinds are real state today: a
/// runtime-learned lesson lands via the shipped `record_lesson` entry point,
/// and an operator-edited lesson is arbitrary text in the persisted
/// `state.lessons` (the operator edits `state.json` directly — there is no
/// lessons-edit endpoint to route through). The green half below PROVES both
/// kinds exist in one state; the red half requires the seed home to declare
/// the additive-only merge contract. RED: no merge vocabulary exists in any
/// accepted home.
#[test]
fn ac2_seeding_never_overwrites_runtime_learned_or_operator_edited_lessons() {
    let mut s = ProjectState::default();
    assert!(
        s.record_lesson("runtime-learned: rebase before pushing a conflict resolution"),
        "the runtime entry point records real lessons"
    );
    s.lessons
        .push("operator-edited: keep the retro working agreement verbatim".to_owned());
    let before = s.lessons.clone();
    assert_eq!(before.len(), 2, "both lesson kinds sit in the store");

    let merge_words = [
        "neveroverwrit",
        "never overwrit",
        "preserve",
        "additive",
        "onlythenew",
        "only the new",
        "merge",
        "existinglessons",
        "existing lessons",
        "keepsexisting",
        "neverremove",
    ];
    // Anchored on seed/bootstrap vocabulary (absent today — see AC1's guard):
    // bare "lesson" windows would false-positive on unrelated prose in the
    // state files ("the additive-field convention", merge-history docs).
    let declared = SHIPPED_SET_HOMES.iter().any(|h| {
        try_read(h).is_some_and(|src| {
            let flat = low(&src);
            ["seed", "bootstrap"].iter().any(|anchor| {
                windows_around(&flat, anchor, 400)
                    .iter()
                    .any(|w| any_of(w, &merge_words))
            })
        })
    });
    assert!(
        declared,
        "no additive-only merge contract exists for lesson seeding — a seed \
         pass could overwrite or evict runtime-learned and operator-edited \
         lessons (the 12-entry prompt cap makes a careless re-seed doubly \
         destructive: it evicts from the END)"
    );
}

/// AC2, second half: "upgrading to a binary with new shipped lessons adds
/// only the new ids". The upgrade path is the same seed run on a state that
/// ALREADY carries the older shipped set — the contract is
/// existing-stay-untouched + only-the-new-added. RED: no shipped set, no
/// upgrade/only-the-new semantics anywhere in the accepted homes.
///
/// NOTE FOR THE SA: "ids" — `LessonRecord` has no id field today; lessons are
/// keyed by verbatim text (see the file header). The guard accepts the merge
/// SEMANTICS in either model (id-keyed or text-keyed); it does not fabricate
/// an id field.
#[test]
fn ac2_an_upgraded_binary_adds_only_the_new_shipped_lessons() {
    let upgrade_words = [
        "upgrade",
        "newshipped",
        "new shipped",
        "onlythenew",
        "only the new",
        "addsonly",
        "newids",
        "new ids",
    ];
    let declared = SHIPPED_SET_HOMES.iter().any(|h| {
        try_read(h).is_some_and(|src| {
            let flat = low(&src);
            ["seed", "bootstrap"].iter().any(|anchor| {
                windows_around(&flat, anchor, 400)
                    .iter()
                    .any(|w| any_of(w, &upgrade_words))
            })
        })
    });
    assert!(
        declared,
        "no upgrade semantics exist for the shipped lesson set — a newer \
         binary cannot add only its new lessons today because nothing ships \
         or merges lessons at all"
    );
}

// --- AC3 ---------------------------------------------------------------------

/// AC3: "Shipped lessons are marked with a source tag visible in the lessons
/// UI, distinguishable from locally learned ones". The tag must ride the
/// record → read model → UI chain (`SOURCE_TAG_HOMES`: `LessonRecord`, the
/// `LessonEfficacyRow` the page consumes, `lessons.js` which renders the
/// rows). RED: the words `source`, `shipped` and `bootstrap` match NOTHING in
/// any of the four homes today — verified before writing this guard — so a
/// shipped lesson is indistinguishable from a locally learned one everywhere.
/// (The loose `origin` was dropped deliberately: "hub-origin" appears in
/// `state/lessons.rs` prose about recurrence provenance and would read as a
/// tag that does not exist.)
#[test]
fn ac3_shipped_lessons_carry_a_source_tag_the_lessons_ui_distinguishes_from_locally_learned() {
    let tag_words = ["source", "shipped", "bootstrap"];
    let marked = SOURCE_TAG_HOMES.iter().any(|h| {
        try_read(h).is_some_and(|src| {
            let flat = low(&src);
            windows_around(&flat, "lesson", 300)
                .iter()
                .any(|w| any_of(w, &tag_words))
        })
    });
    assert!(
        marked,
        "no source tag exists anywhere on the lesson → read model → UI chain \
         ({SOURCE_TAG_HOMES:?}) — shipped and locally learned lessons render \
         identically, so nobody can tell a shipped rule from a project's own \
         scar tissue"
    );
}

// --- AC4 ---------------------------------------------------------------------

/// AC4: "The shipped set includes the depth bar and the engine-vs-task
/// failure rule verbatim enough that role prompts composed on a fresh install
/// carry them". The transport is proven green above (`team_memory_block`
/// carries `state.lessons` verbatim; the texts exist in
/// `ENGINEERING_STANDARDS` and `faults.rs`). What is missing is the SET
/// itself: no accepted home declares a shipped lesson set carrying BOTH
/// rules. RED: no `SHIPPED_SET_HOMES` file contains the depth bar TOGETHER
/// with the engine-vs-task rule vocabulary (the canonical anchors are the
/// `faults.rs` phrasings; `prompts.rs` is deliberately not a home — see the
/// file header).
#[test]
fn ac4_the_shipped_set_carries_the_depth_bar_and_the_engine_vs_task_failure_rule() {
    let depth_bar = ["depthbar", "noshallowfeatures"];
    let engine_vs_task = [
        "is_infra_fault",
        "notanyticketsfault",
        "notaticketsfault",
        "nottheticketsfault",
        "infrastructurefault",
        "taskfailure",
        "enginevstask",
        "enginevstaskfailurerule",
    ];
    let carrying = SHIPPED_SET_HOMES
        .iter()
        .filter_map(|h| try_read(h).map(|src| low(&src)))
        .any(|flat| any_of(&flat, &depth_bar) && any_of(&flat, &engine_vs_task));
    assert!(
        carrying,
        "no shipped lesson set carries BOTH the depth bar and the \
         engine-vs-task failure rule ({SHIPPED_SET_HOMES:?}) — a fresh \
         install composes role prompts from an empty lesson base, so every \
         agent re-derives (or re-violates) the two most expensive lessons"
    );
}

// --- AC5 ---------------------------------------------------------------------

/// AC5: "Unit tests cover seed-on-empty, seed-idempotence and
/// merge-with-existing". The seed module's OWN unit tests must carry the
/// three cases (pure functions over `ProjectState` — no harness). RED: no
/// seed function exists in any accepted home, so no `seed`-named tests exist.
/// The guard reads test-fn identifiers in the accepted homes and requires all
/// three coverage classes among the seed-named ones — the same names this
/// ticket's implementation must ship.
#[test]
fn ac5_unit_tests_cover_seed_on_empty_seed_idempotence_and_merge_with_existing() {
    let names: Vec<String> = SHIPPED_SET_HOMES
        .iter()
        .filter_map(|h| try_read(h))
        .flat_map(|src| test_fn_names(&src))
        .filter(|n| n.contains("seed"))
        .collect();
    let covers = |words: &[&str]| names.iter().any(|n| any_of(&n.to_ascii_lowercase(), words));
    let on_empty = covers(&["empty"]);
    let idempotent = covers(&["idempotent", "second", "twice", "again"]);
    let merge_existing = covers(&["existing", "merge", "preserve", "upgrade"]);
    assert!(
        on_empty && idempotent && merge_existing,
        "the seed module's unit tests do not cover the three contract cases \
         (seed-on-empty={on_empty}, seed-idempotence={idempotent}, \
         merge-with-existing={merge_existing}; seed-named tests found: \
         {names:?}) — CXA-F371 ships with tests or it does not ship"
    );
}
