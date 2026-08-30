//! COX-B008 regression guard, deploy half: a green `cargo build` is not a
//! running app.
//!
//! The bug report had two symptoms with one root cause — the Docker builder
//! failed to compile (`dead_code` on a macOS-only helper under `warnings =
//! "deny"`), so no image was produced and nothing ever answered on the
//! published host port. `platform_gates` guards the compile; this test guards
//! what the user actually asked for: run the exact command this repo's
//! docker-compose.yml documents and prove the hub answers on host port 8101.
//! It also covers failure modes a compile check can never see — a broken
//! entrypoint, a container that exits on boot, a wrong port mapping.
//!
//! `#[ignore]` because it builds a release image (minutes) and binds a host
//! port, which is wrong for a plain `cargo test`. Run it deliberately:
//!
//! ```text
//! cargo test -p coxagent-app --test deploy_smoke -- --ignored --nocapture
//! ```
//!
//! CI runs exactly that in the `deploy-smoke` job.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use coxagent_infrastructure::deploy::{reclaimable_compose_project, reclaimable_raw_container};

/// This project's assigned deploy port — fixed so it never collides with a live
/// hub bound to 4000 on the same docker host (CXA-B069 made every published port
/// env-driven). The smoke test must probe whatever port *this* invocation of
/// `docker compose` actually binds, not a constant that drifts from reality when
/// `APP_PORT` leaks in from the environment (CXA-B077).
const DEFAULT_HOST_PORT: u16 = 8101;

/// How long the stack gets to build and answer before the test gives up.
const READY_TIMEOUT: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_secs(3);

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Pure decision over the environment's APP_PORT value — no IO, so it is
/// trivially unit-testable below without mutating process globals.
fn resolve_host_port(app_port: Option<&str>) -> u16 {
    match app_port {
        None | Some("") => DEFAULT_HOST_PORT,
        Some(n) => n
            .parse()
            .unwrap_or_else(|why| panic!("APP_PORT=`{n}` is not a valid host port ({why})")),
    }
}

/// Resolve a compose host-port field into the number Docker actually binds,
/// honouring an env override. docker-compose.yml publishes `"${APP_PORT:-8101}:4000"`
/// — Docker binds `$APP_PORT` when it is set and falls back to `8101` otherwise.
///
/// Mirroring the semantics in docs_ports.rs keeps this test reading reality:
/// when a concurrent worktree holds 8101 and a redeploy passes `APP_PORT=8110`,
/// this returns 8110 so we probe exactly what *we* orchestrated instead of a
/// stale/unrelated container squatting on the default (the CXA-B077 fragility).
fn effective_host_port() -> u16 {
    resolve_host_port(std::env::var("APP_PORT").ok().as_deref())
}

fn compose(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new("docker")
        .arg("compose")
        .args(args)
        .current_dir(root)
        // The documented bring-up command sets both required secrets inline
        // (PG_PASSWORD for Postgres, COXAGENT_ADMIN_PASSWORD for first-run
        // super-admin bootstrap); without either, compose fails interpolation.
        //
        // APP_PORT is deliberately *not* forced here: its absence lets us resolve
        // exactly what this run will bind and prove we probe it. Set it in env only
        // if you need this worktree on a non-default host port alongside another one.
        .env("PG_PASSWORD", "ci-smoke")
        .env("COXAGENT_ADMIN_PASSWORD", "ci-smoke")
        .output()
        .unwrap_or_else(|e| panic!("`docker compose {}` failed to spawn: {e}", args.join(" ")))
}

/// What one container squatting the effective host port means for this smoke
/// gate — decided as a pure function of that container's compose-project
/// ownership label and name so every branch is deterministically testable
/// without invoking docker (CXA-B082, CXA-B083). Which holders may be reclaimed
/// is decided by the SHIPPED policy (imported above) —
/// [`reclaimable_compose_project`] for labelled compose projects,
/// [`reclaimable_raw_container`] for label-less containers — so this gate
/// exercises the module production uses, never a private copy that can drift
/// from it.
#[derive(Debug, PartialEq)]
enum HolderDecision {
    /// Belongs to a reclaimable agent-preview compose project — tear that whole
    /// project down to release its ports cleanly.
    EvictComposeProject(String),
    /// Label-less AND demonstrably ours by name (`cox-` prefix) — a leftover of
    /// our own preview; stop just it by id.
    EvictRawContainer(String),
}

impl HolderDecision {
    fn evict(&self) {
        match self {
            HolderDecision::EvictComposeProject(project) => {
                let _ = Command::new("docker")
                    .args([
                        "compose",
                        "-p",
                        project.as_str(),
                        "down",
                        "--remove-orphans",
                    ])
                    .output();
            }
            HolderDecision::EvictRawContainer(id) => {
                let _ = Command::new("docker").args(["stop", id]).output();
            }
        }
    }
}

/// The container ids of every running container publishing `port` (CXA-B077:
/// the effective `${APP_PORT:-8101}`, not a constant that can drift).
fn holders_on_the(port: u16) -> Vec<String> {
    let out = Command::new("docker")
        .args(["ps", "-q", "--filter", &format!("publish={port}")])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    out.lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn decide_holder(
    owner_label: Option<&str>,
    container_name: &str,
    id: String,
) -> Option<HolderDecision> {
    match owner_label {
        Some(project) if reclaimable_compose_project(project) => {
            Some(HolderDecision::EvictComposeProject(project.to_owned()))
        }
        // A protected or foreign compose project is never destroyed; deciding None here does not,
        // however, drop it silently — [`assess_host_port`] turns any such holder into BlockedByForeign,
        // which makes verification skip-with-report instead of failing red or touching their stack.
        Some(_protected_or_foreign) => None,
        // A label-less container is judged by its NAME — the only ownership
        // signal it has (CXA-B083). Only a demonstrably ours `cox-`-named
        // squatter is stopped by id; the live hub / shared infra launched via
        // plain `docker run`, or any foreign container (the CXA-B083 repro:
        // `docker run -p 8101:80 nginx`), is never touched — [`assess_host_port`]
        // reports it as BlockedByForeign instead.
        None if reclaimable_raw_container(container_name) => {
            Some(HolderDecision::EvictRawContainer(id))
        }
        None => None,
    }
}

/// The `(container name, compose-project owner label)` parsed from one
/// `docker inspect` line in its `name|label` shape. `None` when the line is
/// not that shape — a vanished container inspects to empty output. The label
/// is `None` when empty: docker prints nothing for a label-less container.
fn parse_ownership(line: &str) -> Option<(String, Option<String>)> {
    let (name, label) = line.trim().split_once('|')?;
    // `docker inspect` prints container names with a leading slash.
    let name = name.trim_start_matches('/').to_owned();
    let label = label.trim();
    Some((name, (!label.is_empty()).then(|| label.to_owned())))
}

/// The container name and compose-project owner label of one container, from a
/// single inspect. `None` only when the container is gone — it can no longer
/// hold the port, so it drops out of the snapshot.
fn inspect_ownership_of(id: &str) -> Option<(String, Option<String>)> {
    // `|` separates the two fields because docker's inspect formatter does NOT
    // interpret `\t` (verified live: it prints a literal backslash-t), and `|`
    // can never occur in a container name or compose project name.
    let out = Command::new("docker")
        .args([
            "inspect",
            "--format",
            "{{ .Name }}|{{ index .Config.Labels \"com.docker.compose.project\" }}",
            id,
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None; // container gone — it can no longer hold the port
    }
    parse_ownership(&String::from_utf8_lossy(&out.stdout))
}

/// Whether this smoke gate may bind the effective host port and verify this
/// build's stack.
#[derive(Debug, PartialEq)]
enum PortState {
    /// Every holder on that port is evictable — free them and verify normally.
    Verifiable,
    /// A protected/foreign compose project, or a raw container that cannot
    /// prove it is ours, holds the effective host port; we refuse to destroy
    /// unrelated user infrastructure (CXA-B082, CXA-B083), and because the
    /// port is fixed for the run we cannot bind either. Verification skips
    /// with this holder named instead of failing red or touching their stack.
    BlockedByForeign(String),
}

/// What this smoke gate plans to do about the effective host port — computed
/// PURELY from one immutable host-port snapshot so every branch is
/// deterministically testable without invoking docker (CXA-B082, CXA-B083).
/// Each entry is `(container id, container name, resolved compose-project
/// owner label)` for one container publishing that port.
#[derive(Debug, PartialEq)]
enum PortPlan {
    /// Every holder on the port may be reclaimed — schedule those teardowns
    /// below, then verify normally.
    EvictThenVerify(Vec<HolderDecision>),
    /// A protected/foreign holder sits on the effective host port; refuse to
    /// destroy it, touch nothing else either (see [`plan_holders`]).
    Skip(String),
}

/// Any single protected/foreign holder wins over every eviction below: even after
/// stopping raw/reclaimable squatters another address-family duplicate held by a
/// protected project could keep the port bound, so once blocked we never risk a
/// half-cleared port or touch their stack — no teardown runs at all.
fn plan_holders(holders: &[(String, String, Option<String>)]) -> PortPlan {
    let mut blocked: Option<String> = None;
    let mut actions = Vec::new();
    for (id, name, owner_label) in holders {
        match decide_holder(owner_label.as_deref(), name, id.clone()) {
            None => {
                if blocked.is_none() {
                    // Name the holder this gate refuses to touch: its compose
                    // project, or — for a label-less container — the container
                    // name itself.
                    blocked = Some(owner_label.clone().unwrap_or_else(|| name.clone()));
                }
            }
            Some(action) => actions.push(action),
        }
    }
    if let Some(owner) = blocked {
        return PortPlan::Skip(owner);
    }
    PortPlan::EvictThenVerify(actions)
}

fn assess_host_port(port: u16) -> PortState {
    let snapshot = holders_on_the(port)
        .into_iter()
        .filter_map(|id| {
            let (name, owner_label) = inspect_ownership_of(&id)?;
            Some((id, name, owner_label))
        })
        .collect::<Vec<_>>();
    match plan_holders(&snapshot) {
        // First EVICT every reclaimable squatter / our own raw container queued
        // above...
        PortPlan::EvictThenVerify(actions) => {
            for action in &actions {
                action.evict();
            }
            // ...only then signal that verification may bind that port fresh.
            // A half-cleared port after any failed stop still lets this proceed —
            // the probe will simply fail like any binding collision would have.
            PortState::Verifiable
        }
        // Skip-with-report; not even one reclaimable squatter sharing the port with
        // a protected stack gets torn down first ([CXA-B082] safest choice).
        PortPlan::Skip(owner) => PortState::BlockedByForeign(owner),
    }
}

/// Exclusive, ephemeral ownership of the stack for one run: brought up here,
/// torn down when this value drops, even on panic, so a failing run never
/// leaves a container holding the host port. Only ever constructed on the
/// [`PortState::Verifiable`] path — when a protected/foreign project holds the
/// effective host port we deliberately construct no stack and touch nothing
/// (CXA-B082).
struct Stack {
    root: PathBuf,
}

impl Stack {
    /// Brings THIS directory's own compose project down before `up`, symmetric
    /// with [`Drop`], so each verification pass starts from nothing it owns.
    fn claim(root: PathBuf) -> Self {
        let stack = Self { root };
        stack.down();
        stack
    }

    fn down(&self) {
        let _ = compose(&self.root, &["down", "-v"]);
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        self.down();
    }
}

/// The HTTP status the hub answers with on the published port, or `None`
/// while it is not answering yet.
async fn probe(client: &reqwest::Client, host_port: u16) -> Option<u16> {
    client
        .get(format!("http://localhost:{host_port}/"))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .ok()
        .map(|r| r.status().as_u16())
}

#[tokio::test]
#[ignore = "builds a release image and binds host port 8101; run with --ignored"]
async fn compose_stack_comes_up_and_answers_on_the_published_port() {
    let root = repo_root();

    // Probe whatever this run actually orchestrated — the effective
    // `${APP_PORT:-8101}` fallback — not a constant that could point at an
    // unrelated container squatting on the default port (CXA-B077).
    let host_port = effective_host_port();

    // CXA-B082 re-scope: decide what owns the effective host port BEFORE
    // binding anything. A protected or foreign compose project — or a raw
    // container that cannot prove it is ours (CXA-B083, e.g. someone's plain
    // `docker run ... nginx`) — is never destroyed, and because the port is
    // fixed for the run we cannot bind over it either; in that case we skip
    // verification with a clear report instead of failing red or tearing
    // unrelated infrastructure down. Reclaimable cox-preview squatters and our
    // own cox-named raw leftovers are still evicted here so normal runs verify
    // in full — which always happens on ephemeral CI runners where no foreign
    // stack can exist.
    // stack can exist.
    match assess_host_port(host_port) {
        PortState::BlockedByForeign(owner) => {
            eprintln!(
                "deploy-smoke SKIPPED: host port {host_port} is held by `{owner}` — a foreign \
                 or protected compose project/container this gate refuses to tear down \
                 (CXA-B082, CXA-B083). Verification could not bind without destroying \
                 unrelated infrastructure."
            );
            return;
        }
        PortState::Verifiable => {}
    }

    let stack = Stack::claim(root.clone());

    // The exact command docker-compose.yml documents. A compile error in the
    // builder stage surfaces here as a non-zero exit — the original bug.
    let up = compose(&root, &["up", "-d", "--build"]);
    assert!(
        up.status.success(),
        "`docker compose up -d --build` failed ({}):\n{}",
        up.status,
        String::from_utf8_lossy(&up.stderr)
    );

    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    let mut last = None;
    while std::time::Instant::now() < deadline {
        last = probe(&client, host_port).await;
        if last == Some(200) {
            drop(stack);
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let logs = compose(&stack.root, &["logs", "--no-color", "--tail", "50"]);
    panic!(
        "hub never answered 200 on host port {host_port} (last status: {last:?})\n{}",
        String::from_utf8_lossy(&logs.stdout)
    );
}

// ---------------------------------------------------------------------------
// The pure resolver, run against synthetic values — proves the probe always
// tracks what docker compose actually binds, default or override (CXA-B077).
// ---------------------------------------------------------------------------

#[test]
fn unset_app_port_resolves_to_the_assigned_default() {
    assert_eq!(resolve_host_port(None), DEFAULT_HOST_PORT);
}

#[test]
fn empty_app_port_falls_back_to_the_assigned_default() {
    // Compose treats an empty VAR like unset for `${VAR:-default}` too.
    assert_eq!(resolve_host_port(Some("")), DEFAULT_HOST_PORT);
}

#[test]
fn an_override_is_probed_as_orchestrated() {
    assert_eq!(resolve_host_port(Some("8110")), 8110);
}

#[test]
fn a_bad_override_panics_rather_than_guessing_a_probe_target() {
    let caught = std::panic::catch_unwind(|| resolve_host_port(Some("not-a-port")));
    let msg = match caught {
        Err(payload) => payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .unwrap_or_default()
            .to_string(),
        Ok(_) => panic!("a non-numeric APP_PORT must not silently probe a guessed port"),
    };
    assert!(msg.contains("APP_PORT"), "unhelpful panic message: {msg}");
}

/// The holder-decision and port-state logic is pure over ownership labels and
/// container names; verify every branch without invoking docker, and pin that
/// protected infrastructure — compose project OR raw label-less container —
/// can never be classified as downed, and that a foreign raw container (the
/// CXA-B083 repro) is left alone rather than force-stopped. A regression here
/// would be a self-inflicted outage (CXA-B082 re-scope pins the
/// foreign/protected skip-with-report contract).
#[cfg(test)]
mod decide_tests {
    use super::{decide_holder, plan_holders, HolderDecision, PortPlan};

    /// One snapshot entry: `(container id, container name, owner label)`.
    fn holder(name: &str, owner: Option<&str>) -> (String, String, Option<String>) {
        ("abc".to_owned(), name.to_owned(), owner.map(str::to_owned))
    }

    #[test]
    fn reclaimable_agent_preview_is_torn_down_as_a_project() {
        assert_eq!(
            decide_holder(
                Some("cox--other-worktree"),
                "cox--other-worktree-hub-1",
                "abc".to_owned()
            ),
            Some(HolderDecision::EvictComposeProject(
                "cox--other-worktree".to_owned()
            ))
        );
    }

    /// A label-less container is only stopped when its NAME shows it is ours —
    /// docker names compose containers `<project>-<service>-<n>`, so a `cox-`
    /// name is a leftover of our own preview (B080's raw fallback target).
    #[test]
    fn our_cox_named_raw_container_is_stopped_by_id() {
        assert_eq!(
            decide_holder(None, "cox--stale-preview-hub-1", "deadbeef".to_owned()),
            Some(HolderDecision::EvictRawContainer("deadbeef".to_owned()))
        );
    }

    /// CXA-B083 AC: an unlabeled foreign container squatting :8101 (the repro:
    /// plain `docker run -p 8101:80 nginx`) must NEVER be stopped by id — the
    /// raw branch used to fire with no ownership check at all.
    #[test]
    fn foreign_raw_container_is_never_stopped_by_id() {
        for name in ["nginx", "bold_curie", "my-live-hub", ""] {
            assert_eq!(
                decide_holder(None, name, "deadbeef".to_owned()),
                None,
                "raw container `{name}` cannot prove it is ours and must never be stopped"
            );
        }
    }

    /// CXA-B083 AC: the live hub or shared infra launched via plain
    /// `docker run` (no compose label to read) is never stopped either — same
    /// protected-name policy as compose projects, case-spoof safe.
    #[test]
    fn protected_named_raw_container_is_never_stopped_by_id() {
        for name in [
            "coxagent-hub-1",
            "cox-infra-redis-1",
            "COXAGENT-HUB",
            "CoxAgent-Hub",
        ] {
            assert_eq!(
                decide_holder(None, name, "deadbeef".to_owned()),
                None,
                "{name} is protected infrastructure and must never be stopped"
            );
        }
    }

    /// CXA-B082 AC: a foreign non-cox compose project holding :8101 must NEVER be
    /// torn down by this gate. `decide_holder` refuses to classify it for eviction,
    /// so nothing of theirs is stopped.
    #[test]
    fn foreign_non_preview_project_is_left_to_itself_not_destroyed() {
        for owner in ["someone-elses-stack", "cxa-backend", "myapp-prod"] {
            assert_eq!(
                decide_holder(Some(owner), &format!("{owner}-web-1"), "abc".to_owned()),
                None
            );
        }
    }

    /// Protected hub/infra prefixes are likewise never evicted (casing-spoof safe).
    #[test]
    fn live_hub_and_infra_prefixes_can_not_be_classified_for_takedown() {
        for owner in [
            "coxagent",
            "coxagent-gateway",
            "coxagent-db",
            "cox-infra",
            "cox-infra-db",
            "COXAGENT",
            "CoxAgent",
            "CoxAgent-Gateway",
            "COXAGENT-GATEWAY",
            "COX-INFRA",
            "COX-Infra-DB",
            "Cox-Infra-Db",
        ] {
            assert_eq!(
                decide_holder(Some(owner), "abc", "abc".to_owned()),
                None,
                "{owner} must never be evicted"
            );
        }
    }

    /// CXA-B082 AC (aggregation half): every holder evictable → schedule those
    /// teardowns and let verification proceed.
    #[test]
    fn only_reclaimable_and_raw_holders_schedule_eviction() {
        let holders = vec![
            holder("cox--other-worktree-hub-1", Some("cox--other-worktree")),
            holder("cox--stale-preview-db-1", None),
            holder("cox-cxa-codebase-hub-1", Some("cox-cxa-codebase")),
        ];
        assert_eq!(
            plan_holders(&holders),
            PortPlan::EvictThenVerify(vec![
                HolderDecision::EvictComposeProject("cox--other-worktree".to_owned()),
                HolderDecision::EvictRawContainer("abc".to_owned()),
                HolderDecision::EvictComposeProject("cox-cxa-codebase".to_owned()),
            ])
        );
    }

    /// CXA-B082 AC: a foreign non-cox compose project holding :8101 must not make
    /// the gate fail red nor tear their stack down — it yields a skip with their
    /// owner named.
    #[test]
    fn foreign_project_blocking_port_is_reported_as_a_skip() {
        let holders = vec![holder(
            "someone-elses-stack-web-1",
            Some("someone-elses-stack"),
        )];
        assert_eq!(
            plan_holders(&holders),
            PortPlan::Skip("someone-elses-stack".to_owned())
        );
    }

    /// CXA-B083 AC: a foreign raw container holding :8101 alone must not make
    /// the gate fail red NOR get stopped — skip-with-report names the container.
    #[test]
    fn foreign_raw_container_blocking_port_is_reported_as_a_skip() {
        let holders = vec![holder("nginx", None)];
        assert_eq!(plan_holders(&holders), PortPlan::Skip("nginx".to_owned()));
    }

    /// CXA-B083 AC (from the CXA-B084 branch): an anonymous/foreign raw holder
    /// squatting :8101 blocks the run even alongside reclaimable squatters —
    /// no eviction may run while an un-touchable raw holder keeps the port.
    #[test]
    fn anonymous_raw_holder_wins_over_co_squatters_no_eviction_runs() {
        let mixed = vec![
            holder("nginx", None),
            holder("cox--stale-preview-hub-1", Some("cox--stale-preview")),
            holder("cox--stale-raw", None),
        ];
        assert_eq!(
            plan_holders(&mixed),
            PortPlan::Skip("nginx".to_owned()),
            "no eviction may run while an un-touchable raw holder keeps the port"
        );
    }

    /// A single protected hub/infra holder blocks the whole run, even when other
    /// squatters sharing :8101 would be reclaimable — we never half-clear the port
    /// or touch anything while a protected stack is on it ([CXA-B082] safest).
    /// CXA-B083 extends this to a protected RAW holder naming a container.
    #[test]
    fn one_protected_or_mixed_blocking_holder_wins_over_every_reclaimable() {
        for (foreign, reclaimable_present) in [
            ("cxa-backend", false),
            ("myapp-prod", true),
            ("coxagent-db", true),
        ] {
            let mut holders = vec![holder("cxa-backend-web-1", Some(foreign))];
            if reclaimable_present {
                holders.push(holder(
                    "cox--stale-preview-hub-1",
                    Some("cox--stale-preview"),
                ));
                // Skip must also short-circuit any raw-container teardown scheduled for them.
                holders.push(holder("cox--stale-preview-db-1", None));
            }
            assert_eq!(
                plan_holders(&holders),
                PortPlan::Skip(foreign.to_owned()),
                "{foreign} should block verification without evicting co-squatters"
            );
        }
    }

    /// CXA-B083: a protected raw container (hub via plain `docker run`) blocks
    /// the whole run with the CONTAINER named, and co-squatters are untouched.
    #[test]
    fn protected_raw_container_blocking_port_is_reported_as_a_skip() {
        let holders = vec![
            holder("coxagent-hub-1", None),
            holder("cox--stale-preview-db-1", None),
        ];
        assert_eq!(
            plan_holders(&holders),
            PortPlan::Skip("coxagent-hub-1".to_owned())
        );
    }
}

/// The `name|label` parser behind the snapshot: pin the exact output shapes
/// live docker emits (verified byte-for-byte during CXA-B083) — a label-less
/// container's line ends in a bare `|`, compose names arrive with a leading
/// slash, and a vanished container inspects to empty output.
#[cfg(test)]
mod ownership_parse_tests {
    use super::parse_ownership;

    #[test]
    fn a_label_less_container_parses_to_no_owner() {
        assert_eq!(
            parse_ownership("/cxa-b083-fmt-check|"),
            Some(("cxa-b083-fmt-check".to_owned(), None))
        );
    }

    #[test]
    fn a_compose_container_parses_to_its_project_label() {
        assert_eq!(
            parse_ownership("/cox-cxa-codebase-coxagent-1|cox-cxa-codebase"),
            Some((
                "cox-cxa-codebase-coxagent-1".to_owned(),
                Some("cox-cxa-codebase".to_owned())
            ))
        );
    }

    #[test]
    fn a_vanished_container_inspects_to_nothing() {
        assert_eq!(parse_ownership(""), None);
    }
}
