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
/// outage, and they bind port 4000 anyway (see AGENTS.md). Anything outside our
/// own namespace is foreign and left alone; raw non-compose containers are
/// stopped by id only when their container NAME itself proves them ours
/// (CXA-B083).
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
/// a pure function of that container's recovered ownership identity so every
/// branch is deterministically testable without invoking docker (CXA-B082/B083).
#[derive(Debug, PartialEq)]
enum HolderDecision {
    /// Belongs to a reclaimable agent-preview compose project — tear that whole
    /// project down to release its ports cleanly.
    EvictComposeProject(String),
    /// An unlabelled holder whose NAME proves it to be our own reclaimable
    /// agent-preview (`docker run --name cox--…`) — stop just it by id.
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

/// The field separator between recoverable identifiers in one inspect pass —
/// a control char no docker identifier can contain.
const IDENT_SEP: char = '\u{1e}';

/// Recoverable ownership identifiers of one running holder, fetched together so
/// classification needs exactly one `docker inspect` per squatter even when the
/// compose-project label is absent (the raw `docker run` case CXA-B083 closed).
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

fn inspect_holder(id: &str) -> Option<HolderIdentity> {
    Command::new("docker")
        .args([
            "inspect",
            "--format",
            "{{ index .Config.Labels \"com.docker.compose.project\" }}{IDENT_SEP}{{ index .Name }}",
            id,
        ])
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
/// The single source of truth for which *names* are ours is [`reclaimable`]
/// (itself mirroring production's `reclaimable_compose_project`, per the note on
/// that fn). That predicate is applied here to whichever identity field actually
/// carries an ownership claim:
///
/// * A compose holder (`owner_project` set) — evictable iff its project is a
///   reclaimable agent-preview; a protected or foreign project is never touched.
/// * An unlabelled raw holder (`owner_project` empty) — only evicted by id if
///   its container **name** itself proves it to be a reclaimable agent-preview.
///   This closes CXA-B083: previously any raw container on :8101 was stopped by
///   id regardless of what it was, so a live hub / db / foreign service launched
///   via plain `docker run -p 8101:…` could be silently killed. Now anonymous and
///   foreign holders — and anything named like our protected control plane — are
///   left strictly alone.
///
/// A `None` verdict never drops a holder silently: [`plan_holders`] turns every
/// non-evictable holder into a skip-with-report (CXA-B082) instead of failing
/// red or touching their stack.
fn decide_holder(identity: Option<&HolderIdentity>, id: String) -> Option<HolderDecision> {
    let Some(identity) = identity else {
        // Inspect gave nothing recoverable — ownership cannot be proven, so the
        // holder is declined (and later reported, never touched).
        return None;
    };
    if !identity.owner_project.is_empty() {
        return if reclaimable(&identity.owner_project) {
            Some(HolderDecision::EvictComposeProject(
                identity.owner_project.clone(),
            ))
        } else {
            // A protected/foreign compose project is never touched.
            None
        };
    }
    // Unlabelled raw holder: `container_name` is already normalized (trimmed,
    // leading `/` stripped) by `HolderIdentity::new`; an empty value — like any
    // non-agent name — fails `reclaimable`, so the holder is left alone.
    if reclaimable(&identity.container_name) {
        Some(HolderDecision::EvictRawContainer(id))
    } else {
        None
    }
}

/// Whether this smoke gate may bind HOST_PORT and verify this build's stack.
#[derive(Debug, PartialEq)]
enum PortState {
    /// Every holder on :8101 is evictable — free them and verify normally.
    Verifiable,
    /// A protected or foreign holder (compose project or raw container) holds
    /// :8101; we refuse to destroy unrelated user infrastructure (CXA-B082
    /// re-scope), and because HOST_PORT is fixed we cannot bind either.
    /// Verification skips with this holder named instead of failing red or
    /// tearing their stack down.
    BlockedByForeign(String),
}

/// What this smoke gate plans to do about :8101 — computed PURELY from one
/// immutable host-port snapshot so every branch is deterministically testable
/// without invoking docker (CXA-B082). Each entry is `(container id, recovered
/// identity)` for one container publishing HOST_PORT.
#[derive(Debug, PartialEq)]
enum PortPlan {
    /// Every holder on :8101 may be reclaimed — schedule those teardowns below,
    /// then verify normally.
    EvictThenVerify(Vec<HolderDecision>),
    /// A protected or foreign holder keeps :8101; refuse to destroy it, touch
    /// nothing else either (see [`plan_holders`]).
    Skip(String),
}

/// Best human-identifiable owner of a non-evictable holder for the skip
/// report: the compose project label when present, else the container name
/// (a raw squatter has no project), else the raw container id.
fn holder_report_name(identity: Option<&HolderIdentity>, id: &str) -> String {
    match identity {
        Some(i) if !i.owner_project.is_empty() => i.owner_project.clone(),
        Some(i) if !i.container_name.is_empty() => i.container_name.clone(),
        _ => id.to_owned(),
    }
}

/// Any single protected/foreign holder wins over every eviction below: even after
/// stopping reclaimable squatters another address-family duplicate held by a
/// protected project could keep :8101 bound, so once blocked we never risk a
/// half-cleared port or touch their stack — no teardown runs at all ([CXA-B082]
/// safest choice).
fn plan_holders(holders: &[(String, Option<HolderIdentity>)]) -> PortPlan {
    let mut blocked: Option<String> = None;
    let mut actions = Vec::new();
    for (id, identity) in holders {
        match decide_holder(identity.as_ref(), id.clone()) {
            None => {
                if blocked.is_none() {
                    blocked = Some(holder_report_name(identity.as_ref(), id));
                }
            }
            Some(action) => actions.push(action),
        }
    }
    match blocked {
        Some(owner) => PortPlan::Skip(owner),
        None => PortPlan::EvictThenVerify(actions),
    }
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

/// Exclusive, ephemeral ownership of the stack for one run: brought up here,
/// torn down when this value drops, even on panic, so a failing run never
/// leaves a container holding the host port. Only ever constructed on the
/// [`PortState::Verifiable`] path — when a protected/foreign holder keeps :8101
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

    // CXA-B082 re-scope + CXA-B083 name check: decide what owns :8101 BEFORE
    // binding anything. A protected or foreign holder (someone else's compose
    // stack — e.g. `cxa-backend`, a user's `myapp-prod` — or an unlabelled
    // container that cannot prove it is ours) is never destroyed, and because
    // HOST_PORT is fixed by our deploy config we cannot bind over it either; in
    // that case we skip verification with a clear report instead of failing red
    // or tearing unrelated infrastructure down. Reclaimable cox-preview
    // squatters — labelled projects and name-proven raw containers alike — are
    // still evicted here so normal runs verify in full, which always happens on
    // ephemeral CI runners where no foreign stack can exist.
    match assess_host_port() {
        PortState::BlockedByForeign(owner) => {
            eprintln!(
                "deploy-smoke SKIPPED: host port {HOST_PORT} is held by `{owner}`, \
                 which this gate refuses to tear down (CXA-B082). Verification \
                 could not bind without destroying unrelated infrastructure."
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

/// The holder-decision and port-plan logic is pure over the recovered identity;
/// verify every branch without invoking docker, and pin that protected
/// infrastructure can never be classified as downed — a regression here would
/// be a self-inflicted outage (CXA-B082 pins the skip-with-report contract,
/// CXA-B083 pins the raw-container name check).
#[cfg(test)]
mod decide_tests {
    use super::{decide_holder, plan_holders, HolderDecision, HolderIdentity, PortPlan};

    /// Build a holder from raw inspect fields: `owner_project` is
    /// `.Config.Labels["com.docker.compose.project"]` (empty for unlabelled raw
    /// containers); `container_name` mirrors docker's `.Name` after our own
    /// strip of its leading `/`.
    fn holder(owner_project: &str, container_name: &str) -> Option<HolderIdentity> {
        Some(HolderIdentity {
            owner_project: owner_project.to_owned(),
            container_name: container_name.to_owned(),
        })
    }

    fn snapshot(id: &str, identity: Option<HolderIdentity>) -> (String, Option<HolderIdentity>) {
        (id.to_owned(), identity)
    }

    #[test]
    fn reclaimable_agent_preview_is_torn_down_as_a_project() {
        assert_eq!(
            decide_holder(holder("cox--other-worktree", "ignored").as_ref(), "abc".to_owned()),
            Some(HolderDecision::EvictComposeProject("cox--other-worktree".to_owned()))
        );
    }

    /// CXA-B082 AC: a foreign non-cox compose project holding :8101 must NEVER be
    /// torn down by this gate. `decide_holder` refuses to classify it for eviction,
    /// so nothing of theirs is stopped.
    #[test]
    fn foreign_non_preview_project_is_left_to_itself_not_destroyed() {
        for foreign in ["someone-elses-stack", "cxa-backend", "myapp-prod"] {
            assert_eq!(decide_holder(holder(foreign, "ignored").as_ref(), "abc".to_owned()), None);
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
                decide_holder(holder(owner, "ignored").as_ref(), "abc".to_owned()),
                None,
                "{owner} must never be evicted"
            );
        }
    }

    /// An unlabelled `docker run --name cox--slot-b-hub -p 8101:… …` left by an
    /// earlier aborted run IS ours to clear — the name itself proves it.
    #[test]
    fn reclaimable_raw_preview_container_is_stopped_by_id() {
        assert_eq!(
            decide_holder(holder("", "cox--slot-b-hub").as_ref(), "deadbeef".to_owned()),
            Some(HolderDecision::EvictRawContainer("deadbeef".to_owned()))
        );
    }

    /// CXA-B083 regression guard: a raw holder we cannot prove to be ours is
    /// NEVER force-stopped by id. This replaces the old rule (any unlabelled
    /// container stopped by id) that let `docker run -p 8101:… nginx` kill
    /// unrelated services. Covers both "inspect gave nothing at all" and "a
    /// struct with no usable identity fields".
    #[test]
    fn unprovable_raw_holder_is_never_stopped_by_id() {
        assert_eq!(decide_holder(None, "deadbeef".to_owned()), None);
        let anonymous = HolderIdentity {
            owner_project: String::new(),
            container_name: String::new(),
        };
        assert_eq!(decide_holder(Some(&anonymous), "deadbeef".to_owned()), None);
    }

    /// The REPRO from CXA-B083 verbatim: `docker run -d -p 8101:80 nginx`
    /// squats :8101 with no compose label and no agent-owned name. It is not
    /// ours — it must survive untouched instead of being stopped by id.
    #[test]
    fn foreign_raw_container_is_not_stopped_by_id() {
        for foreign in ["nginx", "my-app", "someone-svc"] {
            assert_eq!(
                decide_holder(holder("", foreign).as_ref(), "abc".to_owned()),
                None,
                "{foreign} is not ours and must never be stopped"
            );
        }
    }

    /// Even an unlabelled container whose NAME claims our control plane —
    /// e.g. someone ran `docker run --name coxagent-db … -p 8101:` directly —
    /// is protected exactly like its compose counterpart, not stopped by id.
    #[test]
    fn raw_container_named_like_protected_infra_is_not_stopped() {
        for protected in ["coxagent-hub", "coxagent-db", "cox-infra-redis"] {
            assert_eq!(
                decide_holder(holder("", protected).as_ref(), "abc".to_owned()),
                None,
                "{protected} must never be stopped even as a raw container"
            );
        }
    }

    /// CXA-B082 AC (aggregation half): every holder evictable → schedule those
    /// teardowns and let verification proceed. A raw holder counts as evictable
    /// only when its name proves it ours (CXA-B083), so the raw entry here is a
    /// named cox-preview, not an anonymous container.
    #[test]
    fn only_reclaimable_holders_schedule_eviction() {
        let holders = vec![
            snapshot("abc", holder("cox--other-worktree", "ignored")),
            snapshot("def", holder("", "cox--stale-raw")),
            snapshot("ghi", holder("cox-cxa-codebase", "ignored")),
        ];
        assert_eq!(
            plan_holders(&holders),
            PortPlan::EvictThenVerify(vec![
                HolderDecision::EvictComposeProject("cox--other-worktree".to_owned()),
                HolderDecision::EvictRawContainer("def".to_owned()),
                HolderDecision::EvictComposeProject("cox-cxa-codebase".to_owned()),
            ])
        );
    }

    /// CXA-B082 AC: a foreign non-cox compose project holding :8101 must not make
    /// the gate fail red nor tear their stack down — it yields a skip with their
    /// owner named.
    #[test]
    fn foreign_project_blocking_port_is_reported_as_a_skip() {
        let holders = vec![snapshot("abc", holder("someone-elses-stack", "ignored"))];
        assert_eq!(
            plan_holders(&holders),
            PortPlan::Skip("someone-elses-stack".to_owned())
        );
    }

    /// CXA-B082's skip-with-report contract extended over CXA-B083's name check:
    /// a foreign raw container is not stopped AND blocks the run as a skip,
    /// reported by its container name since it has no project label.
    #[test]
    fn foreign_raw_container_blocks_as_a_skip_naming_the_container() {
        let holders = vec![snapshot("abc", holder("", "nginx"))];
        assert_eq!(plan_holders(&holders), PortPlan::Skip("nginx".to_owned()));
    }

    /// A holder with no recoverable identity at all (inspect failure or fully
    /// anonymous container) is likewise never touched and reported by id.
    #[test]
    fn anonymous_holder_blocks_as_a_skip_naming_the_id() {
        let holders = vec![snapshot("abc", None)];
        assert_eq!(plan_holders(&holders), PortPlan::Skip("abc".to_owned()));
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
            let mut holders = vec![snapshot("abc", holder(foreign, "ignored"))];
            if reclaimable_present {
                holders.push(snapshot("def", holder("cox--stale-preview", "ignored")));
                // Skip must also short-circuit any raw-container teardown
                // scheduled for name-proven cox previews.
                holders.push(snapshot("ghi", holder("", "cox--stale-raw")));
            }
            assert_eq!(
                plan_holders(&holders),
                PortPlan::Skip(foreign.to_owned()),
                "{foreign} should block verification without evicting co-squatters"
            );
        }
    }
}

/// `HolderIdentity::new` is the one place raw inspect output is turned into a
/// normalized decision input; pin its splitting, trimming and slash-stripping so
/// silent breakage of that parsing can never resurface as an eviction bug.
#[cfg(test)]
mod parse_tests {
    use super::{HolderIdentity, IDENT_SEP};

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
}
