//! CXA-F267 — MongoTicketArchive cold-store adapter (CXA-F264b, from
//! CXA-F264). RED half of the TDD pair.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "MongoTicketArchive implements TicketArchivePort; put is an upsert
//!    (archiving the same ticket twice succeeds and keeps one document per
//!    (project, id))."
//! 2. "from_env returns Ok(None) for unset/blank COXAGENT_MONGO_URL and
//!    PortError::Backend (with the underlying message) when the URL is set
//!    but unreachable; startup falls back without crashing."
//! 3. "Unique index on (project, id) is created on construction; BSON
//!    round-trip test proves a fully populated ArchivedTicket (criteria,
//!    test cases, evidence) survives encode/decode unchanged."
//! 4. "build_* wiring compiles unconditionally and yields None with no Mongo
//!    env; cargo check --workspace + clippy --all-targets + hex gate green;
//!    compose security gate still passes (no new secrets or bare ports)."
//!
//! AC-NAMING GAP, FLAGGED NOT FABRICATED: the codebase has no
//! `TicketArchivePort` and no `ArchivedTicket` type — the cold-store port is
//! `ArchiveStorePort` (`crates/application/src/ports/outbound/archive.rs`,
//! CXA-F273/F274) and it persists the whole `Ticket` aggregate (see its
//! signature and the F274 read-back design). The identifiers the AC names do
//! not exist, so per the no-invented-identifiers rule these guards pin the
//! AC against the port and aggregate the codebase actually has:
//! `TicketArchivePort` → `ArchiveStorePort`, `ArchivedTicket` → a fully
//! populated `Ticket` (which carries exactly the AC's criteria, test cases
//! and evidence). The type name the AC does coin — `MongoTicketArchive` — is
//! the one identifier with nothing to map to, so it is pinned as the
//! adapter's name (string-scanned, never compiled).
//!
//! HOW THESE CRITERIA ARE ENCODED: pure source-scan guards over the repo
//! (the `fail_closed_test_db_f327_tdd.rs` / `archive_read_back_f274_tdd.rs`
//! discipline) plus green controls over types that exist today — no fake
//! HTTP server, no host harness, no network port, no invented identifiers.
//! The adapter cannot be called from a compiled test before it exists (the
//! symbol is absent from the workspace — verified before writing this file),
//! so the red half pins the missing behaviour where it must be declared, and
//! the green controls pin the contract semantics it must reproduce:
//!   * the BSON round-trip over a fully populated `Ticket` — buildable today
//!     with the real domain API, and exactly the encode/decode shape the
//!     adapter's `put`/`get` must preserve (AC3's round-trip half);
//!   * the upsert-per-(project, id) contract on the live `MemoryArchiveStore`
//!     — the port's documented semantics AC1 restates;
//!   * the `Ok(None)` env contract on the live `MongoDocStore::from_env` —
//!     the documented `COXAGENT_MONGO_URL` contract AC2 restates (unset and
//!     blank both return before any IO, so the control stays harness-free).
//!
//! Where the missing pieces must live: the adapter goes beside its dev/e2e
//! sibling `crates/infrastructure/src/archive_memory.rs` (one adapter per
//! file, named for what it is) and is re-exported from
//! `crates/infrastructure/src/lib.rs` so `build_archive_store` can wire it;
//! the builders wiring extends `build_archive_store` in
//! `crates/app/src/builders.rs` (the slot the memory adapter stands in
//! today, per its own doc comment).
//!
//! Red today, and why (verified before writing this file):
//!   * AC1 — no `MongoTicketArchive` exists anywhere in the workspace; the
//!     archive slot in `lib.rs` exports only `MemoryArchiveStore`.
//!   * AC2 — likewise no `from_env` on any archive adapter; the env contract
//!     exists only on `MongoDocStore`/`MemoryArchiveStore`.
//!   * AC3 — no adapter exists to create the index; the BSON round trip is
//!     green (the shape is pinned, the creator is missing).
//!   * AC4 — `build_archive_store` consults `MemoryArchiveStore` only; the
//!     `MongoTicketArchive::from_env` wiring is absent.
//!   * AC4's gate half (cargo check/clippy/hex gate/compose security gate)
//!     is process verification, encoded by the existing gate suites and re
//!     -run on the implementation PR — no test here duplicates them.
//!
//! AC → test map:
//! - AC1: [`ac1_mongo_ticket_archive_exists_and_implements_the_archive_store_port`]
//!   (RED), [`ac1_put_is_an_upsert_keyed_on_project_and_id`] (RED), plus the
//!   green [`the_archive_port_upsert_contract_holds_on_the_live_adapter`]
//! - AC2: [`ac2_from_env_follows_the_documented_mongo_env_contract`] (RED),
//!   plus the green
//!   [`the_mongo_env_contract_the_archive_must_match_answers_none_without_a_url`]
//! - AC3: [`ac3_a_unique_project_id_index_is_created_on_construction`] (RED),
//!   plus the green [`a_fully_populated_ticket_survives_a_bson_round_trip`]
//! - AC4: [`ac4_build_wiring_compiles_unconditionally_and_yields_none_with_no_mongo_env`]
//!   (RED on the Mongo half), plus the green
//!   [`the_mongodb_dependency_is_not_hidden_behind_a_feature_gate`]

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use coxagent_application::ports::outbound::ArchiveStorePort;
use coxagent_domain::{Role, TechnicalDesign, Ticket, TicketId, TicketType};

/// The adapter type name AC1 coins — string-scanned, never compiled.
const ADAPTER: &str = "MongoTicketArchive";

/// The cold-store port the codebase actually has (see the header's AC-naming
/// gap note).
const PORT: &str = "ArchiveStorePort";

/// The port's declaration site — the AC's "TicketArchivePort" maps here.
const PORT_FILE: &str = "crates/application/src/ports/outbound/archive.rs";

/// The infrastructure lib the adapter must be re-exported from.
const INFRA_LIB: &str = "crates/infrastructure/src/lib.rs";

/// The builders file holding the `build_*` wiring AC4 extends.
const BUILDERS: &str = "crates/app/src/builders.rs";

/// The builders fn that hands the port its adapter.
const BUILD_FN: &str = "build_archive_store";

/// The env var AC2 names (the docs store's documented contract).
const MONGO_URL_VAR: &str = "COXAGENT_MONGO_URL";

/// How far past a needle a window may reach. A `from_env` body is several
/// flattened lines; a cap keeps an UNRELATED later region from being
/// attributed to the needle (the `fail_closed_test_db_f327_tdd.rs` WINDOW
/// convention, widened for whole-function bodies).
const WINDOW: usize = 900;

// --- repo-state scan helpers (the fail_closed_test_db_f327_tdd.rs pattern) ---

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

/// The source with ALL whitespace removed, case preserved — guards match
/// exact source tokens in any formatting.
fn flat(src: &str) -> String {
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Successive windows of flattened source: from each occurrence of `needle`
/// to just before the next occurrence (or [`WINDOW`] chars later, whichever
/// is nearer). Windows are proximity heuristics over flat text, not parses.
fn windows<'a>(src: &'a str, needle: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(at) = rest.find(needle) {
        let after = &rest[at..];
        let next = after[needle.len()..]
            .find(needle)
            .map(|rel| needle.len() + rel);
        let end = ceil_boundary(after, next.unwrap_or(after.len()).min(WINDOW));
        out.push(&after[..end]);
        rest = &after[end..];
    }
    out
}

/// A window CENTERED on each needle occurrence: `radius` flattened chars on
/// both sides. Needed where the guarded tokens sit ABOVE the needle — the
/// index model (keys, `unique`) is built before its `create_index` call, so
/// a forward-only window would miss them.
fn near<'a>(src: &'a str, needle: &str, radius: usize) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut offset = 0;
    while let Some(rel) = src[offset..].find(needle) {
        let at = offset + rel;
        let start = floor_boundary(src, at.saturating_sub(radius));
        let end = ceil_boundary(src, (at + needle.len() + radius).min(src.len()));
        out.push(&src[start..end]);
        offset = at + needle.len();
    }
    out
}

/// Snap a byte offset down to a char boundary (flat sources can carry
/// non-ASCII comment text; slicing mid-char would panic).
fn floor_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Snap a byte offset up to a char boundary.
fn ceil_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// The adapter's source, wherever under `crates/infrastructure/src` it is
/// declared — the AC names the type, not the file, so the guard finds the
/// declaration instead of pinning a path the ticket never stated.
fn find_adapter(dir: &Path) -> Option<(PathBuf, String)> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(found) = find_adapter(&p) {
                return Some(found);
            }
        } else if p.extension().is_some_and(|e| e == "rs") {
            if let Ok(src) = std::fs::read_to_string(&p) {
                if flat(&src).contains(&format!("struct{ADAPTER}")) {
                    return Some((p, src));
                }
            }
        }
    }
    None
}

fn adapter_src() -> (PathBuf, String) {
    let root = repo_root().join("crates/infrastructure/src");
    find_adapter(&root).unwrap_or_else(|| {
        panic!(
            "no adapter declares `struct {ADAPTER}` anywhere under \
             crates/infrastructure/src — the cold-store adapter the ACs name \
             does not exist yet (AC1/AC2/AC3 all red on this)"
        )
    })
}

// --- green fixtures over the real domain types -------------------------------

/// A fully populated archived ticket — every part AC3 names (criteria, test
/// cases, evidence) set through the real domain API, so the BSON round-trip
/// fixture is data the codebase actually has (the `test_case.rs` /
/// `archive_read_back_f274_tdd.rs` fixture conventions).
fn fully_populated_ticket(id: &str) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new(id).unwrap(),
        TicketType::Feature,
        "Mongo cold store keeps archived tickets whole",
        "eviction must never lose criteria, test cases or evidence",
        coxagent_domain::Priority::High,
        coxagent_domain::Complexity::Medium,
        false,
    )
    .unwrap();
    t.set_technical_design(
        Role::Sa,
        TechnicalDesign {
            approach: "MongoTicketArchive behind the ArchiveStorePort".to_owned(),
            files: vec!["crates/infrastructure/src/archive_mongo.rs".to_owned()],
            ..TechnicalDesign::default()
        },
    )
    .unwrap();
    t.set_acceptance_criteria(vec![
        "put is an upsert keyed on (project, id)".to_owned(),
        "a fully populated ticket survives the round trip".to_owned(),
    ]);
    t.stamp_created_at("2026-09-07T00:00:00Z");
    t.ensure_test_cases_from_acceptance();
    assert!(
        t.set_test_case_result(
            "put is an upsert keyed on (project, id)",
            true,
            Some("archived twice, one row".to_owned()),
            Some("/api/projects/cxa/media/f267.png".to_owned()),
            "2026-09-07T09:00:00Z".to_owned(),
        ),
        "the fixture's first case must exist to carry its verdict"
    );
    assert!(
        t.set_test_case_repro(
            "put is an upsert keyed on (project, id)",
            "http://127.0.0.1:4000/projects".to_owned(),
        ),
        "the fixture's repro attaches to existing evidence"
    );
    assert!(
        t.set_test_case_result(
            "a fully populated ticket survives the round trip",
            false,
            Some("round trip pending the adapter".to_owned()),
            None,
            "2026-09-07T09:05:00Z".to_owned(),
        ),
        "the fixture's second case must exist to carry its verdict"
    );
    t
}

// --- green guards: contract semantics over types that exist today ------------

/// AC3's round-trip half, pinned on the type the archive persists: a fully
/// populated `Ticket` (criteria, test cases with verdicts, evidence with
/// image/note/repro, verdict history, the SA design) survives a BSON
/// encode/decode unchanged. The adapter's `put`/`get` must persist exactly
/// this shape — the green control stays as the regression anchor once the
/// adapter lands.
#[test]
fn a_fully_populated_ticket_survives_a_bson_round_trip() {
    let t = fully_populated_ticket("CXC-F267-001");
    assert_eq!(t.acceptance_criteria().len(), 2, "fixture has criteria");
    assert_eq!(t.test_cases().len(), 2, "fixture has synced test cases");

    let doc = mongodb::bson::to_document(&t).unwrap_or_else(|e| panic!("bson encode: {e}"));
    let back: Ticket =
        mongodb::bson::from_document(doc).unwrap_or_else(|e| panic!("bson decode: {e}"));

    assert_eq!(back, t, "the full aggregate survives BSON unchanged");
    assert_eq!(back.id().as_str(), "CXC-F267-001", "id intact");
    assert_eq!(
        back.acceptance_criteria(),
        t.acceptance_criteria(),
        "criteria intact"
    );
    assert_eq!(back.test_cases(), t.test_cases(), "test cases intact");
    let ev = back.test_cases()[0]
        .evidence
        .as_ref()
        .expect("the verified case keeps its evidence");
    assert_eq!(ev.note.as_deref(), Some("archived twice, one row"));
    assert_eq!(
        ev.image.as_deref(),
        Some("/api/projects/cxa/media/f267.png")
    );
    assert_eq!(ev.repro.as_deref(), Some("http://127.0.0.1:4000/projects"));
    assert!(
        !back.test_cases()[0].history.is_empty(),
        "the verdict history survives — the churn view must not blank out"
    );
    assert_eq!(
        back.design().technical.clone().map(|d| d.approach),
        t.design().technical.clone().map(|d| d.approach),
        "the SA design survives"
    );
    assert_eq!(back.created_at(), t.created_at(), "created_at intact");
}

/// AC1's semantics, pinned on the port's live implementation: `put` is an
/// idempotent upsert — archiving the same ticket twice succeeds and keeps
/// ONE row per (project, id), latest version wins, and projects never see
/// each other's rows. The Mongo adapter must reproduce exactly this
/// contract (its `put` is the same port method).
#[tokio::test]
async fn the_archive_port_upsert_contract_holds_on_the_live_adapter() {
    let store = coxagent_infrastructure::MemoryArchiveStore::new();
    let first = fully_populated_ticket("CXC-F267-002");
    store.put("demo", &first).await.unwrap();
    // The retried eviction: same (project, id), edited title — replaces, never duplicates.
    let mut retried = fully_populated_ticket("CXC-F267-002");
    retried
        .edit(Role::Po, "renamed by the retry", "same body, fresh title")
        .unwrap();
    store.put("demo", &retried).await.unwrap();
    // A same-id ticket under ANOTHER project is a different document.
    store.put("other", &first).await.unwrap();

    let listed = store.list("demo").await.unwrap();
    assert_eq!(
        listed.len(),
        1,
        "archiving the same ticket twice keeps one document per (project, id)"
    );
    assert_eq!(
        listed[0].title(),
        retried.title(),
        "the latest version wins"
    );
    assert_eq!(
        store.get("demo", "CXC-F267-002").await.unwrap().unwrap(),
        retried,
        "get serves the latest archived version"
    );
    assert_eq!(
        store.get("other", "CXC-F267-002").await.unwrap().unwrap(),
        first,
        "the (project, id) key scopes documents per project"
    );
}

/// The env contract AC2 restates is the docs store's documented one
/// (`docs_store.rs`: unset or blank `COXAGENT_MONGO_URL` → `Ok(None)` before
/// any IO). Pinned here on the live `MongoDocStore::from_env` so the archive
/// adapter's `from_env` has a real reference contract — both branches return
/// before any connection attempt, so this stays harness-free.
#[tokio::test]
async fn the_mongo_env_contract_the_archive_must_match_answers_none_without_a_url() {
    // Async-aware lock: the guard is legitimately held across the awaited
    // from_env calls (a std MutexGuard would trip clippy::await_holding_lock).
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = ENV_LOCK.lock().await;
    let prior = std::env::var(MONGO_URL_VAR).ok();

    std::env::remove_var(MONGO_URL_VAR);
    assert!(
        coxagent_infrastructure::MongoDocStore::from_env()
            .await
            .unwrap()
            .is_none(),
        "unset COXAGENT_MONGO_URL must answer Ok(None) — the contract AC2 restates"
    );
    std::env::set_var(MONGO_URL_VAR, "   ");
    assert!(
        coxagent_infrastructure::MongoDocStore::from_env()
            .await
            .unwrap()
            .is_none(),
        "a blank COXAGENT_MONGO_URL must answer Ok(None), never an error or a connect"
    );

    match prior {
        Some(v) => std::env::set_var(MONGO_URL_VAR, v),
        None => std::env::remove_var(MONGO_URL_VAR),
    }
}

/// AC4's "compiles unconditionally": the mongodb dependency is a plain
/// workspace dependency of the adapter's crate — no feature gate may hide
/// the adapter (a cfg-gated build_archive_store would silently drop the cold
/// store from default builds).
#[test]
fn the_mongodb_dependency_is_not_hidden_behind_a_feature_gate() {
    let cargo = flat(&read("crates/infrastructure/Cargo.toml"));
    assert!(
        cargo.contains("mongodb.workspace=true"),
        "the adapter's crate must depend on mongodb unconditionally"
    );
    assert!(
        !cargo.contains("[features]"),
        "no [features] section may gate the mongo stack — the wiring must \
         compile unconditionally (AC4)"
    );
}

// --- red guards: the missing behaviour, pinned where it must be declared -----

/// AC1: the adapter exists and implements the cold-store port (the AC's
/// "TicketArchivePort" — see the header's naming-gap note), and is exported
/// so the builders can wire it.
#[test]
fn ac1_mongo_ticket_archive_exists_and_implements_the_archive_store_port() {
    // The port the AC names is the port the codebase has — if this drifts,
    // the AC's naming gap grew and the header must be revisited.
    assert!(
        flat(&read(PORT_FILE)).contains(&format!("pubtrait{PORT}")),
        "{PORT_FILE} must still declare `pub trait {PORT}` — the port AC1 \
         calls TicketArchivePort"
    );

    let (path, src) = adapter_src();
    let f = flat(&src);
    assert!(
        f.contains(&format!("impl{PORT}for{ADAPTER}")),
        "{path:?} must implement `{PORT}` for `{ADAPTER}` — a cold store that \
         does not sit behind the port cannot serve the archive read-back (AC1)"
    );
    assert!(
        flat(&read(INFRA_LIB)).contains(ADAPTER),
        "{INFRA_LIB} must re-export `{ADAPTER}` so build_* wiring can reach it"
    );
}

/// AC1: `put` is an upsert — the write is an upsert operation whose filter
/// keys the document on BOTH project and id, so archiving the same ticket
/// twice succeeds and keeps one document per (project, id).
#[test]
fn ac1_put_is_an_upsert_keyed_on_project_and_id() {
    let (_, src) = adapter_src();
    let f = flat(&src);
    assert!(
        f.contains("asyncfnput("),
        "the adapter must implement the port's `put` (AC1)"
    );
    assert!(
        f.contains("upsert(true)"),
        "`put` must write through the driver's upsert (`.upsert(true)`, the \
         docs-store convention) — an insert would duplicate a retried \
         eviction (AC1: put is an upsert)"
    );
    assert!(
        windows(&f, "\"project\"")
            .iter()
            .any(|w| w.contains("\"id\"")),
        "the write filter must key on BOTH `project` and `id` — one document \
         per (project, id), never per id alone (AC1)"
    );
}

/// AC2: `from_env` follows the documented Mongo env contract — `Ok(None)`
/// for unset/blank `COXAGENT_MONGO_URL`; `PortError::Backend` carrying the
/// underlying message when the URL is set but unreachable; and no panic
/// path, so startup can log and fall back instead of crashing.
#[test]
fn ac2_from_env_follows_the_documented_mongo_env_contract() {
    let (_, src) = adapter_src();
    let f = flat(&src);
    assert!(
        f.contains("asyncfnfrom_env") && f.contains("Result<Option<Self>,PortError>"),
        "the adapter must expose `async fn from_env() -> \
         Result<Option<Self>, PortError>` — the shape startup falls back on \
         (AC2)"
    );
    let env_windows = windows(&f, MONGO_URL_VAR);
    assert!(
        !env_windows.is_empty(),
        "`from_env` must read {MONGO_URL_VAR} (AC2)"
    );
    assert!(
        env_windows
            .iter()
            .any(|w| w.contains("trim") && w.contains("is_empty")),
        "a blank {MONGO_URL_VAR} must return Ok(None) (trim + is_empty check), \
         not attempt a connect (AC2)"
    );
    assert!(
        env_windows
            .iter()
            .any(|w| w.contains("PortError::Backend") && w.contains("map_err")),
        "an unreachable URL must map to PortError::Backend via map_err so the \
         underlying message survives for the startup warn (AC2)"
    );
    let from_env_window = windows(&f, "fnfrom_env");
    assert!(
        from_env_window
            .iter()
            .all(|w| !w.contains("panic!") && !w.contains(".unwrap()") && !w.contains(".expect(")),
        "`from_env` must never panic — startup falls back without crashing \
         (AC2); a panic in from_env takes the hub down"
    );
}

/// AC3: construction creates the unique (project, id) index — the database
/// -level backstop of AC1's upsert semantics. The index build sits in the
/// constructor (`from_env`), not lazily on first put, so a mis-configured
/// cold store fails at startup where the fallback can still act.
#[test]
fn ac3_a_unique_project_id_index_is_created_on_construction() {
    let (_, src) = adapter_src();
    let f = flat(&src);
    assert!(
        f.contains("create_index"),
        "the adapter must create an index (driver `create_index`/\
         `create_indexes`) — without it a race can archive the same ticket \
         twice into two documents (AC3)"
    );
    assert!(
        near(&f, "create_index", 400)
            .iter()
            .any(|w| w.contains("unique") && w.contains("\"project\"") && w.contains("\"id\"")),
        "the index must be UNIQUE on (project, id) — the upsert filter's \
         composite key, enforced by the database (AC3)"
    );
    assert!(
        windows(&f.to_ascii_lowercase(), "fnfrom_env")
            .iter()
            .any(|w| w.contains("index")),
        "the index must be created ON CONSTRUCTION (from `from_env`), not \
         lazily on first put (AC3) — if the creation lives in a helper, call \
         it from the constructor so this guard sees it"
    );
}

/// AC4: `build_archive_store` wires the Mongo adapter unconditionally and
/// keeps its no-env answer `None` — the memory adapter stands in only for
/// dev/e2e, and with no Mongo env the archive slot stays empty exactly as
/// today.
#[test]
fn ac4_build_wiring_compiles_unconditionally_and_yields_none_with_no_mongo_env() {
    let b = flat(&read(BUILDERS));
    let wiring = windows(&b, &format!("fn{BUILD_FN}"));
    assert!(
        wiring
            .iter()
            .any(|w| w.contains(ADAPTER) && w.contains("from_env")),
        "{BUILD_FN} must consult `{ADAPTER}::from_env` — the cold store slot \
         still wires only the in-memory stand-in (AC4)"
    );
    assert!(
        wiring.iter().all(|w| !w.contains("#[cfg(feature")),
        "the {BUILD_FN} wiring must compile unconditionally — no feature gate \
         may hide the cold store from default builds (AC4)"
    );
    assert!(
        wiring.iter().any(|w| w.contains("Ok(None)=>None")),
        "{BUILD_FN} must keep yielding None when no Mongo env is set — an \
         unconfigured hub answers empty, exactly the pre-archive behavior (AC4)"
    );
}

// --- scanner bite controls (the fail_closed_test_db_f327_tdd.rs pattern) -----

/// The AC2/AC3 scanners stay sharp: each must fire on a source shape missing
/// its required token and stay quiet on a shape carrying it — so a guard can
/// never rot into a vacuous pass.
#[test]
fn the_env_and_index_scanners_catch_the_shapes_they_guard() {
    let good_env = flat(
        r#"let url = std::env::var("COXAGENT_MONGO_URL").unwrap_or_default();
           if url.trim().is_empty() { return Ok(None); }
           let client = Client::with_uri_str(&url).await
               .map_err(|e| PortError::Backend(format!("mongo connect: {e}")))?;"#,
    );
    assert!(windows(&good_env, MONGO_URL_VAR)
        .iter()
        .any(|w| w.contains("trim") && w.contains("is_empty")));
    assert!(windows(&good_env, MONGO_URL_VAR)
        .iter()
        .any(|w| w.contains("PortError::Backend") && w.contains("map_err")));

    // Bite: a from_env that connects without the blank check or the Backend
    // mapping must fail both scanners.
    let bad_env = flat(
        r#"let url = std::env::var("COXAGENT_MONGO_URL").unwrap_or_default();
           let client = Client::with_uri_str(&url).await.unwrap();"#,
    );
    assert!(
        !windows(&bad_env, MONGO_URL_VAR)
            .iter()
            .any(|w| w.contains("trim") && w.contains("is_empty")),
        "the blank-check scanner must bite on a missing blank check"
    );
    assert!(
        !windows(&bad_env, MONGO_URL_VAR)
            .iter()
            .any(|w| w.contains("PortError::Backend")),
        "the Backend scanner must bite on an unmapped error"
    );

    let good_index = flat(
        r#"let model = IndexModel::builder()
               .keys(doc! { "project": 1, "id": 1 })
               .options(IndexOptions::builder().unique(true).build())
               .build();
           coll.create_index(model).await?;"#,
    );
    assert!(
        near(&good_index, "create_index", 400)
            .iter()
            .any(|w| w.contains("unique") && w.contains("\"project\"") && w.contains("\"id\"")),
        "the index scanner must accept a unique (project, id) index"
    );
    // Bite: a NON-unique index must fail the uniqueness half.
    let bad_index = flat(
        r#"let model = IndexModel::builder()
               .keys(doc! { "project": 1, "id": 1 })
               .build();
           coll.create_index(model).await?;"#,
    );
    assert!(
        !near(&bad_index, "create_index", 400)
            .iter()
            .any(|w| w.contains("unique")),
        "the index scanner must bite on a missing unique option"
    );
}
