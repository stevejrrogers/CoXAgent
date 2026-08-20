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
/// namespace is foreign and left alone; raw non-compose containers are stopped by id.
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
/// leaves a container holding the host port.
struct Stack {
    root: PathBuf,
}

impl Stack {
    /// Claims host port HOST_PORT for THIS build's verification pass.
    ///
    /// Tears down whatever currently holds the port before `up` — both anything
    /// left under this directory's compose project by an earlier aborted run and
    /// any FOREIGN agent-preview stack squatting :8101 (see [`clear_host_port`]) —
    /// so the probe below can only ever reach an image freshly built from THIS
    /// tree, never a stale unrelated container answering 200 there. Symmetric with
    /// [`Drop`]; each claim starts from nothing.
    fn claim(root: PathBuf) -> Self {
        let stack = Self { root };
        clear_host_port();
        stack.down();
        stack
    }

    fn down(&self) {
        let _ = compose(&self.root, &["down", "-v"]);
    }
}

/// What an automated pass should do about one container squatting HOST_PORT,
/// decided purely over its recoverable identifiers so every branch is
/// deterministically testable without invoking docker.
#[derive(Debug, PartialEq)]
enum Eviction {
    /// The holder belongs to a reclaimable agent-preview compose project —
    /// tear that whole project down to release the port cleanly.
    ComposeProject(String),
    /// An unlabelled holder positively identified as OUR OWN reclaimable
    /// agent-preview (`docker run --name cox--…`) — stop just it by id.
    RawContainer(String),
}

/// The field separator between recoverable identifiers in one inspect pass —
/// a control char no docker identifier can contain.
const IDENT_SEP: char = '\u{1e}';

/// Recoverable ownership identifiers of one running holder, fetched together so
/// classification needs exactly one `docker inspect` per squatter even when the
/// compose-project label is absent (the raw `docker run` case this guard exists
/// for).
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
fn classify_holder(identity: &HolderIdentity, id: String) -> Option<Eviction> {
    if !identity.owner_project.is_empty() {
        return if reclaimable(&identity.owner_project) {
            Some(Eviction::ComposeProject(identity.owner_project.clone()))
        } else {
            // A protected/foreign compose project is never touched; there may be
            // another squatter sharing :8101 (IPv4+IPv6), so this returns only
            // this one's verdict.
            None
        };
    }
    // `container_name` is already normalized (trimmed, leading `/` stripped)
    // by `HolderIdentity::new`; an empty value means no usable name to judge.
    if identity.container_name.is_empty() {
        // No owner label and no usable name → cannot prove ownership → leave alone.
        return None;
    }
    if reclaimable(&identity.container_name) {
        Some(Eviction::RawContainer(id))
    } else {
        None
    }
}

fn execute_eviction(action: Eviction) {
    match action {
        Eviction::ComposeProject(project) => {
            let _ = Command::new("docker")
                .args(["compose", "-p", &project, "down", "--remove-orphans"])
                .output();
        }
        Eviction::RawContainer(id) => {
            let _ = Command::new("docker").args(["stop", &id]).output();
        }
    }
}

fn clear_host_port() {
    for id in holders_on_host_port() {
        let Some(identity) = inspect_holder(&id) else {
            continue;
        };
        if let Some(action) = classify_holder(&identity, id.clone()) {
            execute_eviction(action);
        }
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

/// The eviction decision is pure over the recovered identity; verify every
/// branch without invoking docker, and pin that protected infrastructure can
/// never be classified as downed — a regression here would be a self-inflicted
/// outage (this module's stated intent: 'the live hub ... NEVER touched').
#[cfg(test)]
mod classify_tests {
    use super::{classify_holder, Eviction, HolderIdentity};

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

    #[test]
    fn reclaimable_agent_preview_is_torn_down_as_a_project() {
        assert_eq!(
            classify_holder(&holder("cox--other-worktree", "ignored"), "abc".to_owned()),
            Some(Eviction::ComposeProject("cox--other-worktree".to_owned()))
        );
    }

    #[test]
    fn foreign_non_preview_project_is_left_alone() {
        assert_eq!(
            classify_holder(&holder("someone-elses-stack", "ignored"), "abc".to_owned()),
            None
        );
    }

    #[test]
    fn live_hub_and_infra_prefixes_are_never_touched() {
        // The same casing-spoofing set production deploy's shared policy guards
        // against (reclaimable.rs), mirrored here so drift can never reintroduce
        // an automated teardown of the control plane.
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
                classify_holder(&holder(owner, "ignored"), "abc".to_owned()),
                None,
                "{owner} must never be evicted"
            );
        }
    }

    #[test]
    fn reclaimable_raw_preview_container_is_stopped_by_id() {
        // An unlabelled `docker run --name cox--slot-b-hub -p 8101:… …` left by an
        // earlier aborted run IS ours to clear.
        assert_eq!(
            classify_holder(&holder("", "cox--slot-b-hub"), "deadbeef".to_owned()),
            Some(Eviction::RawContainer("deadbeef".to_owned()))
        );
    }

    #[test]
    fn anonymous_raw_container_is_left_strictly_alone() {
        // CXA-B083 regression guard: an unlabelled holder with no recoverable
        // identity at all must NOT be force-stopped by id. This is exactly what a
        // bare `docker run -p 8101:<port> <image>` produces once we stop reading a
        // name where none exists.
        let anonymous = HolderIdentity {
            owner_project: String::new(),
            container_name: String::new(),
        };
        assert_eq!(classify_holder(&anonymous, "deadbeef".to_owned()), None);
    }

    #[test]
    fn foreign_raw_container_is_not_stopped_by_id() {
        // The REPRO from CXA-B083 verbatim: `docker run -d -p 8101:80 nginx`
        // squats :8101 with no compose label and no agent-owned name. It is not
        // ours — it must survive untouched instead of being stopped by id.
        for foreign in ["nginx", "my-app", "someone-svc"] {
            assert_eq!(
                classify_holder(&holder("", foreign), "abc".to_owned()),
                None,
                "{foreign} is not ours and must never be stopped"
            );
        }
    }

    #[test]
    fn raw_container_named_like_protected_infra_is_not_stopped() {
        // Even an unlabelled container whose NAME claims our control plane —
        // e.g. someone ran `docker run --name coxagent-db … -p 8101:` directly —
        // is protected exactly like its compose counterpart, not stopped by id.
        for protected in ["coxagent-hub", "coxagent-db", "cox-infra-redis"] {
            assert_eq!(
                classify_holder(&holder("", protected), "abc".to_owned()),
                None,
                "{protected} must never be stopped even as a raw container"
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
        let parsed =
            HolderIdentity::new(&format!("someone-elses-stack{IDENT_SEP}\n"));
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
