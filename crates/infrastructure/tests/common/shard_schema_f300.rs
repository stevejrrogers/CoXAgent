//! CXA-F300 (CXA-C019b) — discovery of the shard schema the SQL store must
//! declare. Shared by the `sql_store_sharded_rows_f300_tdd.rs` source guards
//! and the `sql_store_contract.rs` shard contract tests.
//!
//! The acceptance criteria name the artefacts but deliberately not the
//! identifiers ("New shard table", "head-revision row", "the Tickets shard
//! row"). Per the no-invented-identifiers rule these helpers do NOT coin a
//! schema: they parse the implementation's own `INIT_SQL`
//! (`crates/infrastructure/src/state/sql_store.rs`) and hand back the table
//! name, its shard-kind column and the DDL itself. Until C019b lands the
//! parsers fail RED naming the AC that is unmet — which is exactly the
//! failing state the TDD half of the ticket requires.
//!
//! Everything here is a pure function over repo source (the
//! `fail_closed_test_db_f327_tdd.rs` scan discipline): no database, no
//! network, no host harness.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_application::state::ShardKind;

/// The adapter file whose `INIT_SQL` must declare the shard schema (AC1).
pub const SQL_STORE: &str = "crates/infrastructure/src/state/sql_store.rs";

/// The adapter source, for the scan guards.
pub fn sql_store_source() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(SQL_STORE);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The source with the `INIT_SQL` literal removed — everything the adapter
/// EXECUTES in code rather than declares as schema.
pub fn sql_store_code_outside_init_sql() -> String {
    let src = sql_store_source();
    let literal = init_sql_literal_from(&src).expect("INIT_SQL literal is present today");
    src.replacen(&literal, "", 1)
}

/// Extract the `INIT_SQL` string literal from the adapter source.
fn init_sql_literal_from(src: &str) -> Option<String> {
    let start_marker = "const INIT_SQL: &str = \"";
    let start = src.find(start_marker)? + start_marker.len();
    let end = src[start..].find("\";")? + start;
    Some(src[start..end].to_owned())
}

/// The `INIT_SQL` literal as written in the adapter source.
pub fn init_sql_literal() -> String {
    init_sql_literal_from(&sql_store_source())
        .expect("sql_store.rs must still declare `const INIT_SQL` — AC1 pins the schema there")
}

/// The `(table name, DDL)` of the shard table declared in an INIT_SQL-shaped
/// literal, or `None` when no shard table is declared — the pure parser the
/// guards and the bite controls exercise.
pub fn shard_ddl_from(init_sql: &str) -> Option<(String, String)> {
    for line in init_sql.lines() {
        let trimmed = line.trim();
        if !trimmed
            .to_ascii_lowercase()
            .starts_with("create table if not exists")
        {
            continue;
        }
        let lowered = trimmed.to_ascii_lowercase();
        let after = lowered
            .strip_prefix("create table if not exists")
            .expect("checked prefix")
            .trim_start();
        let name: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.to_ascii_lowercase().contains("shard") {
            continue;
        }
        // Capture the statement through its closing `);` line.
        let mut ddl = String::new();
        let mut started = false;
        for l in init_sql.lines() {
            if l.trim() == trimmed {
                started = true;
            }
            if started {
                ddl.push_str(l);
                ddl.push('\n');
                if l.trim().ends_with(");") {
                    break;
                }
            }
        }
        return Some((name, ddl));
    }
    None
}

/// The shard table's DDL — RED (naming AC1) until C019b declares it.
pub fn shard_ddl() -> String {
    shard_ddl_from(&init_sql_literal()).map_or_else(
        || {
            panic!(
                "CXA-F300 AC1 RED: {SQL_STORE} INIT_SQL declares no shard table yet — the \
                 per-shard JSONB storage (and every physical assertion built on it) cannot \
                 run until C019b lands"
            )
        },
        |(_, ddl)| ddl,
    )
}

/// The shard table's name, parsed from the implementation's own INIT_SQL.
pub fn shard_table_name() -> String {
    shard_ddl_from(&init_sql_literal()).map_or_else(
        || {
            panic!(
                "CXA-F300 AC1 RED: {SQL_STORE} INIT_SQL declares no shard table yet — \
                 the physical shard-row assertions cannot run until C019b lands"
            )
        },
        |(name, _)| name,
    )
}

/// The shard-kind column of the shard table — the column that keys one row
/// per [`ShardKind`]. Parsed from the implementation's own DDL: the first
/// column named like a kind discriminator (`shard`, `kind`, …), skipping
/// constraint lines.
pub fn shard_kind_column_from(ddl: &str) -> Option<String> {
    let mut inside_body = false;
    for line in ddl.lines() {
        let trimmed = line.trim();
        if trimmed.to_ascii_lowercase().starts_with("create table") {
            inside_body = true;
            continue;
        }
        if !inside_body || trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with(')') {
            break;
        }
        let head: String = trimmed
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let constraint = head.to_ascii_uppercase();
        if matches!(
            constraint.as_str(),
            "PRIMARY" | "UNIQUE" | "FOREIGN" | "CHECK" | "CONSTRAINT"
        ) {
            continue;
        }
        let lower = head.to_ascii_lowercase();
        if lower == "shard"
            || lower == "kind"
            || lower.ends_with("_kind")
            || lower.ends_with("_shard")
        {
            return Some(head);
        }
    }
    None
}

/// The shard-kind column name — RED (naming AC1) until the DDL keys rows per
/// shard kind.
pub fn shard_kind_column() -> String {
    let ddl = shard_ddl();
    shard_kind_column_from(&ddl).unwrap_or_else(|| {
        panic!(
            "CXA-F300 AC1 RED: the shard table DDL has no per-shard key column — \
             per-shard JSONB storage must key one row per bounded-context kind:\n{ddl}"
        )
    })
}

/// The stored label of a shard kind — the codebase's own vocabulary
/// (`ShardKind` serializes `snake_case`), never an invented literal.
pub fn shard_label(kind: ShardKind) -> String {
    let json = serde_json::to_string(&kind).expect("ShardKind serializes");
    json.trim_matches('"').to_owned()
}

// --- parser bite controls (run in every suite that mounts `common`) ---------

#[test]
fn the_shard_ddl_parser_finds_a_shard_shaped_schema() {
    let init_sql = "
CREATE TABLE IF NOT EXISTS project_state (
    project_id TEXT PRIMARY KEY,
    data JSONB NOT NULL
);
CREATE TABLE IF NOT EXISTS project_state_shard (
    project_id TEXT NOT NULL,
    shard TEXT NOT NULL,
    revision BIGINT NOT NULL DEFAULT 0,
    data JSONB NOT NULL,
    PRIMARY KEY (project_id, shard)
);
INSERT INTO project_head (project_id, revision) VALUES ('', 0) ON CONFLICT DO NOTHING;";
    let (name, ddl) = shard_ddl_from(init_sql).expect("the shard table is discovered");
    assert_eq!(name, "project_state_shard");
    assert!(ddl.contains("JSONB"));
    assert_eq!(
        shard_kind_column_from(&ddl).as_deref(),
        Some("shard"),
        "the kind discriminator column is found among the columns"
    );
}

#[test]
fn the_shard_ddl_parser_bites_on_a_legacy_schema_without_shards() {
    let legacy = "
CREATE TABLE IF NOT EXISTS project_state (
    project_id TEXT PRIMARY KEY,
    data JSONB NOT NULL
);";
    assert!(
        shard_ddl_from(legacy).is_none(),
        "a whole-document schema must NOT satisfy the shard-table discovery"
    );
    assert!(
        shard_kind_column_from(legacy).is_none(),
        "no kind column may be invented for a schema that has none"
    );
}

#[test]
fn the_kind_column_parser_ignores_constraint_lines_and_finds_kind_named_columns() {
    let ddl = "
CREATE TABLE IF NOT EXISTS state_shards (
    project_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    data JSONB NOT NULL,
    PRIMARY KEY (project_id, kind)
);";
    assert_eq!(shard_kind_column_from(ddl).as_deref(), Some("kind"));
}

#[test]
fn shard_labels_are_the_codebases_own_serde_vocabulary() {
    use coxagent_application::state::ShardKind;
    assert_eq!(shard_label(ShardKind::Work), "work");
    assert_eq!(shard_label(ShardKind::Social), "social");
    for kind in ShardKind::ALL {
        let back: ShardKind =
            serde_json::from_str(&format!("\"{}\"", shard_label(kind))).expect("round trip");
        assert_eq!(back, kind);
    }
}
