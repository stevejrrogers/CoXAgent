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

/// The published host port. Fixed by this project's deploy config — see the
/// header comment in docker-compose.yml.
const HOST_PORT: u16 = 8101;

/// How long the stack gets to build and answer before the test gives up.
const READY_TIMEOUT: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_secs(3);

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn compose(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new("docker")
        .arg("compose")
        .args(args)
        .current_dir(root)
        // The documented bring-up command sets both required secrets inline
        // (PG_PASSWORD for Postgres, COXAGENT_ADMIN_PASSWORD for first-run
        // super-admin bootstrap); without either, compose fails interpolation.
        .env("PG_PASSWORD", "ci-smoke")
        .env("COXAGENT_ADMIN_PASSWORD", "ci-smoke")
        .output()
        .unwrap_or_else(|e| panic!("`docker compose {}` failed to spawn: {e}", args.join(" ")))
}

/// Whether an automated pass may tear down a docker compose project to free its
/// host port.
///
/// Mirrors production deploy's shared policy (`reclaimable::reclaimable_compose_project`)
/// rather than importing it: only agent-managed preview deployments (`cox-...`)
/// are evictable. The live hub (`coxagent`) and shared backing infra (`cox-infra`)
/// are NEVER touched — tearing those down to free :8101 would be a self-inflicted
/// outage, and they bind port 4000 anyway (see AGENTS.md). Anything outside our own
/// namespace is foreign and left alone. Unlabelled raw containers are stopped by id
/// only when their NAME itself proves agent ownership (CXA-B083) — see
/// [`decide_holder`].
fn reclaimable(project: &str) -> bool {
    let lower = project.to_ascii_lowercase();
    if lower == "cox-infra"
        || lower == "coxagent"
        || lower.starts_with("coxagent")
        || lower.starts_with("cox-infra")
    {
        return false;
    }
    lower.starts_with("cox-")
}

/// What one container squatting HOST_PORT means for this smoke gate — decided as
/// a pure function of that container's recoverable identity (compose-project
/// label + container name) so every branch is deterministically testable
/// without invoking docker (CXA-B082, CXA-B083).
#[derive(Debug, PartialEq)]
enum HolderDecision {
    /// Belongs to a reclaimable agent-preview compose project — tear that whole
    /// project down to release its ports cleanly.
    EvictComposeProject(String),
    /// Unlabelled (a raw `docker run`) yet positively identified as OUR OWN
    /// reclaimable agent-preview by its name (`docker run --name cox--…`) —
    /// stop just it by id. An anonymous or foreign-named raw container is
    /// NEVER stopped (CXA-B083).
    EvictRawContainer(String),
}

impl HolderDecision {
    fn evict(&self) {
        match self {
            HolderDecision::EvictComposeProject(project) => {
                let _ = Command::new("docker")
                    .args(["compose", "-p", project.as_str(), "down", "--remove-orphans"])
                    .output();
            }
            HolderDecision::EvictRawContainer(id) => {
                let _ = Command::new("docker").args(["stop", id]).output();
            }
        }
    }
}

/// The container ids of every running container publishing HOST_PORT.
fn holders_on_host_port() -> Vec<String> {
    let out = Command::new("docker")
        .args(["ps", "-q", "--filter", &format!("publish={HOST_PORT}")])
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

/// The field separator between recoverable identifiers in one inspect pass —
/// a control char no docker identifier can contain.
const IDENT_SEP: char = '\u{1e}';

/// Recoverable ownership identifiers of one running holder, fetched together so
/// classification needs exactly one `docker inspect` per squatter even when the
/// compose-project label is absent (the raw `docker run` case the CXA-B083
/// guard exists for).
///
/// Nothing here is read from disk or spawned ad-hoc; [`inspect_holder`] takes
/// one snapshot of the outside world and every subsequent decision is a pure
/// function over it (see the IO discipline in AGENTS.md).
#[derive(Debug)]
struct HolderIdentity {
    /// `.Config.Labels["com.docker.compose.project"]`, empty when unlabelled.
    owner_project: String,
    /// The holder's docker name with any leading `/` stripped, so both identity
    /// fields share one normalized shape before any decision reads them.
    container_name: String,
}

impl HolderIdentity {
    fn new(inspect_output: &str) -> Option<Self> {
        let mut parts = inspect_output.split(IDENT_SEP);
        let owner_project = parts.next().unwrap_or_default().trim().to_owned();
        // Trim fully, then drop a single leading '/': docker emits names like
        // "/nginx\n", and surrounding whitespace must never reach a policy
        // comparison later.
        let container_name = parts
            .next()
            .unwrap_or_default()
            .trim()
            .strip_prefix('/')
            .unwrap_or_default()
            .to_owned();
        // A holder with no recoverable identity at all (no project label, no
        // usable name) cannot be proven to be ours; surface as "no identity"
        // so classification declines to touch it.
        if owner_project.is_empty() && container_name.is_empty() {
            return None;
        }
        Some(Self {
            owner_project,
            container_name,
        })
    }
}

/// The `docker inspect --format` template recovering one holder's identity.
///
/// Built with `format!` so [`IDENT_SEP`] reaches docker as the actual control
/// char: inside a plain string literal `{IDENT_SEP}` is just that literal text,
/// docker would echo it back, and `HolderIdentity::new` would never find a
/// separator to split on — silently breaking every eviction decision (the bug
/// the original CXA-B083 branch shipped; see [`parse_tests`]).
fn inspect_template() -> String {
    format!("{{ index .Config.Labels \"com.docker.compose.project\" }}{IDENT_SEP}{{ index .Name }}")
}

fn inspect_holder(id: &str) -> Option<HolderIdentity> {
    Command::new("docker")
        .args(["inspect", "--format", &inspect_template(), id])
        .output()
        .ok()
        // A dead squatter races between `ps` and `inspect`; treat it as gone —
        // nothing left to evict, and _not_ a reason to fail the whole smoke pass.
        .and_then(|o| HolderIdentity::new(&String::from_utf8_lossy(&o.stdout)))
}

/// What one squatting holder should be done with, as a pure function of its
/// recovered identity so every branch is deterministically testable without
/// invoking docker.
///
/// The single source of truth for which names are ours is [`reclaimable`]
/// (itself mirroring production's `reclaimable_compose_project`, per the note
/// on that fn). That predicate is applied to whichever identity field actually
/// carries an ownership claim:
///
/// * A compose holder (`owner_project` set) — evictable iff its project is a
///   reclaimable agent-preview; a protected or foreign project is never touched.
/// * An unlabelled raw holder (`owner_project` empty) — evicted by id ONLY if
///   its container **name** itself proves it to be a reclaimable agent-preview
///   of ours. This closes CXA-B083: previously any raw container on :8101 was
///   stopped by id regardless of what it was, so a live hub / db / foreign
///   service launched via plain `docker run -p 8101:…` could be silently
///   killed. Now anonymous and foreign holders — and anything named like our
///   protected control plane — are left strictly alone.
///
/// A `None` verdict is never a silent pass-through: [`plan_holders`] turns any
/// declined holder into a skip-with-report naming it, because the fixed
/// HOST_PORT cannot be bound while it stays.
fn decide_holder(identity: &HolderIdentity, id: String) -> Option<HolderDecision> {
    if !identity.owner_project.is_empty() {
        return reclaimable(&identity.owner_project)
            .then(|| HolderDecision::EvictComposeProject(identity.owner_project.clone()));
    }
    // `container_name` is already normalized (trimmed, leading `/` stripped)
    // by `HolderIdentity::new`; an empty value means no usable name to judge
    // ownership from — an anonymous container is never ours to stop.
    if identity.container_name.is_empty() {
        return None;
    }
    reclaimable(&identity.container_name).then_some(HolderDecision::EvictRawContainer(id))
}

/// Whether this smoke gate may bind HOST_PORT and verify this build's stack.
#[derive(Debug, PartialEq)]
enum PortState {
    /// Every holder on :8101 is evictable — free them and verify normally.
    Verifiable,
    /// A holder this gate refuses to touch keeps :8101: a protected or foreign
    /// compose project, or a raw container not positively owned by this agent
    /// (CXA-B082 + CXA-B083). Because HOST_PORT is fixed we cannot bind either;
    /// verification skips with that holder named instead of failing red or
    /// tearing unrelated infrastructure down.
    BlockedByForeign(String),
}

/// What this smoke gate plans to do about :8101 — computed PURELY from one
/// immutable host-port snapshot so every branch is deterministically testable
/// without invoking docker (CXA-B082). Each entry is `(container id, recovered
/// [`HolderIdentity`])` for one container publishing HOST_PORT; a `None`
/// identity means the holder vanished (or left nothing recoverable) between
/// `ps` and `inspect`.
#[derive(Debug, PartialEq)]
enum PortPlan {
    /// Every holder on :8101 may be reclaimed — schedule those teardowns below,
    /// then verify normally.
    EvictThenVerify(Vec<HolderDecision>),
    /// A holder this gate refuses to touch keeps :8101; refuse to destroy it and
    /// touch nothing else either (see [`plan_holders`]).
    Skip(String),
}

/// Any single holder we decline to evict wins over every eviction below: even
/// after stopping reclaimable squatters, another address-family duplicate held
/// by a protected project or an un-evictable raw container could keep :8101
/// bound, so once blocked we never risk a half-cleared port or touch their
/// stack — no teardown runs at all. The reported owner is the holder's compose
/// project when it has one, else its container name.
fn plan_holders(holders: &[(String, Option<HolderIdentity>)]) -> PortPlan {
    let mut blocked: Option<String> = None;
    let mut actions = Vec::new();
    for (id, identity) in holders {
        // Unidentifiable (vanished between `ps` and `inspect`) — nothing to act on.
        let Some(identity) = identity else {
            continue;
        };
        match decide_holder(identity, id.clone()) {
            None => {
                if blocked.is_none() {
                    let owner = if identity.owner_project.is_empty() {
                        identity.container_name.clone()
                    } else {
                        identity.owner_project.clone()
                    };
                    blocked = Some(owner);
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

fn assess_host_port() -> PortState {
    let snapshot = holders_on_host_port()
        .into_iter()
        .map(|id| {
            let identity = inspect_holder(&id);
            (id, identity)
        })
        .collect::<Vec<_>>();
    match plan_holders(&snapshot) {
        // First EVICT every reclaimable squatter / raw container queued above...
        PortPlan::EvictThenVerify(actions) => {
            for action in &actions {
                action.evict();
            }
            // ...only then signal that verification may bind HOST_PORT fresh.
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
/// [`PortState::Verifiable`] path — when a protected/foreign project holds :8101
/// we deliberately construct no stack and touch nothing (CXA-B082).
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
async fn probe(client: &reqwest::Client) -> Option<u16> {
    client
        .get(format!("http://localhost:{HOST_PORT}/"))
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

    // CXA-B082/B083 re-scope: decide what owns :8101 BEFORE binding anything. A
    // holder this gate refuses to touch — a protected or foreign compose project
    // (someone else's stack — e.g. `cxa-backend`, a user's `myapp-prod`) or a raw
    // container not positively owned by this agent (an anonymous `docker run`,
    // CXA-B083) — is never destroyed, and because HOST_PORT is fixed by our
    // deploy config we cannot bind over it either; in that case we skip
    // verification with a clear report instead of failing red or tearing
    // unrelated infrastructure down. Reclaimable cox-preview squatters and raw
    // containers positively owned by name are still evicted here so normal runs
    // verify in full — which always happens on ephemeral CI runners where no
    // foreign stack can exist.
    match assess_host_port() {
        PortState::BlockedByForeign(owner) => {
            eprintln!(
                "deploy-smoke SKIPPED: host port {HOST_PORT} is held by `{owner}`, which \
                 this gate refuses to tear down (CXA-B082/CXA-B083). Verification could \
                 not bind without destroying unrelated infrastructure."
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
        last = probe(&client).await;
        if last == Some(200) {
            drop(stack);
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let logs = compose(&stack.root, &["logs", "--no-color", "--tail", "50"]);
    panic!(
        "hub never answered 200 on host port {HOST_PORT} (last status: {last:?})\n{}",
        String::from_utf8_lossy(&logs.stdout)
    );
}

/// The holder-decision and port-state logic is pure over each holder's
/// recovered identity; verify every branch without invoking docker, and pin
/// that protected infrastructure can never be classified as downed — a
/// regression here would be a self-inflicted outage (CXA-B082 pins the
/// foreign/protected skip-with-report contract, CXA-B083 the raw-name guard).
#[cfg(test)]
mod decide_tests {
    use super::{decide_holder, plan_holders, HolderDecision, HolderIdentity, PortPlan};

    /// Build a holder from raw inspect fields: `owner_project` is
    /// `.Config.Labels["com.docker.compose.project"]` (empty for unlabelled raw
    /// containers); `container_name` mirrors docker's `.Name` after our own
    /// strip of its leading `/`.
    fn holder(owner_project: &str, container_name: &str) -> HolderIdentity {
        HolderIdentity {
            owner_project: owner_project.to_owned(),
            container_name: container_name.to_owned(),
        }
    }

    /// One snapshot entry `(container id, identity)` with id "abc".
    fn id_with(owner_project: &str, container_name: &str) -> (String, Option<HolderIdentity>) {
        (
            "abc".to_owned(),
            Some(holder(owner_project, container_name)),
        )
    }

    #[test]
    fn reclaimable_agent_preview_is_torn_down_as_a_project() {
        assert_eq!(
            decide_holder(&holder("cox--other-worktree", "ignored"), "abc".to_owned()),
            Some(HolderDecision::EvictComposeProject(
                "cox--other-worktree".to_owned()
            ))
        );
    }

    /// CXA-B083 AC: an unlabelled `docker run --name cox--slot-b-hub -p 8101:… …`
    /// left by an earlier aborted run IS positively ours (by name) and so is the
    /// one raw container this gate may still stop by id.
    #[test]
    fn reclaimable_raw_preview_container_is_stopped_by_id() {
        assert_eq!(
            decide_holder(&holder("", "cox--slot-b-hub"), "deadbeef".to_owned()),
            Some(HolderDecision::EvictRawContainer("deadbeef".to_owned()))
        );
    }

    /// CXA-B083 regression guard: an unlabelled holder with no recoverable
    /// identity at all must NOT be force-stopped by id.
    #[test]
    fn anonymous_raw_container_is_left_strictly_alone() {
        let anonymous = HolderIdentity {
            owner_project: String::new(),
            container_name: String::new(),
        };
        assert_eq!(decide_holder(&anonymous, "deadbeef".to_owned()), None);
    }

    /// The REPRO from CXA-B083 verbatim: `docker run -d -p 8101:80 nginx`
    /// squats :8101 with no compose label and no agent-owned name. It is not
    /// ours — it must survive untouched instead of being stopped by id.
    #[test]
    fn foreign_raw_container_is_not_stopped_by_id() {
        for foreign in ["nginx", "my-app", "someone-svc"] {
            assert_eq!(
                decide_holder(&holder("", foreign), "abc".to_owned()),
                None,
                "{foreign} is not ours and must never be stopped"
            );
        }
    }

    /// Even an unlabelled container whose NAME claims our control plane — e.g.
    /// someone ran `docker run --name coxagent-db … -p 8101:…` directly — is
    /// protected exactly like its compose counterpart, not stopped by id; the
    /// casing-spoof cases pin that the same lowercase-normalized policy guards
    /// the raw-name path, not just the compose-label path.
    #[test]
    fn raw_container_named_like_protected_infra_is_not_stopped() {
        for protected in [
            "coxagent-hub",
            "coxagent-db",
            "cox-infra-redis",
            "COXAGENT",
            "CoxAgent-Gateway",
            "COX-INFRA",
        ] {
            assert_eq!(
                decide_holder(&holder("", protected), "abc".to_owned()),
                None,
                "{protected} must never be stopped even as a raw container"
            );
        }
    }

    /// CXA-B082 AC: a foreign non-cox compose project holding :8101 must NEVER be
    /// torn down by this gate. `decide_holder` refuses to classify it for eviction,
    /// so nothing of theirs is stopped.
    #[test]
    fn foreign_non_preview_project_is_left_to_itself_not_destroyed() {
        for foreign in ["someone-elses-stack", "cxa-backend", "myapp-prod"] {
            assert_eq!(
                decide_holder(&holder(foreign, "ignored"), "abc".to_owned()),
                None,
                "{foreign} must never be evicted"
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
                decide_holder(&holder(owner, "ignored"), "abc".to_owned()),
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
            id_with("cox--other-worktree", "ignored"),
            id_with("", "cox--stale-raw"),
            id_with("cox-cxa-codebase", "ignored"),
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
        let holders = vec![id_with("someone-elses-stack", "ignored")];
        assert_eq!(
            plan_holders(&holders),
            PortPlan::Skip("someone-elses-stack".to_owned())
        );
    }

    /// CXA-B083 AC: an anonymous/foreign raw container squatting :8101 blocks
    /// the run the same way a foreign project does — named in a skip, never
    /// stopped, and no co-squatter is evicted around it either.
    #[test]
    fn anonymous_raw_container_blocking_port_is_reported_as_a_skip() {
        let alone = vec![id_with("", "nginx")];
        assert_eq!(plan_holders(&alone), PortPlan::Skip("nginx".to_owned()));

        let mixed = vec![
            id_with("", "nginx"),
            id_with("cox--stale-preview", "ignored"),
            id_with("", "cox--stale-raw"),
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
    #[test]
    fn one_protected_or_mixed_blocking_holder_wins_over_every_reclaimable() {
        for (foreign, reclaimable_present) in [
            ("cxa-backend", false),
            ("myapp-prod", true),
            ("coxagent-db", true),
        ] {
            let mut holders = vec![id_with(foreign, "ignored")];
            if reclaimable_present {
                holders.push(id_with("cox--stale-preview", "ignored"));
                // Skip must also short-circuit any raw-container teardown that
                // the cox-- name would otherwise schedule.
                holders.push(id_with("", "cox--stale-raw"));
            }
            assert_eq!(
                plan_holders(&holders),
                PortPlan::Skip(foreign.to_owned()),
                "{foreign} should block verification without evicting co-squatters"
            );
        }
    }

    /// A holder that vanished (or left nothing recoverable) between `ps` and
    /// `inspect` is neither evicted nor blocking — the run just proceeds.
    #[test]
    fn unidentifiable_holder_is_skipped_not_evicted() {
        let gone: Vec<(String, Option<HolderIdentity>)> = vec![("gone".to_owned(), None)];
        assert_eq!(plan_holders(&gone), PortPlan::EvictThenVerify(vec![]));

        let mixed: Vec<(String, Option<HolderIdentity>)> = vec![
            ("gone".to_owned(), None),
            id_with("cox--stale-preview", "ignored"),
        ];
        assert_eq!(
            plan_holders(&mixed),
            PortPlan::EvictThenVerify(vec![HolderDecision::EvictComposeProject(
                "cox--stale-preview".to_owned()
            )])
        );
    }
}

/// `HolderIdentity::new` is the one place raw inspect output is turned into a
/// normalized decision input; pin its splitting, trimming and slash-stripping so
/// silent breakage of that parsing can never resurface as an eviction bug.
#[cfg(test)]
mod parse_tests {
    use super::{inspect_template, HolderIdentity, IDENT_SEP};

    #[test]
    fn splits_project_and_strips_leading_slash_from_name() {
        // Mirrors real `docker inspect --format` stdout: label, separator,
        // then a docker-style `/name` with the trailing newline docker appends.
        let raw = format!("cox--slot-a-hub{IDENT_SEP}/hub_1\n");
        let parsed = HolderIdentity::new(&raw);
        assert!(parsed.is_some(), "both fields present");
        let identity = parsed.unwrap();
        assert_eq!(identity.owner_project, "cox--slot-a-hub");
        assert_eq!(identity.container_name, "hub_1");
    }

    #[test]
    fn unlabelled_holder_yields_only_a_name() {
        let parsed = HolderIdentity::new(&format!("{IDENT_SEP}/nginx\n"));
        assert!(parsed.is_some(), "name present");
        let identity = parsed.unwrap();
        assert!(identity.owner_project.is_empty());
        assert_eq!(identity.container_name, "nginx");
    }

    #[test]
    fn labelled_holder_with_no_recoverable_name_is_still_some() {
        let parsed = HolderIdentity::new(&format!("someone-elses-stack{IDENT_SEP}\n"));
        assert!(parsed.is_some(), "label present");
        let identity = parsed.unwrap();
        assert_eq!(identity.owner_project, "someone-elses-stack");
        assert!(identity.container_name.is_empty());
    }

    #[test]
    fn anonymous_inspect_output_yields_no_identity() {
        // Both fields empty → nothing to judge ownership from → declined entirely.
        assert!(HolderIdentity::new("\n").is_none());
    }

    /// The separator must reach docker as a real control char. The original
    /// CXA-B083 branch interpolated `{IDENT_SEP}` inside a plain string literal,
    /// where it is NOT expanded — docker echoed the literal text back, no split
    /// ever happened, and every eviction decision silently degraded.
    #[test]
    fn inspect_template_embeds_the_real_separator() {
        let template = inspect_template();
        assert!(
            template.contains(IDENT_SEP),
            "IDENT_SEP must be embedded as the actual control char"
        );
        assert!(
            !template.contains("{IDENT_SEP}"),
            "the literal placeholder text must never reach docker"
        );
    }
}
