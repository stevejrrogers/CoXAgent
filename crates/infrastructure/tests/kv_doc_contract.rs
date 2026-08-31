//! `PgKvDoc` CAS contract (CXA-C017): the `KvDocPort` trait defaults keep a
//! backend without revision tracking on plain save/load behaviour, and the
//! Postgres adapter rejects a stale guarded write with [`PortError::Conflict`]
//! while the revision stays monotonic across guarded AND legacy writes — the
//! `sql_store_contract.rs` CXA-F003 suite, mirrored for hub-wide KV docs.
//!
//! The Postgres halves are skipped unless `COXAGENT_TEST_PG_DSN` is set (no
//! database in ordinary CI), so they are no-ops by default and a full
//! integration check when a DSN is provided.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use coxagent_application::ports::outbound::KvDocPort;
use coxagent_application::PortError;
use coxagent_infrastructure::PgKvDoc;
use std::collections::HashMap;
use std::sync::Mutex;

/// A minimal fake implementing only `load`/`save` — every such backend must
/// keep compiling and behaving unchanged when the trait grows its guarded
/// methods, because they carry defaults.
#[derive(Default)]
struct FakeKv {
    docs: Mutex<HashMap<String, String>>,
}

#[async_trait]
impl KvDocPort for FakeKv {
    async fn load(&self, key: &str) -> Result<Option<String>, PortError> {
        Ok(self.docs.lock().unwrap().get(key).cloned())
    }

    async fn save(&self, key: &str, json: &str) -> Result<(), PortError> {
        self.docs
            .lock()
            .unwrap()
            .insert(key.to_owned(), json.to_owned());
        Ok(())
    }
}

/// The trait defaults: `save_expecting` falls through to `save`,
/// `current_version` reports "no guard available". Pure — no server, no port.
#[tokio::test]
async fn a_backend_without_revisions_keeps_legacy_save_load_behaviour() {
    let kv = FakeKv::default();
    kv.save_expecting("k", r#"{"v":1}"#, Some(0))
        .await
        .expect("default save_expecting falls through to save");
    assert_eq!(
        kv.load("k").await.expect("load").as_deref(),
        Some(r#"{"v":1}"#),
        "the fall-through must have persisted via save"
    );
    assert_eq!(
        kv.current_version("k").await.expect("version"),
        None,
        "a backend without revision tracking reports no guard"
    );
}

/// Doc content is compared structurally (`serde_json::Value`), not byte-wise:
/// Postgres stores JSONB and normalizes key order and whitespace on the way
/// in, so `load` legitimately returns a re-serialized document.
fn same_json(a: &str, b: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(a).ok()
        == serde_json::from_str::<serde_json::Value>(b).ok()
}

/// The gated tests connect in parallel; on a FRESH database their concurrent
/// `CREATE TABLE IF NOT EXISTS` races the catalog and one loses with
/// "migrate: db error". Run the first migration once, alone.
static MIGRATED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

async fn connect_store(dsn: &str) -> PgKvDoc {
    MIGRATED
        .get_or_init(|| async {
            PgKvDoc::connect(dsn).await.expect("initial migrate");
        })
        .await;
    PgKvDoc::connect(dsn).await.expect("connect + migrate")
}

/// CXA-C017: a stale guarded write is rejected with [`PortError::Conflict`]
/// and persists nothing, and a retry at the current revision converges —
/// exactly how an optimistic retry recovers after seeing a conflict.
#[tokio::test]
async fn kv_doc_rejects_stale_revision_write_with_conflict() {
    let Ok(dsn) = std::env::var("COXAGENT_TEST_PG_DSN") else {
        eprintln!("COXAGENT_TEST_PG_DSN unset — skipping Postgres KV contract test");
        return;
    };
    // Unique key per run so repeated runs against the same DB are clean.
    let key = format!("kv-contract-test-{}", std::process::id());
    let store = connect_store(&dsn).await;

    // 1. An absent key exposes baseline revision 0 — its first write lands at 1.
    assert_eq!(
        store.current_version(&key).await.expect("absent version"),
        Some(0),
        "an absent key exposes baseline revision 0"
    );

    // 2. First writer captured 0 and guards on it -> lands at revision 1.
    let first = r#"{"kind":"kv-contract","writer":"a","n":1}"#;
    store
        .save_expecting(&key, first, Some(0))
        .await
        .expect("guarded save with expected 0 succeeds");
    assert_eq!(
        store.current_version(&key).await.expect("after first"),
        Some(1)
    );
    let loaded = store.load(&key).await.expect("reload").unwrap_or_default();
    assert!(
        same_json(&loaded, first),
        "round-trip must preserve the document: {loaded}"
    );

    // 3. A second writer captured rev 1 too and races ahead, bumping to 2.
    let theirs = r#"{"kind":"kv-contract","writer":"b","n":2}"#;
    store
        .save_expecting(&key, theirs, Some(1))
        .await
        .expect("concurrent writer saves against its own held rev");
    assert_eq!(
        store.current_version(&key).await.expect("after concurrent"),
        Some(2)
    );

    // 4. Writer A still holds rev 1 but the row is now at 2. The stale guard
    //    must conflict and change nothing — their winning doc survives.
    let stale = r#"{"kind":"kv-contract","writer":"a","n":3}"#;
    match store.save_expecting(&key, stale, Some(1)).await {
        Err(PortError::Conflict(_)) => {}
        other => panic!("expected Conflict for stale rev-1 write over rev-2 row — got {other:?}"),
    }
    let loaded = store
        .load(&key)
        .await
        .expect("reload after rejected write")
        .unwrap_or_default();
    assert!(
        same_json(&loaded, theirs),
        "the rejected write must have persisted nothing — got {loaded}"
    );
    assert_eq!(
        store
            .current_version(&key)
            .await
            .expect("version after rejected write"),
        Some(2),
        "the rejected write must not have moved the revision"
    );

    // 5. Retrying with the CURRENT revision (reload-then-save) converges.
    store
        .save_expecting(&key, stale, Some(2))
        .await
        .expect("retry with current revision succeeds");
    assert_eq!(
        store.current_version(&key).await.expect("after retry"),
        Some(3)
    );
}

/// CXA-C017 monotonicity regression: a legacy `save()` between two guarded
/// writes must ALSO advance the revision — a blind write landing under a
/// concurrent guard invalidates that guard, it never slips beneath it.
#[tokio::test]
async fn kv_doc_legacy_save_keeps_the_revision_monotonic_under_a_guard() {
    let Ok(dsn) = std::env::var("COXAGENT_TEST_PG_DSN") else {
        eprintln!("COXAGENT_TEST_PG_DSN unset — skipping Postgres KV contract test");
        return;
    };
    let key = format!("kv-monotonic-test-{}", std::process::id());
    let store = connect_store(&dsn).await;

    // Absent key -> baseline 0; the first guarded write lands at 1.
    assert_eq!(
        store.current_version(&key).await.expect("absent version"),
        Some(0)
    );
    let first = r#"{"kind":"kv-contract","writer":"a","n":1}"#;
    store
        .save_expecting(&key, first, Some(0))
        .await
        .expect("guarded save with expected 0 succeeds");
    assert_eq!(
        store.current_version(&key).await.expect("after first"),
        Some(1)
    );

    // A blind legacy write lands while writer A holds rev 1 — it must bump
    // the revision to 2, not sneak in at the same revision.
    let blind = r#"{"kind":"kv-contract","writer":"b","n":2}"#;
    store.save(&key, blind).await.expect("legacy save");
    assert_eq!(
        store
            .current_version(&key)
            .await
            .expect("after legacy save"),
        Some(2),
        "a legacy save must bump the revision too, or it defeats the CAS"
    );

    // Writer A's guard on 1 must now conflict instead of clobbering the
    // blind write, and the blind write's content must survive untouched.
    let stale = r#"{"kind":"kv-contract","writer":"a","n":3}"#;
    match store.save_expecting(&key, stale, Some(1)).await {
        Err(PortError::Conflict(_)) => {}
        other => {
            panic!("expected Conflict for a guarded write under a legacy save — got {other:?}")
        }
    }
    let loaded = store
        .load(&key)
        .await
        .expect("reload after legacy-conflict")
        .unwrap_or_default();
    assert!(
        same_json(&loaded, blind),
        "the blind legacy write must survive the conflicting guard — got {loaded}"
    );

    // And the guarded retry at the current revision converges as always.
    store
        .save_expecting(&key, stale, Some(2))
        .await
        .expect("retry after legacy save converges");

    // Isolation: revisions are scoped PER KEY — a busy key never lends its
    // revision to a sibling, which still reads as never-written.
    let other = format!("{key}-other");
    assert_eq!(
        store.current_version(&other).await.expect("other version"),
        Some(0),
        "a sibling key must read as never-written"
    );
    assert_eq!(
        store.load(&other).await.expect("other load"),
        None,
        "a sibling key must have no document"
    );
}
