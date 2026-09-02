//! Fail-closed test-database fixture shared by every DB-backed integration
//! suite (CXA-F327).
//!
//! Policy (the recorded team decision): no suite ever runs against a database
//! an operator exported, and no suite is silently green without one. Each test
//! claims its OWN ephemeral Postgres + Redis from the single compose fixture
//! [`test_pg_compose_file`]; the claim fails RED on any refusal or
//! provisioning failure, and docker-less environments skip explicitly via
//! [`claim_or_skip`].
//!
//! This structurally replaces the old fail-open posture:
//!   * `let Ok(dsn) = std::env::var("COXAGENT_TEST_PG_DSN") else { return }` —
//!     no CI job exported it, so all eight DB-backed tests quietly never ran;
//!   * the per-family `is_live_hub_db` blocklist — a hand-copied heuristic
//!     (a live hub is a `cxa` row with revision > 0) that already failed once:
//!     an exported DSN pointing at the production store filled it with
//!     `test-<pid>` rows before any probe fired. [`external_dsn_refusal`]
//!     refuses ANY exported URL before a single connection is attempted, so
//!     there is no heuristic left to misclassify.
//!
//! Teardown is a per-claim [`Drop`] guard (`docker compose down -v`), the
//! deploy_smoke.rs `Stack` pattern. A shared per-binary static is deliberately
//! NOT used: Rust never runs `Drop` for statics at process exit, so it would
//! leak a compose project on every run (CXA-B134 class) — per-claim guards are
//! the only honest teardown. A fresh database per claim is also what retires
//! the suites' old `MIGRATED` OnceCell dance: no two tests ever share a
//! database, so their concurrent `CREATE TABLE IF NOT EXISTS` can no longer
//! race the catalog.
//!
//! Isolation: every claim boots its own compose project
//! `cox-test-deps-<pid>-<nanos>-<claim>` (the atomic claim sequence
//! disambiguates two tests started inside one ~1µs clock tick) with per-claim
//! credentials and free loopback
//! ports, so repeated and concurrent runs — even of different suites — never
//! see each other's state. A run killed outright (SIGKILL) is the one residue
//! path; its projects are all named `cox-test-deps-*`, so
//! `docker ps -a --filter name=cox-test-deps-` finds, and
//! `docker compose -p <project> down -v` removes, anything left behind.
#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use coxagent_infrastructure::SqlStateStore;

/// The one compose stack every DB-backed suite provisions through. Lives
/// inside the test tree: it is a throwaway fixture, not a deployable stack,
/// and stays invisible to the product compose gates (compose_security_gate /
/// compose_healthcheck_gate / docs_ports scan the root and deploy/ stacks by
/// path only).
fn test_pg_compose_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/common/test-pg.compose.yml")
}

/// The variable the exported-DSN incident travelled in on. Still honoured —
/// by being refused, loudly, with [`external_dsn_refusal`].
const EXPORTED_DSN_VAR: &str = "COXAGENT_TEST_PG_DSN";

/// The Redis twin of the same incident class: an exported URL would point the
/// lease/desired-state traffic at whatever Redis the operator has live.
const EXPORTED_REDIS_VAR: &str = "COXAGENT_TEST_REDIS_URL";

/// Compose project prefix of every claim; the teardown-residue grep is
/// `docker ps -a --filter name=cox-test-deps-`.
const PROJECT_PREFIX: &str = "cox-test-deps";

/// Pure policy: what (if anything) is wrong with an externally provided
/// database URL. `None` — the variable is absent, so the fixture provisions.
/// ANY present value is refused: instead of probing whether a configured DSN
/// *looks* like a live hub (the heuristic blocklist this replaces, which
/// already let one exported prod DSN through), a suite never dials an
/// operator-supplied URL at all. The refusal message cites the incident and
/// the unset instruction so the fix is obvious in the failure output.
pub fn external_dsn_refusal(value: Option<&str>) -> Option<String> {
    match value {
        None => None,
        Some("") => Some(format!(
            "an external test-database URL is exported but EMPTY ({EXPORTED_DSN_VAR} \
             or {EXPORTED_REDIS_VAR}) — unset it and let the shared compose fixture \
             ({}) provision a dedicated ephemeral test database (CXA-F327)",
            test_pg_compose_file().display()
        )),
        Some(_) => Some(format!(
            "an external test-database URL is exported ({EXPORTED_DSN_VAR} or \
             {EXPORTED_REDIS_VAR}) — refusing to run against an operator-provided \
             database. This closes the incident where an exported DSN pointing at \
             the production store was filled with test-<pid> rows. Unset the \
             variable; the shared compose fixture ({}) provisions a dedicated \
             ephemeral test database automatically (CXA-F327)",
            test_pg_compose_file().display()
        )),
    }
}

/// Pure decision over the `docker info` probe: `Some(reason)` exactly when
/// docker is effectively absent — CLI not runnable, or daemon not answering.
/// That is the ONLY lawful explicit-skip situation (CXA-F327 AC4); with docker
/// present, a fixture stack that fails to come up is RED, never a skip.
fn docker_absent_reason(probe: std::io::Result<bool>, detail: &str) -> Option<String> {
    match probe {
        Err(e) => Some(format!(
            "docker CLI not runnable ({e}) — the DB-backed suites provision their \
             own ephemeral database through it, so they are explicitly skipped"
        )),
        Ok(false) => Some(format!(
            "docker daemon did not answer `docker info` ({detail}) — start the \
             daemon to run the DB-backed suites; they are explicitly skipped \
             meanwhile (they never run against a shared live database)"
        )),
        Ok(true) => None,
    }
}

/// Cached per test process: `Some(reason)` = docker absent (skip), `None` =
/// usable (provision).
static DOCKER_ABSENT: OnceLock<Option<String>> = OnceLock::new();

/// Disambiguates claims within one test process: `SystemTime` ticks at ~1µs
/// on macOS, and two tests started in the same tick must still get distinct
/// compose project names, or their networks and containers collide.
static CLAIM_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn docker_absent_cached() -> Option<String> {
    DOCKER_ABSENT
        .get_or_init(|| {
            let out = Command::new("docker")
                .arg("info")
                .arg("--format")
                .arg("ok")
                .output();
            let detail = out
                .as_ref()
                .map(|o| String::from_utf8_lossy(&o.stderr).trim().to_owned())
                .unwrap_or_default();
            docker_absent_reason(out.map(|o| o.status.success()), &detail)
        })
        .clone()
}

/// One claimed ephemeral database pair, alive for exactly one test. Dropping
/// it tears the compose project down — `down -v`, even on panic — so hold the
/// value for the whole test body.
pub struct TestDb {
    project: String,
    dsn: String,
    redis_url: String,
}

impl TestDb {
    /// The Postgres DSN of the claimed ephemeral database.
    pub fn dsn(&self) -> String {
        self.dsn.clone()
    }

    /// The Redis URL of the claimed ephemeral instance.
    pub fn redis_url(&self) -> String {
        self.redis_url.clone()
    }

    /// Fail-closed claim: refuses any exported DSN (panic), boots the shared
    /// compose stack (panic on failure, naming the fixture), and runs the
    /// schema-init migration itself so no suite ever races the catalog on a
    /// fresh database. Panics — never skips; the only lawful skip (docker
    /// absent) is decided by [`claim_or_skip`] before this is called.
    pub async fn claim() -> TestDb {
        for exported in [
            std::env::var(EXPORTED_DSN_VAR).ok(),
            std::env::var(EXPORTED_REDIS_VAR).ok(),
        ] {
            if let Some(refusal) = external_dsn_refusal(exported.as_deref()) {
                panic!("CXA-F327 fail-closed policy: {refusal}");
            }
        }

        let claim = CLAIM_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let project = format!(
            "{PROJECT_PREFIX}-{}-{}-{claim}",
            std::process::id(),
            nanos()
        );
        // Both ports claimed while BOTH listeners are alive: a sequential
        // claim-release-claim could hand the kernel's same port to redis.
        let [pg_port, redis_port] = free_loopback_ports();
        let pg_password = run_secret();
        let redis_password = run_secret();
        let dsn = format!("postgres://postgres:{pg_password}@127.0.0.1:{pg_port}/coxagent");
        let redis_url = format!("redis://:{redis_password}@127.0.0.1:{redis_port}");

        // The guard exists from the moment the stack is up: a schema-init
        // failure panics WITH the guard alive, so its Drop tears the stack
        // down even on the panic path.
        let db = Self {
            project: project.clone(),
            dsn: dsn.clone(),
            redis_url,
        };
        boot_compose(&project, pg_port, redis_port, &pg_password, &redis_password);

        // The harness owns schema-init: one migration alone on the fresh
        // database, and proof the database is REACHABLE — an unreachable one
        // fails here naming the DSN and the fixture instead of surfacing as a
        // confusing per-test connect error.
        if let Err(e) = SqlStateStore::connect(&dsn, "schema-init").await {
            panic!(
                "the ephemeral test database is up but not reachable/migratable \
                 ({project}): {e}\ndsn: {dsn}\nfixture: {}",
                test_pg_compose_file().display()
            );
        }
        db
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        // By project name only — deliberately NOT `-f <file>`: `down` still
        // interpolates the compose file, and the fixture's required-form
        // variables (${VAR:?}) would make teardown itself fail when they are
        // absent, leaving every container running. Label-driven down needs no
        // file, no credentials, and survives the fixture file moving.
        let down = Command::new("docker")
            .args([
                "compose",
                "-p",
                &self.project,
                "down",
                "-v",
                "--remove-orphans",
                "-t",
                "5",
            ])
            .output();
        match down {
            Ok(o) if o.status.success() => {}
            Ok(o) => real_eprint(&format!(
                "CXA-F327 teardown warning: `docker compose down -v` for {} failed \
                 ({}): {}\n--- stdout ---\n{}",
                self.project,
                o.status,
                String::from_utf8_lossy(&o.stderr).trim(),
                String::from_utf8_lossy(&o.stdout).trim()
            )),
            Err(e) => real_eprint(&format!(
                "CXA-F327 teardown warning: could not spawn docker to tear down \
                 {}: {e}",
                self.project
            )),
        }
    }
}

/// Write straight to the real stderr. libtest's output capture intercepts the
/// `eprintln!` macro per test and only replays it on failure — a skip (or a
/// teardown warning about docker residue) that only shows on failure is
/// exactly the invisibility CXA-F327 removes, so these lines bypass capture
/// and stay visible in every run summary.
fn real_eprint(message: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr().lock(), "{message}");
}

/// The entry the suites use: claim a dedicated ephemeral database for one
/// test — or, ONLY when docker is effectively absent, report an explicit skip
/// (the reason is printed un-captured, so it is visible in the run summary)
/// and return `None`. Every other refusal or failure panics RED out of
/// [`TestDb::claim`].
pub async fn claim_or_skip() -> Option<TestDb> {
    if let Some(reason) = docker_absent_cached() {
        real_eprint(&format!(
            "CXA-F327 explicit skip (docker absent — the only lawful green \
             non-run): {reason}"
        ));
        return None;
    }
    Some(TestDb::claim().await)
}

/// Bring the shared fixture stack up for one claim and wait until both
/// services answer their authenticating healthchecks (`--wait`). Panics RED
/// naming the fixture stack — an unprovisionable database must never be green.
fn boot_compose(
    project: &str,
    pg_port: u16,
    redis_port: u16,
    pg_password: &str,
    redis_password: &str,
) {
    let compose_file = test_pg_compose_file();
    let out = Command::new("docker")
        .args([
            "compose",
            "-p",
            project,
            "-f",
            compose_file.to_str().expect("utf-8 compose path"),
            "up",
            "-d",
            "--wait",
            "--wait-timeout",
            "90",
        ])
        .env("TEST_PG_HOST_PORT", pg_port.to_string())
        .env("TEST_PG_PASSWORD", pg_password)
        .env("TEST_REDIS_HOST_PORT", redis_port.to_string())
        .env("TEST_REDIS_PASSWORD", redis_password)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "could not spawn docker to provision the ephemeral test database \
                 (project {project}): {e}\nfixture: {}",
                compose_file.display()
            )
        });
    assert!(
        out.status.success(),
        "the shared compose fixture failed to come up (project {project}, \
         fixture {}):\n{}\n--- stdout ---\n{}\nThe DB-backed suites refuse to \
         run without their ephemeral database — fix docker/the fixture stack \
         and re-run; they never fall back to an exported {}.",
        compose_file.display(),
        String::from_utf8_lossy(&out.stderr).trim(),
        String::from_utf8_lossy(&out.stdout).trim(),
        EXPORTED_DSN_VAR
    );
}

/// Run-unique nonce for project names and per-claim credentials.
fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

/// Per-claim credential: run-unique and hex-only, so it is safe both as a
/// compose-provided password and inside a URL. It guards a tmpfs database on
/// a loopback port that lives for one test — throwaway by construction.
fn run_secret() -> String {
    format!("{:x}{:x}", std::process::id(), nanos())
}

/// Two free 127.0.0.1 ports, claimed by the kernel for the duration of the
/// call so concurrent claims never collide, then released for compose to
/// publish. The brief release→publish window is a misfortune, not a hazard —
/// a lost race fails the compose publish, which fails the claim RED, visibly.
fn free_loopback_ports() -> [u16; 2] {
    let a = std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("bind an ephemeral loopback port for the fixture stack");
    let b = std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("bind a second ephemeral loopback port for the fixture stack");
    let port =
        |l: &std::net::TcpListener| l.local_addr().expect("ephemeral loopback address").port();
    [port(&a), port(&b)]
}

// --- pure policy tests (run in every suite binary that mounts this module) ---

#[test]
fn an_absent_export_is_allowed_the_fixture_provisions() {
    assert_eq!(external_dsn_refusal(None), None);
}

#[test]
fn an_exported_but_empty_var_is_refused() {
    let refusal = external_dsn_refusal(Some("")).expect("an empty export is still an export");
    assert!(
        refusal.contains(EXPORTED_DSN_VAR),
        "the refusal must name the variable: {refusal}"
    );
}

#[test]
fn an_exported_dsn_is_refused_citing_the_incident_and_the_unset_instruction() {
    let refusal = external_dsn_refusal(Some("postgres://postgres@db.prod.example:5432/coxagent"))
        .expect("any exported DSN is refused");
    assert!(
        refusal.contains("incident"),
        "the refusal must cite the exported-DSN incident: {refusal}"
    );
    assert!(
        refusal.contains("Unset"),
        "the refusal must tell the operator how to proceed: {refusal}"
    );
    assert!(
        refusal.contains("test-pg.compose.yml"),
        "the refusal must name the fixture that replaces the export: {refusal}"
    );
}

#[test]
fn docker_outcomes_split_exactly_at_usable_or_explicit_skip() {
    let not_runnable = Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no docker",
    ));
    let daemon_down = Ok(false);
    let usable = Ok(true);

    assert!(
        docker_absent_reason(not_runnable, "").is_some(),
        "a missing docker CLI is the explicit-skip situation"
    );
    let down = docker_absent_reason(daemon_down, "Cannot connect to the Docker daemon")
        .expect("a silent daemon is still docker-less for the suites");
    assert!(
        down.contains("Cannot connect"),
        "the skip reason must carry what the probe saw: {down}"
    );
    assert_eq!(
        docker_absent_reason(usable, ""),
        None,
        "usable docker is never a skip — from here a broken stack is RED"
    );
}

/// Regression (CXA-F327 first live run): the pair is claimed while BOTH
/// listeners are alive, so the kernel can never hand pg's port to redis — a
/// sequential claim-release-claim did exactly that, and compose then failed
/// publishing two services on one port.
#[test]
fn the_port_pair_is_two_distinct_ports() {
    let [pg, redis] = free_loopback_ports();
    assert_ne!(
        pg, redis,
        "pg and redis must never share a loopback port — the second publish \
         would fail the claim"
    );
}
