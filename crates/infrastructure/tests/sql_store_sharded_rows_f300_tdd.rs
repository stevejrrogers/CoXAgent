//! CXA-F300 (CXA-C019b: `SqlStateStore` sharded rows — per-shard JSONB
//! storage, legacy-row migration, shard-scoped `claim_ticket`). RED half of
//! the TDD pair, in the `fail_closed_test_db_f327_tdd.rs` /
//! `mongo_ticket_archive_f267_tdd.rs` discipline: pure source-scan guards over
//! the repo plus scanner bite controls — no fake HTTP server, no host
//! harness, no network port, no invented identifiers.
//!
//! The acceptance criteria, verbatim:
//! 1. "New shard table + head-revision row created idempotently in INIT_SQL; a
//!    project saved by the OLD code path (legacy single-blob row) loads
//!    correctly and is migrated to shards on next save, with data identical
//!    pre/post migration"
//! 2. "save/load/save_expecting round-trip, Conflict-on-stale-revision, and
//!    current_version behaviour identical to today (existing
//!    sql_store_contract.rs assertions stay green)"
//! 3. "claim_ticket performs SELECT FOR UPDATE only on the Tickets shard row;
//!    a contract test proves two sequential claims cannot both win and a claim
//!    on another shard's data does not take the whole-row lock"
//! 4. "gate_save refusal still quarantines and returns Err before any shard
//!    row is written (extend existing F229 test)"
//! 5. "Contract tests added to crates/infrastructure/tests/sql_store_contract.rs
//!    (env-gated via COXAGENT_TEST_PG_DSN as today): legacy migration,
//!    per-shard round-trip, cross-revision conflict; cargo test green"
//!
//! AC-NAMING NOTES, FLAGGED NOT FABRICATED:
//!   * The criteria coin no table or column names, so none are invented: the
//!     schema is DISCOVERED from the implementation's own `INIT_SQL` by
//!     `common::shard_schema_f300` (RED, naming AC1, until C019b lands).
//!   * "The Tickets shard row" is `ShardKind::Work` — `state::shards.rs`
//!     places `tickets` in the Work payload (CXA-C019a), and its row label is
//!     `ShardKind`'s own serde vocabulary.
//!   * AC5's "env-gated via COXAGENT_TEST_PG_DSN as today": since CXA-F327 the
//!     suites' gating mechanism is the fail-closed compose fixture
//!     (`common::claim_or_skip()`), which REFUSES an exported DSN by policy —
//!     the DB-backed half of these criteria is encoded there, on today's
//!     actual mechanism.
//!
//! Red today, and why (verified before writing this file):
//!   * AC1 — `INIT_SQL` declares only the legacy whole-document
//!     `project_state` table; no shard table, no head-revision seed, and
//!     `sql_store.rs` never references `into_shards`/`from_shards` or any
//!     shard table outside its DDL.
//!   * AC3 — `claim_ticket` takes `SELECT … FROM project_state WHERE …
//!     FOR UPDATE`: the whole-aggregate row lock the criteria remove.
//!   * AC2/AC5's equivalence halves and AC4's gate ordering are "stay green"
//!     criteria — their encodings are green controls by construction (the
//!     f267 pattern), and the DB-backed shard behaviour they must survive
//!     lives in `sql_store_contract.rs`'s new F300 tests, which fail until the
//!     shard storage exists.
//!
//! AC → test map:
//! - AC1: [`ac1_init_sql_declares_the_per_shard_jsonb_table_idempotently`] (RED),
//!   [`ac1_init_sql_seeds_the_head_revision_row_idempotently`] (RED),
//!   [`ac1_the_sql_store_decomposes_and_reassembles_through_the_shard_projection`]
//!   (RED), [`ac1_the_store_addresses_the_shard_table_outside_its_own_ddl`] (RED);
//!   the legacy-migration behaviour is contract-tested in
//!   `sql_store_contract.rs::f300` (RED there)
//! - AC2: [`ac2_the_persist_path_keeps_its_validation_gate_and_cas_shape`]
//!   (green control — the shape that must not change); the round-trip /
//!   conflict / version equivalence is contract-tested in `sql_store_contract.rs`
//! - AC3: [`ac3_claim_ticket_takes_its_for_update_lock_on_the_tickets_shard_row_only`]
//!   (RED); the lock behaviour is contract-tested in `sql_store_contract.rs::f300`
//! - AC4: [`ac4_the_refusal_gate_still_precedes_every_write_in_the_persist_path`]
//!   (green control — ordering that must survive), plus the shard-row-refusal
//!   contract test in `sql_store_contract.rs::f300` (RED there)
//! - AC5: the placement criterion itself — the DB-backed tests live in
//!   `crates/infrastructure/tests/sql_store_contract.rs` (this file pins the
//!   scan half only)

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::shard_schema_f300 as schema;

/// The source with ALL whitespace removed and lower-cased, underscores kept —
/// guards match exact source tokens in any formatting (the f267 convention).
/// Lower-casing lets one needle match the SQL keyword style (`FOR UPDATE`) and
/// the Rust identifier style (`ShardKind::Work`) alike; identifiers keep their
/// underscores (`gate_save`, `project_state_shard`, `fromproject_statewhere`).
fn flat(src: &str) -> String {
    src.chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Successive windows of flattened source: from each occurrence of `needle`
/// to just before the next occurrence (or `window` chars later, whichever is
/// nearer) — proximity heuristics over flat text, not parses.
fn windows<'a>(src: &'a str, needle: &str, window: usize) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(at) = rest.find(needle) {
        let end = (at + needle.len() + window).min(rest.len());
        out.push(&rest[at..end]);
        rest = &rest[end..];
    }
    out
}

/// The flattened body of one fn, from its `fn <name>` to the next `async fn`
/// at the same impl-block level (or end of file).
fn fn_body<'a>(flat_src: &'a str, flat_fn_name: &str) -> &'a str {
    let start = flat_src
        .find(flat_fn_name)
        .unwrap_or_else(|| panic!("{flat_fn_name} must still exist in sql_store.rs"));
    let rest = &flat_src[start + flat_fn_name.len()..];
    let end = rest.find("asyncfn").map_or(rest.len(), |at| at);
    &rest[..end]
}

// --- AC1: the shard schema in INIT_SQL ---------------------------------------

/// AC1: `INIT_SQL` must declare the per-shard JSONB table — idempotent DDL
/// (`IF NOT EXISTS`), keyed per project and per bounded-context kind, with a
/// JSONB payload column.
#[test]
fn ac1_init_sql_declares_the_per_shard_jsonb_table_idempotently() {
    let ddl = flat(&schema::shard_ddl().to_ascii_lowercase());
    assert!(
        ddl.contains("createtableifnotexists"),
        "the shard table must be created idempotently (CREATE TABLE IF NOT EXISTS) — \
         every connect re-runs INIT_SQL (AC1):\n{ddl}"
    );
    assert!(
        ddl.contains("jsonb"),
        "the shard table must store per-shard JSONB payloads (AC1):\n{ddl}"
    );
    assert!(
        ddl.contains("project_id"),
        "the shard table must be scoped per project — many projects share one \
         database (AC1):\n{ddl}"
    );
}

/// AC1: `INIT_SQL` must create the head-revision row idempotently — a ROW is
/// data, so idempotent creation in INIT_SQL can only be a seed insert that
/// keeps the existing head on re-connect.
#[test]
fn ac1_init_sql_seeds_the_head_revision_row_idempotently() {
    let f = flat(&schema::init_sql_literal());
    assert!(
        f.contains("insertinto"),
        "INIT_SQL must seed the head-revision ROW (a row is data — plain DDL \
         cannot create it) (AC1)"
    );
    assert!(
        f.contains("onconflict") && f.contains("donothing"),
        "the head-revision seed must be idempotent (ON CONFLICT … DO NOTHING) — \
         a re-connect must never reset or duplicate the head revision (AC1)"
    );
}

/// AC1: the store's write/read paths must decompose and reassemble through
/// the C019a shard projection — `ProjectState::into_shards` /
/// `ProjectState::from_shards` are the only partition the codebase defines,
/// and migration "with data identical pre/post" is defined by them.
#[test]
fn ac1_the_sql_store_decomposes_and_reassembles_through_the_shard_projection() {
    let f = flat(&schema::sql_store_source());
    assert!(
        f.contains("into_shards"),
        "sql_store.rs must decompose the aggregate via into_shards — the shard \
         rows and the legacy row must partition identically or the migration \
         cannot guarantee identical data (AC1)"
    );
    assert!(
        f.contains("from_shards"),
        "sql_store.rs must reassemble the aggregate via from_shards — load over \
         shard rows must rebuild the exact ProjectState (AC1)"
    );
}

/// AC1: the shard table must be addressed by the store's CODE, not only
/// declared in its DDL — writes, loads and claims that never touch it are
/// whole-document storage with extra furniture.
#[test]
fn ac1_the_store_addresses_the_shard_table_outside_its_own_ddl() {
    let name = schema::shard_table_name();
    let code = flat(&schema::sql_store_code_outside_init_sql());
    assert!(
        code.contains(&name),
        "sql_store.rs must address the shard table (`{name}`) in its code — the \
         save/load/claim paths read and write per-shard rows (AC1)"
    );
}

// --- AC2: the persist path keeps its shape -----------------------------------

/// AC2 (green control): the validation gate and the CAS upsert keep their
/// place in `persist_at_revision` — whatever C019b changes about WHERE the
/// payload lands, `save`/`save_expecting` keep validating, CASing on the
/// caller's revision and refusing on a stale one, exactly as
/// `sql_store_contract.rs` asserts today.
#[test]
fn ac2_the_persist_path_keeps_its_validation_gate_and_cas_shape() {
    let f = flat(&schema::sql_store_source());
    let body = fn_body(&f, "fnpersist_at_revision");
    assert!(
        body.contains("gate_save("),
        "persist_at_revision must keep the F229 structural-integrity gate on the \
         shard write path too (AC2: behaviour identical to today / AC4)"
    );
    assert!(
        body.contains("conflict(") || body.contains("porterror::conflict"),
        "persist_at_revision must keep returning PortError::Conflict on a stale \
         revision (AC2)"
    );
    if let Some(first_write) = body.find(".execute(") {
        let gate = body.find("gate_save(").expect("checked above");
        assert!(
            gate < first_write,
            "validation must precede the first row write in the persist path — \
             a refused payload reaches no row, legacy or shard (AC2/AC4)"
        );
    }
}

// --- AC3: the shard-scoped claim lock ----------------------------------------

/// AC3: `claim_ticket` performs its `SELECT FOR UPDATE` ONLY on the Tickets
/// (`ShardKind::Work`) shard row — never again the whole-aggregate legacy
/// row, and never every shard row of the project.
#[test]
fn ac3_claim_ticket_takes_its_for_update_lock_on_the_tickets_shard_row_only() {
    let f = flat(&schema::sql_store_source());
    let body = fn_body(&f, "asyncfnclaim_ticket");
    assert!(
        body.contains("forupdate"),
        "claim_ticket must stay a locked, transactional critical section (AC3)"
    );
    assert!(
        !body.contains("fromproject_statewhere"),
        "claim_ticket must stop taking the WHOLE-aggregate row lock — it today \
         runs `SELECT … FROM project_state WHERE … FOR UPDATE`, which serializes \
         every claim behind every other shard write (AC3)"
    );
    let shard_table = schema::shard_table_name();
    assert!(
        windows(body, "forupdate", 300)
            .iter()
            .any(|w| w.contains(&shard_table)),
        "claim_ticket's FOR UPDATE must target the shard table (`{shard_table}`) \
         — the lock is the Tickets shard row, not the legacy document (AC3)"
    );
    assert!(
        body.contains("shardkind::work") || body.contains("'work'"),
        "the FOR UPDATE must be scoped to the Work (Tickets) shard row — locking \
         every shard row of the project is a whole-row lock by another name (AC3)"
    );
}

// --- AC4: the refusal gate precedes any shard row ----------------------------

/// AC4 (green control): the F229 refusal path still returns Err from the gate
/// — the DB-backed proof that no shard row is written on a refusal lives in
/// `sql_store_contract.rs::f300`. This guard pins the ORDER in the source so
/// a shard write cannot be hoisted above the gate.
#[test]
fn ac4_the_refusal_gate_still_precedes_every_write_in_the_persist_path() {
    let f = flat(&schema::sql_store_source());
    let body = fn_body(&f, "fnpersist_at_revision");
    let gate = body
        .find("gate_save(")
        .expect("persist path must still run the F229 gate (AC4)");
    let refusal = body
        .find("returnerr(refused.error)")
        .expect("the gate refusal must still return Err from persist_at_revision (AC4)");
    assert!(
        gate < refusal,
        "the gate must run before the refusal returns (AC4)"
    );
    if let Some(first_write) = body.find(".execute(") {
        assert!(
            refusal < first_write,
            "the refusal must return Err BEFORE the first row write — no legacy or \
             shard row may observe a refused payload (AC4)"
        );
    }
}

// --- scanner bite controls ----------------------------------------------------

/// The scanners stay sharp: each must fire on a source shape missing its
/// required token and stay quiet on a shape carrying it — so a guard can
/// never rot into a vacuous pass (the f267 convention).
#[test]
fn the_shard_schema_and_claim_scanners_bite_on_the_missing_shapes() {
    // The claim scanner bites on today's whole-row lock shape…
    let today_shape = flat(
        "let Some(row) = tx.query_opt(
             \"SELECT data FROM project_state WHERE project_id = $1 FOR UPDATE\",
             &[&self.project_id])? else { return Ok(false) };",
    );
    assert!(today_shape.contains("forupdate"));
    assert!(
        today_shape.contains("fromproject_statewhere"),
        "the legacy whole-row shape must trip the AC3 negative guard"
    );
    // …and stay quiet on the shard-scoped shape the AC demands.
    let shard_shape = flat(
        "let Some(row) = tx.query_opt(
             \"SELECT data FROM project_state_shard
                WHERE project_id = $1 AND shard = $2 FOR UPDATE\",
             &[&self.project_id, &ShardKind::Work])? else { return Ok(false) };",
    );
    assert!(!shard_shape.contains("fromproject_statewhere"));
    assert!(shard_shape.contains("shardkind::work"));

    // The persist-order scanner: gate before write passes, write before gate bites.
    let good = flat(
        "if let Err(refused) = gate_save(&mut state, &self.quarantine) {
             return Err(refused.error);
         }
         let rows = client.execute(GUARDED_SAVE_SQL, &[…]).await?;",
    );
    let gate = good.find("gate_save(").expect("gate present");
    assert!(gate < good.find(".execute(").expect("write present"));
    let bad = flat(
        "let rows = client.execute(SHARD_SAVE_SQL, &[…]).await?;
         if let Err(refused) = gate_save(&mut state, &self.quarantine) {
             return Err(refused.error);
         }",
    );
    let gate_bad = bad.find("gate_save(").expect("gate present");
    let write_bad = bad.find(".execute(").expect("write present");
    assert!(
        gate_bad > write_bad,
        "the order scanner must bite when a write is hoisted above the gate"
    );
}
