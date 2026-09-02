//! The ONE door to a test database for the DSN-gated integration tests
//! (CXA-F326): a fail-closed guard that refuses anything but an ephemeral
//! `cxa_test*` Postgres — plus, where required, an explicit Redis URL.
//!
//! The hole this closes: the old handling was fail-open. Tests silently
//! skipped (printed to stderr, reported `ok`) when `COXAGENT_TEST_PG_DSN`
//! was unset, so CI green never meant the contract tests ran; the live-hub
//! refusal was copy-pasted per family and `kv_doc_contract.rs` had none at
//! all; and a probe error answered "not live", letting the destructive
//! suites run against an unverified — potentially live hub — database.
//!
//! Failure mode is ALWAYS panic-with-remedy, never a silent skip. The only
//! sanctioned skip is the `#[ignore]` attribute on the gated tests themselves,
//! which ordinary CI (no database) reports explicitly.
//!
//! Shape (house hexagonal pattern, test scale): the decision is a pure
//! function over an explicit snapshot — env presence, DSN shape, probe
//! verdict — and the only IO (reading the env, connecting to probe the
//! target) lives in the thin adapters below. The refusal table is unit-tested
//! right here, DB-free, under every including test binary.

#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_application::ports::outbound::StateStorePort;
use coxagent_infrastructure::SqlStateStore;

/// The env var naming the (only) door to a Postgres test database.
pub const PG_VAR: &str = "COXAGENT_TEST_PG_DSN";

/// The env var naming the Redis the coordination tests drive their leases
/// through. Required only by [`pg_and_redis`].
pub const REDIS_VAR: &str = "COXAGENT_TEST_REDIS_URL";

/// Ephemeral test databases are named `cxa_test*` — anything else (a real
/// hub store, the standing `cox-infra` database) is refused.
const DB_PREFIX: &str = "cxa_test";

/// What the probe adapter saw when it inspected the target database.
#[derive(Debug)]
pub enum ProbeVerdict {
    /// A dedicated test database: no hub row at all, or revision 0.
    Fresh,
    /// The target holds a real hub project (`cxa` row with revision > 0).
    LiveHub,
    /// The probe could not decide — connection or query failure. Never
    /// treated as "not live": that fail-open default caused the incident.
    Unverifiable(String),
}

/// A verified, ephemeral Postgres target plus this run's fixture namespace.
///
/// Every fixture id MUST be minted through [`TestPg::project`] or
/// [`TestPg::kv_key`] so rows are namespaced `cxa-test-<pid>-<nanos>` and can
/// neither collide with another run's data nor be mistaken for a real hub's.
///
/// # Teardown expectations
/// - State-store fixtures: end the test with `store.delete()` — the
///   `SqlStateStore` sweep purges the aggregate, the coordination rows and
///   the project's Redis keyspace.
/// - Auth fixtures: end the test with `svc.delete_user(...)`.
/// - KV-doc fixtures have no port-level delete; their namespaced keys are
///   reclaimed when the ephemeral database is torn down. Never run these
///   tests against a database that outlives the test session.
pub struct TestPg {
    /// The verified DSN — pass it to every store/auth adapter under test.
    pub dsn: String,
    /// Namespace prefix (`cxa-test-<pid>-<nanos>`) unique to this `pg()` call.
    pub namespace: String,
}

impl TestPg {
    /// A project/store id namespaced to this run: `{namespace}-{suffix}`.
    pub fn project(&self, suffix: &str) -> String {
        format!("{}-{suffix}", self.namespace)
    }

    /// A KV-doc key namespaced to this run: `{namespace}-{key}`.
    pub fn kv_key(&self, key: &str) -> String {
        format!("{}-{key}", self.namespace)
    }
}

/// Verify the test environment for a Postgres-only integration test and mint
/// its fixture namespace. The single entry point for every DSN-gated test
/// that needs Postgres but not Redis.
///
/// # Panics
/// Fail-closed, with the remedy in the message, when `COXAGENT_TEST_PG_DSN`
/// is unset/empty, is not a `postgres://` URL, does not point at a
/// `cxa_test*` database, holds a live hub project, or cannot be probed.
pub async fn pg(caller: &'static str) -> TestPg {
    verified_env(caller, false).await.0
}

/// [`pg`] plus the explicit `COXAGENT_TEST_REDIS_URL` the coordination tests
/// need for their leader/stage leases.
///
/// # Panics
/// As [`pg`], and additionally when `COXAGENT_TEST_REDIS_URL` is unset/empty.
pub async fn pg_and_redis(caller: &'static str) -> (TestPg, String) {
    verified_env(caller, true).await
}

/// The one composition: read the env (IO) → verify shape (pure) → verify
/// Redis presence (pure) → prepare the schema and probe the target (IO) →
/// verify the verdict (pure) → mint the namespace. Every refusal panics.
async fn verified_env(caller: &'static str, require_redis: bool) -> (TestPg, String) {
    let pg_dsn = std::env::var(PG_VAR).ok();
    // `Option<Option<_>>`: the outer layer is "this caller needs Redis at
    // all", the inner is "was the variable actually set" — a Postgres-only
    // test never even reads the Redis variable.
    let redis_url = require_redis.then(|| std::env::var(REDIS_VAR).ok());

    let dsn = verify_dsn(caller, pg_dsn.as_deref()).unwrap_or_else(|refusal| panic!("{refusal}"));
    let redis = match redis_url {
        Some(opt) => {
            verify_redis(caller, opt.as_deref()).unwrap_or_else(|refusal| panic!("{refusal}"))
        }
        None => String::new(),
    };

    let verdict = match ensure_schema(&dsn).await {
        Ok(()) => probe_live_hub(&dsn).await,
        Err(why) => ProbeVerdict::Unverifiable(why),
    };
    verify_probe(caller, &verdict).unwrap_or_else(|refusal| panic!("{refusal}"));

    (
        TestPg {
            dsn,
            namespace: mint_namespace(),
        },
        redis,
    )
}

/// Presence + shape half of the refusal table (pure): unset/empty env, a
/// non-postgres URL, or a database name off `cxa_test*` is a refusal.
fn verify_dsn(caller: &str, pg_dsn: Option<&str>) -> Result<String, String> {
    let Some(dsn) = pg_dsn.filter(|v| !v.trim().is_empty()) else {
        return Err(format!(
            "{PG_VAR} is unset — refusing to run `{caller}`. These integration tests \
             create and delete project rows, so they never guess a target. Point \
             {PG_VAR} at an ephemeral Postgres whose database name starts with \
             `{DB_PREFIX}` (README → Integration test environment has a one-liner). \
             In ordinary CI the tests are #[ignore]d (skipped) — that is the only \
             sanctioned skip; running them demands an explicit, verified target."
        ));
    };
    if !(dsn.starts_with("postgres://") || dsn.starts_with("postgresql://")) {
        return Err(format!(
            "{PG_VAR} must be a postgres:// URL — refusing `{caller}`. See README → \
             Integration test environment."
        ));
    }
    let name = parse_db_name(dsn);
    let shaped = name.as_deref().is_some_and(|n| n.starts_with(DB_PREFIX));
    if !shaped {
        let got = name.unwrap_or_else(|| {
            "a DSN with no database segment — cannot tell what it points at".to_owned()
        });
        return Err(format!(
            "{PG_VAR} must point at an ephemeral test database named `{DB_PREFIX}*` — \
             refusing `{caller}`. Got {got}. A real hub database would be destroyed \
             by these fixtures. See README → Integration test environment."
        ));
    }
    Ok(dsn.to_owned())
}

/// Redis-presence half of the refusal table (pure): the coordination tests
/// never inherit a default URL.
fn verify_redis(caller: &str, redis_url: Option<&str>) -> Result<String, String> {
    redis_url
        .filter(|v| !v.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            format!(
                "{REDIS_VAR} is unset — refusing to run `{caller}`. The coordination \
                 tests drive leader/stage leases through Redis and never inherit a \
                 default URL: set {REDIS_VAR} (e.g. redis://localhost:56379) \
                 alongside {PG_VAR}."
            )
        })
}

/// Probe-verdict half of the refusal table (pure): only a verified-fresh
/// `cxa_test*` target lets the destructive suites through.
fn verify_probe(caller: &str, verdict: &ProbeVerdict) -> Result<(), String> {
    match verdict {
        ProbeVerdict::Fresh => Ok(()),
        ProbeVerdict::LiveHub => Err(format!(
            "{PG_VAR} points at a LIVE hub database — refusing `{caller}`. The target \
             holds a real hub project (a `cxa` row with revision > 0) and these tests \
             would fill it with fixture rows. Point {PG_VAR} at an ephemeral \
             `{DB_PREFIX}*` database instead."
        )),
        ProbeVerdict::Unverifiable(why) => Err(format!(
            "cannot verify the target is not a live hub — refusing `{caller}` \
             fail-closed. Verification of {PG_VAR} failed: {why}. The old guard \
             answered \"not live\" on probe errors, and that fail-open default is \
             exactly how the live-hub incident happened. Fix the connection or point \
             {PG_VAR} at an ephemeral `{DB_PREFIX}*` database."
        )),
    }
}

/// The probe adapter (IO): inspect the target through the same store adapter
/// the destructive tests use. A dedicated test database has no `cxa` row; a
/// live hub's `cxa` row always carries revision > 0. This closed a real
/// incident: an exported `COXAGENT_TEST_PG_DSN` pointing at the production
/// store was filled with `test-<pid>` project rows.
pub async fn probe_live_hub(dsn: &str) -> ProbeVerdict {
    let Ok(probe) = SqlStateStore::connect(dsn, "cxa").await else {
        return ProbeVerdict::Unverifiable("connection failed".to_owned());
    };
    match probe.current_version().await {
        Ok(Some(v)) if v > 0 => ProbeVerdict::LiveHub,
        Ok(_) => ProbeVerdict::Fresh,
        Err(e) => ProbeVerdict::Unverifiable(format!("revision probe failed: {e}")),
    }
}

/// Parse the database name out of a libpq URL (`postgres://user:pass@host:
/// port/db?params`). `None` when the URL carries no database segment — the
/// caller refuses rather than guessing (libpq would default to the user name).
fn parse_db_name(dsn: &str) -> Option<String> {
    let rest = dsn.split_once("://")?.1;
    let path = rest.split_once('/')?.1;
    let name = path.split_once('?').map_or(path, |(name, _)| name);
    Some(name.to_owned())
}

/// Mint the per-run fixture namespace: `cxa-test-<pid>-<nanos>`.
fn mint_namespace() -> String {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("cxa-test-{pid}-{nanos}")
}

/// Run the FIRST schema migration alone. The gated tests connect in
/// parallel; on a fresh database their concurrent `CREATE TABLE IF NOT
/// EXISTS` races the catalog and one loses with "migrate: db error" — the
/// probe is one of those connectors, and losing would flip it to
/// Unverifiable. Serializing the first connect through a process-wide cell
/// removes the race for the guard and every gated test behind it. A failure
/// here is a verification failure like any other: the caller refuses with
/// the same cannot-verify wording (a target whose schema cannot even be
/// prepared is precisely a target that cannot be verified).
async fn ensure_schema(dsn: &str) -> Result<(), String> {
    static SCHEMA: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    SCHEMA
        .get_or_try_init(|| async {
            SqlStateStore::connect(dsn, "schema-init")
                .await
                .map(|_| ())
                .map_err(|e| format!("schema prepare failed: {e}"))
        })
        .await
        .copied()
}

/// The refusal table, unit-tested DB-free under every including test binary:
/// each row pins one panic-with-remedy the destructive suites must never be
/// allowed to bypass.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_env_refusal_names_the_var_the_remedy_and_the_sanctioned_skip() {
        let refusal = verify_dsn("unit", None).unwrap_err();
        assert!(refusal.contains(PG_VAR), "unhelpful: {refusal}");
        assert!(refusal.contains("cxa_test"), "unhelpful: {refusal}");
        assert!(refusal.contains("skipped"), "unhelpful: {refusal}");
    }

    #[test]
    fn an_empty_env_is_refused_like_an_unset_one() {
        let refusal = verify_dsn("unit", Some("   ")).unwrap_err();
        assert!(refusal.contains(PG_VAR), "unhelpful: {refusal}");
    }

    #[test]
    fn a_non_postgres_url_is_refused() {
        let refusal =
            verify_dsn("unit", Some("mysql://cox:test@localhost:5432/cxa_test")).unwrap_err();
        assert!(refusal.contains("postgres://"), "unhelpful: {refusal}");
    }

    #[test]
    fn a_database_off_the_cxa_test_shape_is_refused() {
        for dsn in [
            "postgres://cox:test@localhost:5432/coxagent",
            "postgres://cox:test@localhost:5432/",
            "postgres://cox:test@localhost:5432",
        ] {
            let refusal = verify_dsn("unit", Some(dsn)).unwrap_err();
            assert!(refusal.contains("cxa_test"), "{dsn}: {refusal}");
        }
    }

    #[test]
    fn a_fresh_cxa_test_database_passes_the_shape_check() {
        let dsn = "postgres://cox:test@localhost:5432/cxa_test?sslmode=disable";
        assert_eq!(verify_dsn("unit", Some(dsn)).unwrap(), dsn);
    }

    #[test]
    fn a_live_hub_probe_refuses() {
        let refusal = verify_probe("unit", &ProbeVerdict::LiveHub).unwrap_err();
        assert!(refusal.contains("LIVE hub"), "unhelpful: {refusal}");
        assert!(refusal.contains("refus"), "unhelpful: {refusal}");
        assert!(refusal.contains(PG_VAR), "unhelpful: {refusal}");
    }

    #[test]
    fn an_unverifiable_probe_fails_closed_with_the_exact_wording() {
        let refusal = verify_probe(
            "unit",
            &ProbeVerdict::Unverifiable("connection refused".to_owned()),
        )
        .unwrap_err();
        assert!(
            refusal.contains("cannot verify the target is not a live hub"),
            "unhelpful: {refusal}"
        );
    }

    #[test]
    fn a_fresh_probe_verdict_passes() {
        verify_probe("unit", &ProbeVerdict::Fresh)
            .unwrap_or_else(|refusal| panic!("fresh must pass: {refusal}"));
    }

    #[test]
    fn an_unset_redis_refusal_names_the_var() {
        let refusal = verify_redis("unit", None).unwrap_err();
        assert!(refusal.contains(REDIS_VAR), "unhelpful: {refusal}");
        let empty = verify_redis("unit", Some("")).unwrap_err();
        assert!(empty.contains(REDIS_VAR), "unhelpful: {empty}");
    }

    #[test]
    fn an_unreachable_probe_is_unverifiable_never_fresh() {
        // Nothing listens on port 9 of the loopback interface: the guard
        // cannot verify what this target is, which is exactly the case it
        // must refuse (the old guard answered "not live" here — fail-open).
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let verdict = probe_live_hub("postgres://cox:test@127.0.0.1:9/cxa_test_unit").await;
                assert!(
                    matches!(verdict, ProbeVerdict::Unverifiable(_)),
                    "an unreachable target must be Unverifiable, got {verdict:?}"
                );
            });
    }

    #[test]
    fn db_name_parsing_covers_the_urls_the_suite_uses() {
        assert_eq!(
            parse_db_name("postgres://cox:test@localhost:5432/cxa_test?sslmode=disable").as_deref(),
            Some("cxa_test")
        );
        assert_eq!(
            parse_db_name("postgresql:///cxa_test_2").as_deref(),
            Some("cxa_test_2")
        );
        assert_eq!(
            parse_db_name("postgres://cox:test@localhost:5432/").as_deref(),
            Some("")
        );
        assert_eq!(parse_db_name("postgres://cox:test@localhost:5432"), None);
    }

    #[test]
    fn minted_namespaces_are_prefixed_and_unique_per_call() {
        let a = mint_namespace();
        let b = mint_namespace();
        assert!(a.starts_with("cxa-test-") && b.starts_with("cxa-test-"));
        assert_ne!(a, b, "each pg() call must get its own fixture namespace");
    }
}
