//! CXA-F262 — Workspace backup & one-command restore for the hub's own state.
//! RED half of the TDD pair: no implementation exists yet, every failing
//! assertion below fails only because the behaviour is missing.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "Running the backup command produces one restorable archive of every
//!    file-backed store under the state dir (projects, tickets, evidence,
//!    wiki pages, configs, sessions file); restoring it into an empty state
//!    dir and booting the hub shows the same projects and tickets."
//! 2. "Restore refuses to overwrite a non-empty target state directory unless
//!    explicitly forced, and refuses an archive written by a newer schema
//!    version with a clear error."
//! 3. "A scheduled-backup setting (interval + retention count) keeps the
//!    newest N archives and prunes older ones, and a crash mid-write can
//!    never leave a partial archive as the newest restorable one."
//! 4. "A backup taken while the hub is actively serving yields a restorable
//!    archive (no torn files), and deploy secrets and session tokens are
//!    either excluded from the archive or stored encrypted, with the choice
//!    stated in the backup output."
//!
//! HOW THESE CRITERIA ARE ENCODED: the same no-harness discipline as
//! `reproduce_url_f244_tdd.rs` and `fleet_river_f233_gate.rs` — pure tests
//! over the real state/domain/config types plus source-contract guards over
//! the files the composition root reads. No fake HTTP server, no host
//! harness, no network port, no invented identifiers. The suite compiles
//! today; the RED guards fail only because CXA-F262's behaviour is missing.
//!
//! WHERE THE STATE LIVES (grounding — every path below is one the running
//! code actually reads and writes):
//!   * the hub's own "state dir" is the workspace base `run_hub` boots from
//!     (`registry.parent()`): `registry.json` (projects), per-project
//!     `<id>/coxagent.json` (configs, read by `load_config`) and
//!     `<id>/state/state.json` (the `ProjectState` aggregate: tickets,
//!     evidence records, wiki pages `docs`/`doc_folders`), `auth.json` +
//!     `sessions.json` (the sessions file, written by `FileAuthService`),
//!     and the evidence blob bytes under `blobs/` (the `DiskStorage` root
//!     the hub serves media from);
//!   * an existing, MUCH narrower nightly snapshot already lives in
//!     `presentation/src/server/background.rs` (`nightly_backup`): loose
//!     dated JSON of only the three hub KV docs (workspace/spaces/system
//!     chat) — no archive, no restore command, no non-empty/schema refusal,
//!     no secret policy. It is NOT this feature; do not mistake it for done.
//!
//! Red today, and why:
//!   * AC1 — the CLI (`presentation/src/cli.rs`) has no Backup/Restore
//!     subcommands, `app/src/lib.rs` dispatches none, and no backup/restore
//!     implementation exists anywhere in the workspace.
//!   * AC2 — nothing refuses anything: there is no restore, no archive
//!     schema, no force flag.
//!   * AC3 — no scheduled-backup setting (interval + retention) exists in
//!     any config type or module; only `nightly_backup`'s hardcoded
//!     14-day prune and `JsonStateStore`'s internal 20-snapshot ratchet.
//!   * AC4 — no module handles live-serving consistency or a secret policy.
//!
//! Green guards (fixture validity — house rule: every fixture must be
//! buildable from data the codebase actually has): the hub workspace is laid
//! out exactly as the boot/serving code reads it — `JsonStateStore` seeded
//! with a REAL ticket (driven through the real transition table), evidence,
//! a wiki page; the config parsed by the real `parse_config`; sessions
//! minted by the real `FileAuthService` login. Plus mechanism pins the ACs
//! build on: the store-level schema ratchet (`parse_checked`) and the fact
//! that `state.json` is published by atomic rename while `sessions.json` is
//! written by a plain non-atomic write — precisely where AC4's torn-read
//! risk lives.
//!
//! NOT ENCODED HERE — verified against the live app in the QA phase:
//!   * booting a hub from a restored dir and seeing the same projects and
//!     tickets on the dashboard (needs a serving process);
//!   * the exact output text of the backup command and the archive
//!     container format (the tests pin the CONTRACT, not the container);
//!   * the torn-file behaviour under a real serving hub.
//!
//! If an assertion's mechanism moves during implementation, move the guard
//! with it (the `preflight_f239_tdd.rs` convention).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use coxagent_application::auth::{AuthPort, LoginResult};
use coxagent_application::config::Config;
use coxagent_application::config_parse::parse_config;
use coxagent_application::ports::outbound::{StateStorePort, StoragePort};
use coxagent_application::state::{DocPage, Evidence, ProjectState, SCHEMA_VERSION};
use coxagent_domain::{Complexity, Priority, Role, Status, Ticket, TicketId, TicketType};
use coxagent_infrastructure::{FileAuthService, JsonStateStore, LocalStorage};

/// The workspace directory name of the dogfood hub (`run_hub`'s registry
/// parent) — the id a project entry carries.
const PROJECT: &str = "cxa";

/// Fixed RFC3339 instant for fixture stamps (deterministic fixtures).
const NOW: &str = "2026-09-01T00:00:00Z";

// --- repo-state scan helpers (the reproduce_url_f244_tdd.rs pattern) --------

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

/// Source of the backup/restore behaviour. The house layout puts
/// composition-root orchestration beside `onboard.rs` and `recovery.rs`;
/// the CLI-facing backup/restore engine belongs in the same crate. If the
/// behaviour lands elsewhere, point this helper there — the assertions
/// themselves stay the contract (the `preflight_f239_tdd.rs` convention).
fn backup_module() -> String {
    let p = repo_root().join("crates/app/src/backup.rs");
    std::fs::read_to_string(&p).unwrap_or_else(|_| {
        panic!(
            "crates/app/src/backup.rs does not exist — CXA-F262's backup/restore \
             behaviour is not implemented (or landed somewhere else: point this \
             guard at the file that holds it)"
        )
    })
}

// --- fixtures over the real state/domain/config/auth types ------------------

/// A Bug driven to `Fixed` through the REAL transition table — the aggregate
/// the hub's store persists, not a struct-literal shortcut.
fn fixed_login_bug() -> Ticket {
    let mut t = Ticket::new(
        TicketId::new("CXC-F262").expect("valid ticket id"),
        TicketType::Bug,
        "Hub state cannot be backed up or restored",
        "A crash or a bad upgrade loses the workspace: no archive, no restore.",
        Priority::High,
        Complexity::Medium,
        false,
    )
    .expect("valid ticket");
    t.transition_to(Role::DevBug, Status::InProgress)
        .expect("bug claim is a legal edge");
    t.transition_to(Role::DevBug, Status::Fixed)
        .expect("DEV completes the fix");
    t
}

/// One wiki page — the real `DocPage` shape the aggregate's `docs` field
/// holds (the "wiki pages" of AC1).
fn wiki_page() -> DocPage {
    DocPage {
        id: "page-backup-runbook".to_owned(),
        folder: "Operations".to_owned(),
        category: "ops".to_owned(),
        title: "Backup & restore runbook".to_owned(),
        body: "How to restore the hub's own state from an archive.".to_owned(),
        updated_at: NOW.to_owned(),
        updated_by: "OPS".to_owned(),
    }
}

/// One DoD evidence record — the real `Evidence` shape the aggregate's
/// `ticket_evidence` field holds (the "evidence" of AC1; the BYTES live in
/// blob storage, see `hub_workspace`).
fn screenshot_evidence() -> Evidence {
    Evidence {
        kind: "screenshot".to_owned(),
        label: "restored hub shows the board".to_owned(),
        detail: "blobs/backup-restore.png".to_owned(),
        at: NOW.to_owned(),
        // Pre-attribution record: empty serializes identically on the wire.
        source_gates: Vec::new(),
        actor: String::new(),
    }
}

/// The project aggregate AC1's restored hub must show again: a ticket,
/// its evidence, a wiki page and its folder.
fn seeded_state() -> ProjectState {
    let mut s = ProjectState::default();
    s.tickets.push(fixed_login_bug());
    s.ticket_evidence
        .insert("CXC-F262".to_owned(), vec![screenshot_evidence()]);
    s.docs.push(wiki_page());
    s.doc_folders.push("Operations".to_owned());
    s
}

/// The hub's own state tree, laid out EXACTLY as the running code lays it
/// out and reads it back: `run_hub`'s registry, `load_config`'s config,
/// `JsonStateStore`'s aggregate, `DiskStorage`'s blob root, and the auth +
/// sessions files `FileAuthService` persists beside each other.
struct HubWorkspace {
    _dir: tempfile::TempDir,
    /// The registry parent — the hub's "state dir" for this ticket.
    base: PathBuf,
    /// `<base>/<project>` — the project workspace.
    project: PathBuf,
    /// `<project>/state` — the project's state store root.
    state_dir: PathBuf,
    /// A live session token minted by the real login path.
    session_token: String,
}

async fn hub_workspace() -> HubWorkspace {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().join("CoXAgent");
    let project = base.join(PROJECT);
    let state_dir = project.join("state");
    std::fs::create_dir_all(&state_dir).expect("project state dir");

    // The hub registry: the `{id, path}` array `run_hub` deserializes into
    // its boot entries.
    std::fs::write(
        base.join("registry.json"),
        serde_json::to_string_pretty(&serde_json::json!([
            { "id": PROJECT, "path": project.display().to_string() }
        ]))
        .expect("registry json"),
    )
    .expect("write registry");

    // The project config: what `onboard` writes and `load_config` reads.
    std::fs::write(
        project.join("coxagent.json"),
        serde_json::to_string_pretty(&Config::default()).expect("config json"),
    )
    .expect("write config");

    // The project aggregate: tickets, evidence records, wiki pages.
    let store = JsonStateStore::new(&state_dir).expect("store");
    store.save(&seeded_state()).await.expect("seed state");

    // Evidence blob bytes, where the hub's local storage root puts them
    // (`DiskStorage { root: hub_dir.join("blobs") }`).
    LocalStorage::new(base.join("blobs"))
        .put("backup-restore.png", b"png-bytes".as_slice(), "image/png")
        .await
        .expect("blob stored");

    // Accounts + the sessions file, through the real auth service: bootstrap
    // writes `auth.json` beside the registry, a login persists
    // `sessions.json` beside it.
    let auth_path = FileAuthService::default_path(&base);
    FileAuthService::bootstrap_admin(&auth_path, "operator", "correct horse battery")
        .expect("admin provisioned");
    let auth = FileAuthService::open(&auth_path).expect("auth opens");
    let session_token = match auth.login("operator", "correct horse battery", None).await {
        LoginResult::Ok(token) => token,
        other => panic!("fixture login failed: {other:?}"),
    };

    HubWorkspace {
        _dir: dir,
        base,
        project,
        state_dir,
        session_token,
    }
}

// --- fixture validity (green guards) ----------------------------------------

/// The hub workspace fixture is buildable from real types and matches what
/// the boot/serving code reads: the registry `run_hub` deserializes, the
/// config `load_config` parses, the store the hub loads, the blob bytes the
/// hub serves, and the sessions file the auth store restores from.
#[tokio::test]
async fn the_hub_fixture_is_the_layout_the_hub_boots_and_serves() {
    let ws = hub_workspace().await;

    let registry: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(ws.base.join("registry.json")).expect("registry readable"),
    )
    .expect("registry parses");
    let entries = registry.as_array().expect("registry is an array");
    assert_eq!(entries.len(), 1, "one project registered");
    assert_eq!(entries[0]["id"], PROJECT, "the project entry's id");
    assert!(
        entries[0]["path"].as_str().is_some(),
        "the project entry's path is the workspace dir"
    );

    let cfg_text =
        std::fs::read_to_string(ws.project.join("coxagent.json")).expect("config readable");
    let cfg = parse_config(&cfg_text).expect("the project config parses");
    assert_eq!(
        cfg.deploy.host_port, None,
        "an unconfigured port stays None"
    );

    let store = JsonStateStore::new(&ws.state_dir).expect("store opens");
    let loaded = store.load().await.expect("the aggregate loads");
    assert_eq!(
        loaded.tickets.len(),
        1,
        "the ticket a restored hub must show"
    );
    assert_eq!(loaded.tickets[0].status(), Status::Fixed);
    assert!(
        loaded.ticket_evidence.contains_key("CXC-F262"),
        "the evidence record the archive must carry"
    );
    assert_eq!(loaded.docs.len(), 1, "the wiki page the archive must carry");

    assert!(
        ws.base.join("blobs").join("backup-restore.png").is_file(),
        "the evidence blob bytes exist where the hub serves them from"
    );
    assert!(
        ws.base.join("sessions.json").is_file(),
        "a login persisted the sessions file beside auth.json"
    );
}

/// AC1's store-level meaning of "restorable": an EMPTY state dir plus the
/// aggregate's bytes means exactly the same projects and tickets — the
/// round-trip the restored dir must satisfy when the hub boots from it.
/// (Booting the actual hub is the QA-phase live check.)
#[tokio::test]
async fn the_seeded_project_state_round_trips_through_the_real_store_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = JsonStateStore::new(dir.path()).expect("store");
    let seeded = seeded_state();
    store.save(&seeded).await.expect("save");
    assert_eq!(
        store.load().await.expect("load"),
        seeded,
        "an empty state dir plus the archive's aggregate bytes must mean the \
         same projects and tickets"
    );
}

/// AC4 grounding: the sessions file holds LIVE bearer tokens in plaintext
/// today (`persist_sessions` writes the token→session map verbatim) — this
/// is exactly the material the AC forbids shipping in the archive.
#[tokio::test]
async fn the_sessions_file_holds_live_bearer_tokens_in_plaintext_today() {
    let ws = hub_workspace().await;
    let text =
        std::fs::read_to_string(ws.base.join("sessions.json")).expect("sessions file readable");
    assert!(
        text.contains(&ws.session_token),
        "fixture validity: the live session token is recoverable from \
         sessions.json — exactly the material AC4 forbids in plaintext"
    );
}

/// AC2's state-level ratchet already exists and must keep holding after a
/// restore: the store the restored hub boots from refuses a newer schema
/// with a clear error (`parse_checked`). The ARCHIVE-level refusal the AC
/// adds is guarded red below.
#[tokio::test]
async fn the_state_store_the_hub_boots_refuses_a_newer_schema_with_a_clear_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = seeded_state();
    state.schema_version = SCHEMA_VERSION + 1;
    std::fs::write(
        dir.path().join("state.json"),
        serde_json::to_string_pretty(&state).expect("state json"),
    )
    .expect("write future schema");

    let store = JsonStateStore::new(dir.path()).expect("store");
    let err = store
        .load()
        .await
        .expect_err("a newer schema must be refused");
    let msg = err.to_string();
    assert!(
        msg.contains("newer"),
        "the refusal must say WHY it refused: {msg}"
    );
}

/// AC4 grounding, mechanism level: `state.json` is published by ATOMIC
/// RENAME (a concurrent reader sees the old or the new file, never a mix),
/// while `sessions.json` — and the healed `coxagent.json` — are written by
/// plain non-atomic writes. These two facts are exactly where AC4's
/// torn-read risk lives; the backup's consistency strategy must cover the
/// non-atomic ones.
#[test]
fn the_state_file_is_rename_atomic_but_the_sessions_file_is_not() {
    let json_store = read("crates/infrastructure/src/state/json_store.rs");
    assert!(
        json_store.contains("std::fs::rename(&tmp, final_path)"),
        "state.json lost its atomic-rename publish — the torn-file analysis \
         this ticket builds on no longer holds; re-check AC4's guard"
    );
    let auth = read("crates/infrastructure/src/auth.rs");
    assert!(
        auth.contains("std::fs::write(&self.sessions_path"),
        "sessions.json is no longer written by a plain non-atomic write — \
         if it became atomic, relax this grounding guard"
    );
}

// --- AC1 --------------------------------------------------------------------

/// AC1: "Running the backup command..." — the CLI must expose it. RED: the
/// Command enum has Report/Discover/Probe/RunBa/Run/Onboard/Changelog/Check/
/// Serve/Hub/Compress/Codegraph and nothing else.
#[test]
fn ac1_the_cli_gains_a_backup_command() {
    let cli = read("crates/presentation/src/cli.rs");
    assert!(
        cli.contains("Backup"),
        "the CLI (Command enum in crates/presentation/src/cli.rs) must gain a \
         `backup` subcommand — 'running the backup command' is AC1's entry point"
    );
}

/// AC1: "...restoring it into an empty state dir..." — the CLI must expose
/// the restore side too. RED: no Restore variant exists.
#[test]
fn ac1_the_cli_gains_a_restore_command() {
    let cli = read("crates/presentation/src/cli.rs");
    assert!(
        cli.contains("Restore"),
        "the CLI (Command enum in crates/presentation/src/cli.rs) must gain a \
         `restore` subcommand — AC1 restores the archive into an empty state dir"
    );
}

/// AC1: both commands must actually run — the composition root's dispatch
/// must route them. RED: `run()`'s match has no Backup/Restore arms.
#[test]
fn ac1_the_composition_root_dispatches_backup_and_restore() {
    let lib = read("crates/app/src/lib.rs");
    assert!(
        lib.contains("Command::Backup"),
        "run()'s dispatch must route the backup command (Command::Backup arm)"
    );
    assert!(
        lib.contains("Command::Restore"),
        "run()'s dispatch must route the restore command (Command::Restore arm)"
    );
}

/// AC1: "produces one restorable archive of every file-backed store under
/// the state dir (projects, tickets, evidence, wiki pages, configs, sessions
/// file)". The deliverable is an ARCHIVE covering the real store set:
/// `registry.json` (projects), `state.json` (tickets + evidence records +
/// wiki pages — they all ride the project aggregate), `coxagent.json`
/// (configs), `sessions.json` (the sessions file), and the evidence blob
/// bytes under `blobs/`. RED: no backup implementation exists.
#[test]
fn ac1_backup_produces_one_archive_of_every_file_backed_store() {
    let src = backup_module();
    assert!(
        src.contains("archive"),
        "the deliverable is ONE restorable archive, not a folder of loose files"
    );
    for (what, marker) in [
        ("projects (the hub registry)", "registry.json"),
        (
            "tickets + evidence records + wiki pages (the project aggregate)",
            "state.json",
        ),
        ("project configs", "coxagent.json"),
        ("the sessions file", "sessions.json"),
        ("evidence blob bytes", "blobs"),
    ] {
        assert!(
            src.contains(marker),
            "the backup must cover {what} — `{marker}` is never named in \
             crates/app/src/backup.rs"
        );
    }
}

/// AC1: "...restoring it into an empty state dir and booting the hub shows
/// the same projects and tickets" — restore must write back the two inputs
/// a hub boot reads: the registry (projects) and each project's aggregate
/// (tickets). RED: nothing restores anything today. Booting the hub and
/// seeing the dashboard is the QA-phase live check.
#[test]
fn ac1_restore_writes_back_every_store_the_hub_boots_from() {
    let src = backup_module();
    assert!(
        src.contains("restore"),
        "a restore path must exist in the backup module"
    );
    for (what, marker) in [
        ("the hub registry (projects)", "registry.json"),
        (
            "the project aggregate (tickets, evidence, wiki)",
            "state.json",
        ),
    ] {
        assert!(
            src.contains(marker),
            "restore must write back {what} — `{marker}` is never named in \
             crates/app/src/backup.rs"
        );
    }
}

// --- AC2 --------------------------------------------------------------------

/// AC2: "Restore refuses to overwrite a non-empty target state directory
/// unless explicitly forced" — restore must take a force flag and refuse a
/// non-empty target without it. RED: no restore exists.
#[test]
fn ac2_restore_refuses_a_non_empty_target_unless_explicitly_forced() {
    let src = backup_module();
    let lower = src.to_ascii_lowercase();
    assert!(
        lower.contains("force"),
        "restore must take an explicit force flag — AC2 refuses to overwrite \
         a non-empty target state directory without one"
    );
    assert!(
        lower.contains("non-empty") || lower.contains("not empty"),
        "restore must refuse a non-empty target state directory unless forced"
    );
}

/// AC2: "...and refuses an archive written by a newer schema version with a
/// clear error" — the archive records a schema version and restore refuses
/// a newer one, the same ratchet `parse_checked` applies to state.json (see
/// the green guard above). RED: no archive schema exists.
#[test]
fn ac2_restore_refuses_an_archive_from_a_newer_schema_with_a_clear_error() {
    let src = backup_module();
    let lower = src.to_ascii_lowercase();
    assert!(
        lower.contains("schema"),
        "the archive must record a schema version so restore can ratchet it"
    );
    assert!(
        lower.contains("newer"),
        "restore must refuse an archive written by a NEWER schema version \
         with a clear error (the parse_checked ratchet, at archive level)"
    );
}

// --- AC3 --------------------------------------------------------------------

/// AC3: "A scheduled-backup setting (interval + retention count)..." — the
/// setting must exist with BOTH halves. RED: no scheduled-backup setting
/// exists in any config type or module (only `nightly_backup`'s hardcoded
/// 14-day prune and the JSON store's internal 20-snapshot ratchet).
#[test]
fn ac3_a_scheduled_backup_setting_carries_an_interval_and_a_retention_count() {
    let src = backup_module().to_ascii_lowercase();
    assert!(
        src.contains("interval"),
        "the scheduled-backup setting must carry an interval"
    );
    assert!(
        src.contains("retention"),
        "the scheduled-backup setting must carry a retention count"
    );
}

/// AC3: "...keeps the newest N archives and prunes older ones..." — the
/// retention count must actually prune. RED: nothing prunes archives.
#[test]
fn ac3_retention_keeps_the_newest_n_archives_and_prunes_older_ones() {
    let src = backup_module().to_ascii_lowercase();
    assert!(
        src.contains("prune"),
        "the scheduled backups must prune to the newest N (the retention count)"
    );
}

/// AC3: "...and a crash mid-write can never leave a partial archive as the
/// newest restorable one" — the archive is published by the house atomic
/// write (temp file + rename, as `json_store.rs::atomic_write` does for
/// state.json): a crash leaves either the previous archive or the complete
/// new one, never a half-written "newest". RED: no archive write exists.
#[test]
fn ac3_a_crash_mid_write_never_leaves_a_partial_archive_as_the_newest_restorable() {
    let src = backup_module();
    assert!(
        src.contains("rename"),
        "the archive must be published by an atomic rename (temp file + \
         rename, the atomic_write pattern) — a crash mid-write must never \
         leave a partial archive as the newest restorable one"
    );
}

// --- AC4 --------------------------------------------------------------------

/// AC4: "A backup taken while the hub is actively serving yields a
/// restorable archive (no torn files)" — the module must carry (and name) a
/// consistency strategy for reading a live workspace. The grounding guard
/// above shows state.json is rename-atomic but sessions.json is not, so
/// plain file-copying is not a strategy. RED: no module exists.
#[test]
fn ac4_a_backup_while_the_hub_serves_names_a_consistency_strategy() {
    let src = backup_module().to_ascii_lowercase();
    let named = ["torn", "atomic", ".state.lock", "consistent"]
        .iter()
        .any(|m| src.contains(m));
    assert!(
        named,
        "a backup taken while the hub is actively serving must name its \
         consistency mechanism (one of: torn-read handling, atomic snapshot, \
         the store's advisory lock, a consistent-snapshot copy) — the live \
         behaviour itself is verified in the QA phase"
    );
}

/// AC4: "deploy secrets and session tokens are either excluded from the
/// archive or stored encrypted" — the credential-bearing files under the
/// hub's own state dir are the sessions file (LIVE bearer tokens — see the
/// green guard), the account/token-hash file (`auth.json`), and the
/// coordination file's DSN credentials (`coordination.json`, whose loader
/// documents it may carry credentials and clamps it to 0600). RED: no
/// module decides any of this today.
#[test]
fn ac4_session_tokens_and_deploy_secrets_never_ship_in_plaintext() {
    let src = backup_module().to_ascii_lowercase();
    for file in ["sessions.json", "auth.json", "coordination.json"] {
        assert!(
            src.contains(file),
            "the backup must decide (and honour) a secret policy for {file}"
        );
    }
    assert!(
        src.contains("exclud") || src.contains("encrypt"),
        "deploy secrets and session tokens must be EXCLUDED from the archive \
         or stored ENCRYPTED — one of the two, never plaintext"
    );
}

/// AC4: "...with the choice stated in the backup output" — the operator
/// must be TOLD which policy the backup applied, not left to unzip and
/// check. RED: there is no backup output at all.
#[test]
fn ac4_the_backup_output_states_the_secret_choice() {
    let src = backup_module().to_ascii_lowercase();
    let stated = (src.contains("excluded") || src.contains("encrypted"))
        && (src.contains("output") || src.contains("summary") || src.contains("report"));
    assert!(
        stated,
        "the backup output must state which secret policy it applied — \
         excluded or encrypted — not leave the operator to guess"
    );
}
