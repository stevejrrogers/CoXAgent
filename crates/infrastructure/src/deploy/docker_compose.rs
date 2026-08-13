//! `DockerComposeDeploy` — deploys a codebase with `docker compose up -d
//! --build`. Skips gracefully (success, not deployed) when the project has no
//! compose file, so non-dockerised projects don't error the cycle.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{DeployPort, DeployReport};
use coxagent_application::PortError;
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

const COMPOSE_FILES: &[&str] = &[
    "docker-compose.yml",
    "docker-compose.yaml",
    "compose.yml",
    "compose.yaml",
];
const DEPLOY_TIMEOUT: Duration = Duration::from_secs(900);

/// Keys every secret-bearing compose file this adapter can meet requires via
/// `${VAR:?}` interpolation (COX-C012): the root docker-compose.yml needs both
/// or `up` dies before starting anything — the CXA-B010 symptom.
const REQUIRED_SECRET_KEYS: &[&str] = &["PG_PASSWORD", "COXAGENT_ADMIN_PASSWORD"];

/// A cryptographically-random 32-char fallback secret. Used ONLY when an
/// operator configured no value anywhere — never a baked constant, so no reader
/// of source can predict a deployment's superuser password (CXA-B017). The
/// alphabet drops look-alikes (0/O, 1/l/I) and any character that would break
/// YAML or a DSN URL.
fn random_secret() -> String {
    use rand::Rng;
    const CHARSET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    let mut rng = rand::thread_rng();
    let mut out = String::with_capacity(32);
    for _ in 0..32 {
        let idx = rng.gen_range(0..CHARSET.len());
        out.push(CHARSET[idx] as char);
    }
    out
}

/// Resolve which required secrets must be injected for one logical deploy pass.
///
/// Security rule (CXA-B017): an app-driven deploy must NEVER bake a public,
/// source-published credential into long-running services — anyone who read
/// `ci-verify-pg` / `ci-verify-admin` could log in as super on any deployment —
/// and must NEVER clobber a real secret an operator configured.
///
/// Compose resolves `${VAR:?}` from two sources in precedence order: the
/// process environment (which every subcommand inherits by default), then a
/// `.env` file in the compose project dir. Any key already resolvable from
/// EITHER is left entirely alone — we return nothing for it, so compose uses
/// the operator's own value and we can never override or poison it. Only keys
/// missing from BOTH sources get a fresh cryptographically-random fallback so
/// interpolation still resolves and services start without a known credential.
///
/// Computed ONCE per pass so an eviction retry re-runs identical interpolation
/// against any state the initial `up` already created.
/// Which required secret keys are NOT provided by the operator and therefore
/// need a random fallback seed for one deploy pass.
///
/// Pure over its inputs so all precedence branches are deterministically
/// unit-testable: `provided_by_env` answers "does this process already carry
/// `key` (non-blank)?" and `dot_env_keys` is whatever an operator's project-dir
/// `.env` configured. A key resolvable from EITHER source is left alone — we
/// never override or poison it; only keys missing from both get a fallback.
fn missing_required_secrets(
    provided_by_env: impl Fn(&str) -> bool,
    dot_env_keys: &std::collections::HashSet<String>,
) -> Vec<&'static str> {
    REQUIRED_SECRET_KEYS
        .iter()
        .copied()
        .filter(|key| !provided_by_env(key) && !dot_env_keys.contains(*key))
        .collect()
}

/// Stability rule (CXA-B031): once generated for a project, a fallback secret
/// never changes across deploy cycles. Operator-configured values (process env
/// or project-dir `.env`) are honoured verbatim and never overridden; only keys
/// missing from BOTH get a value, reused from this project's out-of-tree store
/// or freshly generated and persisted there - so an unconfigured app keeps one
/// stable superuser password instead of rotating it every pass (which breaks
/// Postgres auth and admin login once pgdata is initialized). Stored OUTSIDE any
/// project source tree (`deploy_secrets_root`), never written into the work dir's
/// `.env`, preserving CXA-B028's no-secret-in-source guarantee.
fn resolve_deploy_secrets(work_dir: &std::path::Path) -> Vec<(String, String)> {
    resolve_deploy_secrets_in(work_dir, &deploy_secrets_root())
}

fn resolve_deploy_secrets_in(
    work_dir: &std::path::Path,
    secret_root: &std::path::Path,
) -> Vec<(String, String)> {
    let dot_env = read_dot_env(&work_dir.join(".env"));
    let missing = missing_required_secrets(
        |key| std::env::var(key).is_ok_and(|v| !v.trim().is_empty()),
        &dot_env,
    );
    if missing.is_empty() {
        return Vec::new();
    }
    let mut stored = read_stored_secrets(&store_file(secret_root, work_dir));
    let mut resolved: Vec<(String, String)> = Vec::with_capacity(missing.len());
    for key in missing {
        let value = match stored.get(key) {
            Some(existing) => existing.clone(),
            None => random_secret(),
        };
        stored.insert(key.to_owned(), value.clone());
        resolved.push((key.to_owned(), value));
    }
    write_stored_secrets(&store_file(secret_root, work_dir), &stored);
    resolved
}

fn seed_deploy_secrets(cmd: &mut tokio::process::Command, secrets: &[(String, String)]) {
    for (key, value) in secrets {
        cmd.env(key.as_str(), value.as_str());
    }
}

/// Parse the KEY=VALUE assignments out of an operator-authored `.env` file
/// (the project-dir file docker compose reads automatically for interpolation).
/// Returns only which keys are assigned a non-blank value — we need presence,
/// never the secret itself. Mirrors compose's loose grammar closely enough to
/// avoid clobbering anything an operator actually configured: blank lines and
/// `#` comments are skipped; an optional leading `export` is tolerated; values
/// are split on the FIRST `=` so passwords containing `=` survive intact.
fn read_dot_env(path: &std::path::Path) -> std::collections::HashSet<String> {
    use std::collections::HashSet;
    let Ok(contents) = std::fs::read_to_string(path) else {
        return HashSet::new();
    };
    contents
        .lines()
        .filter_map(|raw| {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let line = line.strip_prefix("export ").unwrap_or(line);
            let (k, v) = line.split_once('=')?;
            let key = k.trim().to_owned();
            if key.is_empty() || v.trim().is_empty() {
                return None;
            }
            Some(key)
        })
        .collect()
}

/// Root directory for durable per-project deploy-secret state (CXA-B031).
///
/// Always OUTSIDE any project's source tree - never inside `<project>/codebase`
/// where agents read diffs from and commit from (CXA-B028). Defaults to a
/// per-user data dir, overridable with `COXAGENT_DEPLOY_SECRETS_DIR` so tests
/// and sandboxes can point it at an isolated location.
fn deploy_secrets_root() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("COXAGENT_DEPLOY_SECRETS_DIR") {
        return std::path::PathBuf::from(dir);
    }
    match std::env::var_os("HOME") {
        Some(home) => std::path::PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("coxagent")
            .join("deploy-secrets"),
        None => std::env::temp_dir().join("coxagent-deploy-secrets"),
    }
}

/// Path of one project's stored secrets file: a stable hash of the project's
/// compose project name ([`compose_project_name`]), NOT its absolute filesystem
/// path. Postgres data persists in docker NAMED volumes keyed by that compose
/// project name (`<project>_db`), which survive re-clones and relocations; an
/// unconfigured app-driven deploy regenerates its secrets when its store key is
/// based on where on disk it happens to live (CXA-B032). Keying by compose
/// project name makes secret stability track exactly what docker uses to persist
/// pgdata, so moving/cloning an app between hosts or checkouts reuses the same
/// PG_PASSWORD instead of resurrecting auth failure against an initialized volume.
///
/// The digest stays only a KEY - its values are still unguessable random secrets,
/// and two genuinely distinct projects keep separate store files.
fn store_file(secret_root: &std::path::Path, work_dir: &std::path::Path) -> std::path::PathBuf {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    // `compose_project_name` derives from basenames only (`cox-<parent>-<dir>`),
    // so it is invariant under relocation — exactly like docker's volume naming —
    // while still distinguishing otherwise-unrelated projects.
    hasher.update(compose_project_name(work_dir).as_bytes());
    secret_root.join(format!("{:x}.env", hasher.finalize()))
}

/// Read a project's previously generated secrets back out of the out-of-tree
/// store as `KEY -> value`. Absent or unreadable store = nothing persisted yet.
fn read_stored_secrets(path: &std::path::Path) -> std::collections::HashMap<String, String> {
    use std::{collections::HashMap, fs};
    let Ok(contents) = fs::read_to_string(path) else {
        return HashMap::new();
    };
    contents
        .lines()
        .filter_map(|raw| {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (k, v) = line.split_once('=')?;
            let key = k.trim().to_owned();
            (!key.is_empty() && !v.is_empty()).then_some((key, v.to_owned()))
        })
        .collect()
}

/// Persist a project's generated secrets into the out-of-tree store so later
/// deploy cycles reuse them ([resolve_deploy_secrets_in]). Writes atomically
/// via a temp sibling + rename so a crash mid-write can never leave a half-
/// written secret file that reads back as empty, and hardens permissions to
/// owner-only since these are credentials at rest.
fn write_stored_secrets(
    path: &std::path::Path,
    secrets: &std::collections::HashMap<String, String>,
) {
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let mut body = String::new();
    for (k, v) in secrets {
        body.push_str(k);
        body.push('=');
        body.push_str(v);
        body.push('\n');
    }
    // Temp sibling + rename keeps readers from ever observing partial content.
    let tmp = parent.join(format!(
        "{}.tmp-{}",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id()
    ));
    if std::fs::write(&tmp, body.as_bytes()).is_err() {
        return;
    }
    set_secret_perms(&tmp);
    // Best-effort store: never fail a deploy because we could not persist;
    // drop the temp so no stray secret file is left behind.
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Hardens an on-disk secret file to owner-only once written; best-effort so a
/// filesystem that cannot represent modes never fails a deploy.
#[cfg(unix)]
fn set_secret_perms(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt as _;
    if let Ok(meta) = std::fs::metadata(path) {
        meta.permissions().set_mode(0o600);
    }
}
#[cfg(not(unix))]
fn set_secret_perms(_path: &std::path::Path) {}

/// Pull the host port out of a compose bind error like
/// `Bind for 0.0.0.0:8100 failed: port is already allocated`.
fn extract_bind_port(err: &str) -> Option<String> {
    let idx = err.find("Bind for ")?;
    let rest = &err[idx + "Bind for ".len()..];
    let addr = rest.split_whitespace().next()?;
    Some(addr.rsplit(':').next()?.trim().to_owned())
}

/// The compose project name of whatever container currently publishes `port`
/// on this host, or `None` when the squatter isn't compose-managed (we never
/// evict arbitrary containers).
async fn compose_project_on_port(port: &str) -> Option<String> {
    let out = Command::new("docker")
        .args([
            "ps",
            "--filter",
            &format!("publish={port}"),
            "--format",
            "{{.Label \"com.docker.compose.project\"}}",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!name.is_empty()).then_some(name)
}

/// Whether the deploy may `down` a compose project that is squatting one of the
/// agent's host ports. This is the blast-radius guard for the port-eviction
/// self-heal: only agent-managed preview projects (`cox-{parent}-{dir}`) may be
/// evicted. The live hub (`coxagent` / `coxagent-*`) and shared infra
/// (`cox-infra`) are NEVER evocable — downing either is a self-inflicted outage,
/// exactly the class of bug where an agent deploy took the whole control plane
/// down trying to free a port it thought it owned.
fn evictable_project(project: &str) -> bool {
    // `compose_project_name` always yields `cox-<parent>-<dir>`, so an
    // evocable preview is recognisable by its `cox-` prefix — provided it is
    // NOT the live hub. `coxagent` and anything starting with `coxagent` (the
    // production project plus any of its service containers) are protected, as
    // is shared shared infrastructure (`cox-infra`).
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

/// Clamp every container of this compose project to a CPU/memory budget via
/// `docker update`, regardless of what the agent-authored compose file says —
/// a runaway service (busy loop, leak) can then never take the whole host.
/// Defaults: 2 CPUs, 1g memory. Override with `COXAGENT_DEPLOY_CPUS` /
/// `COXAGENT_DEPLOY_MEM`; set either to `off` to skip. Best-effort.
async fn apply_resource_limits(proj: &str) {
    let cpus = std::env::var("COXAGENT_DEPLOY_CPUS").unwrap_or_else(|_| "2".to_owned());
    let mem = std::env::var("COXAGENT_DEPLOY_MEM").unwrap_or_else(|_| "1g".to_owned());
    if cpus.eq_ignore_ascii_case("off") || mem.eq_ignore_ascii_case("off") {
        return;
    }
    let Ok(out) = Command::new("docker")
        .args(["compose", "-p", proj, "ps", "-q"])
        .stdin(std::process::Stdio::null())
        .output()
        .await
    else {
        return;
    };
    for id in String::from_utf8_lossy(&out.stdout).lines() {
        let id = id.trim();
        if id.is_empty() {
            continue;
        }
        let _ = Command::new("docker")
            .args([
                "update",
                "--cpus",
                &cpus,
                "--memory",
                &mem,
                "--memory-swap",
                &mem,
                id,
            ])
            .stdin(std::process::Stdio::null())
            .output()
            .await;
    }
}

/// The id of ANY container publishing `port` (compose-labelled or not).
async fn container_on_port(port: &str) -> Option<String> {
    let out = Command::new("docker")
        .args([
            "ps",
            "--filter",
            &format!("publish={port}"),
            "--format",
            "{{.ID}}",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    let id = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    (!id.is_empty()).then_some(id)
}

/// Deterministic compose project name for a deploy dir: `cox-<parent>-<dir>`
/// (sanitized). Ends the era of accidental project names like "116" or
/// "codebase" colliding/littering docker — every CoXAgent deploy is grouped
/// and identifiable, and the janitor can target the `cox-` prefix safely.
fn compose_project_name(work_dir: &Path) -> String {
    let comp = |o: Option<&std::ffi::OsStr>| {
        o.map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>()
    };
    let dir = comp(work_dir.file_name());
    let parent = comp(work_dir.parent().and_then(|p| p.file_name()));
    let mut name = format!("cox-{parent}-{dir}");
    name.truncate(60);
    name.trim_matches('-').to_owned()
}

async fn running_services(work_dir: &Path) -> Vec<String> {
    let proj = compose_project_name(work_dir);
    let Ok(out) = Command::new("docker")
        .args([
            "compose",
            "-p",
            &proj,
            "ps",
            "--services",
            "--status",
            "running",
        ])
        .current_dir(work_dir)
        .stdin(std::process::Stdio::null())
        .output()
        .await
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Deploys via the `docker` CLI.
#[derive(Default)]
pub struct DockerComposeDeploy;

impl DockerComposeDeploy {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// Whether the Docker daemon answers `docker info`.
async fn daemon_up() -> bool {
    Command::new("docker")
        .args(["info"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
}

/// Detect the project's toolchain and return its test command, or `None`.
fn test_command(work_dir: &Path) -> Option<(&'static str, Vec<&'static str>)> {
    let has = |f: &str| work_dir.join(f).exists();
    if has("Cargo.toml") {
        Some(("cargo", vec!["test", "--quiet"]))
    } else if has("go.mod") {
        Some(("go", vec!["test", "./..."]))
    } else if has("package.json") {
        Some(("npm", vec!["test", "--silent"]))
    } else if has("pyproject.toml") || has("pytest.ini") || has("requirements.txt") {
        Some(("pytest", vec!["-q"]))
    } else {
        None
    }
}

/// Whether a Linux C/C++ cross toolchain that can actually link native crates
/// (tree-sitter, ring build C in build.rs) exists on this host. Having a rustup
/// *target* installed is not enough — without a C compiler for that target,
/// `cargo check --target x86_64-unknown-linux-gnu` can only ever report those
/// crates' build-tool failure, never whether *this ticket* broke Linux.
/// Considers common glibc/musl names in PATH plus `/opt/homebrew/bin` and
/// `/usr/local/bin`, and explicit rust-style env overrides.
fn linux_c_toolchain_present() -> bool {
    use std::{
        os::unix::fs::PermissionsExt as _,
        path::{Path, PathBuf},
    };

    const NAMES: &[&str] = &[
        "x86_64-linux-gnu-gcc",
        "x86_64-linux-gnu-cc",
        "aarch64-linux-gnu-gcc",
        "x86_64-unknown-linux-musl-gcc",
        "musl-clang",
        "zig", // zig cc can drive a configured cross build when present
    ];
    if std::env::var("CARGO_BUILD_TARGET")
        .ok()
        .is_some_and(|v| v.contains("linux"))
    {
        return true;
    }
    let overrides = [
        "CC_x86_64_UNKNOWN_LINUX_GNU",
        "CC_aarch64_UNKNOWN_LINUX_GNU",
    ];
    if overrides.iter().any(|k| std::env::var(k).is_ok()) {
        return true;
    }
    let mut dirs: Vec<String> = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|d| !d.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    dirs.push("/opt/homebrew/bin".to_string());
    dirs.push("/usr/local/bin".to_string());
    for dir in &dirs {
        for name in NAMES.iter().copied() {
            let probe: PathBuf = Path::new(dir).join(name);
            if probe.exists()
                && probe
                    .metadata()
                    .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
            {
                return true;
            }
        }
    }
    false
}

#[async_trait]
impl DeployPort for DockerComposeDeploy {
    async fn lint(&self, work_dir: &Path) -> Result<Option<u64>, PortError> {
        // Rust-only for now: clippy's error count is the lint currency the
        // DoD gate compares against the project baseline.
        if !work_dir.join("Cargo.toml").exists() {
            return Ok(None);
        }
        let _slot = crate::proc::heavy_slot().await;
        let child = crate::proc::low_priority("cargo")
            .args(["clippy", "--workspace", "--all-targets", "--quiet"])
            .current_dir(work_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| PortError::Backend(format!("spawn clippy: {e}")))?;
        let leader = child.id();
        let Ok(out) =
            tokio::time::timeout(Duration::from_secs(600), child.wait_with_output()).await
        else {
            if let Some(pid) = leader {
                crate::proc::kill_group(pid);
            }
            return Err(PortError::Backend("clippy timed out".to_owned()));
        };
        let out = out.map_err(|e| PortError::Backend(format!("clippy wait: {e}")))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let count = text
            .lines()
            .filter(|l| l.trim_start().starts_with("error"))
            .count() as u64;
        Ok(Some(count))
    }

    async fn lint_report(
        &self,
        work_dir: &Path,
    ) -> Result<Option<coxagent_application::ports::outbound::LintReport>, PortError> {
        if !work_dir.join("Cargo.toml").exists() {
            return Ok(None);
        }
        let _slot = crate::proc::heavy_slot().await;
        let child = crate::proc::low_priority("cargo")
            .args(["clippy", "--workspace", "--all-targets", "--quiet"])
            .current_dir(work_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| PortError::Backend(format!("spawn clippy: {e}")))?;
        let leader = child.id();
        let Ok(out) =
            tokio::time::timeout(Duration::from_secs(600), child.wait_with_output()).await
        else {
            if let Some(pid) = leader {
                crate::proc::kill_group(pid);
            }
            return Err(PortError::Backend("clippy timed out".to_owned()));
        };
        let out = out.map_err(|e| PortError::Backend(format!("clippy wait: {e}")))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let (errors, files) = parse_clippy(&text);
        // A dozen error lines is plenty for a repair prompt; the agent can run
        // clippy itself for the rest.
        let sample = errors
            .iter()
            .take(12)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        Ok(Some(coxagent_application::ports::outbound::LintReport {
            errors: errors.len() as u64,
            sample,
            files,
        }))
    }

    async fn cross_target_check(
        &self,
        work_dir: &Path,
    ) -> Result<coxagent_application::ports::outbound::CrossCheck, PortError> {
        use coxagent_application::ports::outbound::CrossCheck;
        const TARGET: &str = "x86_64-unknown-linux-gnu";
        if !work_dir.join("Cargo.toml").exists() {
            return Ok(CrossCheck {
                available: false,
                reason: "not a cargo project".to_owned(),
                errors: Vec::new(),
            });
        }
        // Self-provision rather than wait to be told. A capability the check
        // needs and can install itself is not a reason to skip verifying — that
        // silence is exactly how the same Linux-only dead_code error got filed
        // three times while every gate on this macOS host stayed green.
        let mut installed = linux_target_installed(TARGET).await;
        if !installed {
            let added = Command::new("rustup")
                .args(["target", "add", TARGET])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await
                .is_ok_and(|s| s.success());
            if added {
                installed = linux_target_installed(TARGET).await;
                if installed {
                    tracing::info!("cross-check installed the {TARGET} target itself");
                }
            }
        }
        if !installed {
            // No rustup to extend — but Docker compiles for Linux by
            // definition, and it is the build that actually breaks. Use it.
            if let Some(check) = compose_build_check(work_dir).await {
                return Ok(check);
            }
            return Ok(CrossCheck {
                available: false,
                reason: format!(
                    "cannot verify the {TARGET} build: `rustup target add {TARGET}` did not \
                     succeed (rustup may be absent) and no Docker build is available here. Until \
                     one of them exists, a symbol that is dead code on Linux compiles clean on \
                     this host and only breaks in Docker/CI."
                ),
                errors: Vec::new(),
            });
        }
        // The rustup *target* is installed, but that is not enough to verify
        // this ticket's code against native crates: tree-sitter/ring build C in
        // build.rs and need a real Linux C cross toolchain, which this macOS
        // host does not have. Running cargo check here would fail on those
        // crates' tool-not-found every single time — an infra gap, not evidence
        // about this ticket. Prefer a Docker answer; otherwise degrade honestly
        // to unavailable so run_dev warns instead of hard-blocking every ticket.
        if !linux_c_toolchain_present() {
            if let Some(check) = compose_build_check(work_dir).await {
                return Ok(check);
            }
            return Ok(CrossCheck {
                available: false,
                reason: format!(
                    "cannot verify {TARGET}: rustup target present but no Linux C/cross \
                     toolchain (x86_64-linux-gnu-gcc / x86_64-unknown-linux-musl-gcc / \
                     musl-clang) on this host — native crates cannot build without it"
                ),
                errors: Vec::new(),
            });
        }
        let _slot = crate::proc::heavy_slot().await;
        let child = crate::proc::low_priority("cargo")
            .args(["check", "--workspace", "--all-targets", "--target", TARGET])
            .current_dir(work_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| PortError::Backend(format!("spawn cargo check: {e}")))?;
        let leader = child.id();
        let Ok(out) =
            tokio::time::timeout(Duration::from_secs(900), child.wait_with_output()).await
        else {
            if let Some(pid) = leader {
                crate::proc::kill_group(pid);
            }
            return Err(PortError::Backend(
                "cross-target check timed out".to_owned(),
            ));
        };
        let out = out.map_err(|e| PortError::Backend(format!("cargo check wait: {e}")))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let (errors, _files) = parse_clippy(&text);
        Ok(CrossCheck {
            available: true,
            reason: String::new(),
            errors: errors.into_iter().take(12).collect(),
        })
    }

    async fn run_tests_scoped(
        &self,
        work_dir: &Path,
        changed: &[String],
    ) -> Result<DeployReport, PortError> {
        // Narrow when we can prove what the change can reach; otherwise this
        // IS the full run. See deploy::scoped_tests for the rules.
        let Some((cmd, args)) = crate::deploy::scoped_tests::scoped_test_command(work_dir, changed)
        else {
            return self.run_tests(work_dir).await;
        };
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.run_test_command(work_dir, &cmd, &refs).await
    }

    async fn run_tests(&self, work_dir: &Path) -> Result<DeployReport, PortError> {
        let Some((cmd, args)) = test_command(work_dir) else {
            return Ok(DeployReport {
                success: true,
                deployed: false,
                summary: "no recognised test runner".to_owned(),
            });
        };
        self.run_test_command(work_dir, cmd, &args).await
    }

    async fn down(&self, work_dir: &Path) -> Result<(), PortError> {
        let proj = compose_project_name(work_dir);
        let _ = Command::new("docker")
            .args(["compose", "-p", &proj, "down", "--remove-orphans"])
            .current_dir(work_dir)
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| PortError::Backend(format!("compose down: {e}")))?;
        Ok(())
    }

    async fn health(&self, port: u16) -> Result<bool, PortError> {
        // Something accepting TCP on the published port = the app is up.
        let addr = format!("127.0.0.1:{port}");
        let ok = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            tokio::net::TcpStream::connect(&addr),
        )
        .await
        .is_ok_and(|r| r.is_ok());
        Ok(ok)
    }

    /// COX-F005: an HTTP GET against the app's root, unlike [`Self::health`]
    /// this captures the HTTP status and response time so a deploy attempt's
    /// health outcome can be recorded in full, not just as a bool. Bounded by
    /// a fixed per-probe timeout — connection refused, DNS failure, and a
    /// wedged connection are all reported as `passed: false`, never left
    /// hanging.
    async fn health_check(&self, port: u16) -> coxagent_application::state::HealthCheckResult {
        let url = format!("http://127.0.0.1:{port}/");
        let start = std::time::Instant::now();
        let Ok(client) = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        else {
            return coxagent_application::state::HealthCheckResult {
                passed: false,
                http_status: None,
                response_time_ms: None,
            };
        };
        match client.get(&url).send().await {
            Ok(resp) => {
                let status = resp.status();
                // Any answer that isn't a 5xx means the app bound the port and
                // is serving — which is exactly what this gate exists to prove
                // (COX-B001: compose exits 0 while the app inside never
                // listens). Deployed projects are arbitrary, so demanding a 2xx
                // at `/` would roll back every app that simply has no root
                // route; a 5xx, by contrast, is a genuinely broken app.
                coxagent_application::state::HealthCheckResult {
                    passed: !status.is_server_error(),
                    http_status: Some(status.as_u16()),
                    response_time_ms: Some(
                        u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                    ),
                }
            }
            Err(_) => coxagent_application::state::HealthCheckResult {
                passed: false,
                http_status: None,
                response_time_ms: Some(
                    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                ),
            },
        }
    }

    async fn ensure_daemon(&self) -> Result<bool, PortError> {
        if daemon_up().await {
            return Ok(true);
        }
        // Try to start it: Docker Desktop on macOS, systemd on Linux.
        #[cfg(target_os = "macos")]
        let _ = Command::new("open")
            .args(["-a", "Docker"])
            .stdin(std::process::Stdio::null())
            .status()
            .await;
        #[cfg(target_os = "linux")]
        let _ = Command::new("systemctl")
            .args(["start", "docker"])
            .stdin(std::process::Stdio::null())
            .status()
            .await;
        // Poll for it to come up (Docker Desktop can take a while to boot).
        for _ in 0..30 {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if daemon_up().await {
                return Ok(true);
            }
        }
        Ok(false)
    }

    // Linear up → evict-retry → summarize pass; splitting it would obscure it.
    #[allow(clippy::too_many_lines)]
    async fn deploy(&self, work_dir: &Path) -> Result<DeployReport, PortError> {
        if !COMPOSE_FILES.iter().any(|f| work_dir.join(f).exists()) {
            return Ok(DeployReport {
                success: true,
                deployed: false,
                summary: "no compose file — deploy skipped".to_owned(),
            });
        }

        // Bring the previous stack down first (best-effort). Without this, a
        // still-running container from a prior cycle keeps holding its host
        // ports, so `up` fails with "port is already allocated" — a recurring,
        // self-inflicted deploy blocker. `down --remove-orphans` releases the
        // project's own ports (and orphaned services) so `up` starts clean.
        let proj = compose_project_name(work_dir);
        // Safety: `compose_project_name` always yields `cox-<parent>-<dir>`,
        // but double-check it can never collide with the live hub project
        // before we `down --remove-orphans` anything.
        if !evictable_project(&proj) {
            return Err(PortError::Backend(format!(
                "refusing to deploy project `{proj}` — collides with the live hub"
            )));
        }
        let _ = Command::new("docker")
            .args(["compose", "-p", &proj, "down", "--remove-orphans"])
            .current_dir(work_dir)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output()
            .await;

        // Secret-bearing compose files use `${VAR:?}` (COX-C012) and fail
        // interpolation without a value. Resolve them ONCE for this deploy pass:
        // an operator-supplied PG_PASSWORD / COXAGENT_ADMIN_PASSWORD (process env
        // or a project-dir `.env`) is honoured verbatim, and only when none was
        // configured anywhere does a fresh random value get used — so services
        // still start but no public known credential ever boots them (CXA-B017 —
        // never bake source-published constants into real deploys).
        let secrets = resolve_deploy_secrets(work_dir);

        // Compose builds are as heavy as test suites — same host-wide gate.
        let _slot = crate::proc::heavy_slot().await;
        let mut cmd = Command::new("docker");
        cmd.arg("compose")
            .args(["-p", &proj])
            .arg("up")
            .arg("-d")
            .arg("--build")
            // Secret-bearing compose files use `${VAR:?}` (COX-C012) and fail
            // interpolation without a value; seed ephemeral verification-only
            // ones so an automated deploy of this repo survives its own compose
            // file's required env sections (CXA-B010).
            .current_dir(work_dir)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        seed_deploy_secrets(&mut cmd, &secrets);

        let mut output = tokio::time::timeout(DEPLOY_TIMEOUT, cmd.output())
            .await
            .map_err(|_| PortError::Backend("docker compose timed out".to_owned()))?
            .map_err(|e| PortError::Backend(format!("spawn docker: {e}")))?;

        // Self-heal a squatted port: if `up` failed because the host port is
        // held by ANOTHER compose project on this machine (typically a stale
        // PR preview or an old build with a different project name), evict that
        // project and retry once — instead of failing every cycle until a human
        // notices.
        let mut evicted = None;
        // Up to two eviction+retry rounds: round 1 handles a stale compose
        // project; round 2 (or when no compose label exists) stops whatever
        // raw container is squatting the port. Docker also needs a beat to
        // release a freshly-stopped binding, hence the short sleep.
        for round in 0..2u8 {
            if output.status.success() {
                break;
            }
            let err = String::from_utf8_lossy(&output.stderr).to_string();
            if !err.contains("port is already allocated") {
                break;
            }
            let Some(port) = extract_bind_port(&err) else {
                break;
            };
            if let Some(project) = compose_project_on_port(&port).await {
                // Blast-radius guard: never `down` the live hub or shared infra
                // to free the port — that is a self-inflicted outage, not a
                // port eviction. Only agent preview projects (`cox-...`) are
                // evictable; anything else is reported as a collision.
                if !evictable_project(&project) {
                    break;
                }
                let _ = Command::new("docker")
                    .args(["compose", "-p", &project, "down", "--remove-orphans"])
                    .stdin(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .output()
                    .await;
                evicted = Some(format!("compose project `{project}`"));
            } else if let Some(id) = container_on_port(&port).await {
                let _ = Command::new("docker")
                    .args(["stop", &id])
                    .stdin(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .output()
                    .await;
                evicted = Some(format!("container `{id}`"));
            } else if round > 0 {
                break; // nothing visible holds the port — give up, report
            }
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let mut retry = Command::new("docker");
            retry
                .args(["compose", "-p", &proj, "up", "-d", "--build"])
                // Same ephemeral secrets as the initial `up` — a port-eviction
                // retry re-runs the same interpolation (CXA-B010).
                .current_dir(work_dir)
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true);
            seed_deploy_secrets(&mut retry, &secrets);
            output = tokio::time::timeout(DEPLOY_TIMEOUT, retry.output())
                .await
                .map_err(|_| PortError::Backend("docker compose timed out".to_owned()))?
                .map_err(|e| PortError::Backend(format!("spawn docker: {e}")))?;
        }

        let success = output.status.success();
        if success {
            apply_resource_limits(&proj).await;
        }
        let summary = if success {
            let note = evicted
                .map(|p| format!(" (evicted stale {p} off the port)"))
                .unwrap_or_default();
            match running_services(work_dir).await {
                services if !services.is_empty() => {
                    format!("running: {}{note}", services.join(", "))
                }
                _ => format!("docker compose up -d --build succeeded{note}"),
            }
        } else {
            // Compose prints the real cause somewhere in stderr, but the final
            // line is often blank; surface the last *non-empty* line (falling
            // back to stdout, then the exit code) so the reason is never empty.
            let err = String::from_utf8_lossy(&output.stderr);
            let out = String::from_utf8_lossy(&output.stdout);
            let last_meaningful = |s: &str| -> Option<String> {
                s.lines()
                    .map(str::trim)
                    .rev()
                    .find(|l| !l.is_empty())
                    .map(ToOwned::to_owned)
            };
            let detail = last_meaningful(&err)
                .or_else(|| last_meaningful(&out))
                .unwrap_or_else(|| {
                    format!("exit {} (no output)", output.status.code().unwrap_or(-1))
                });
            format!("docker compose failed: {detail}")
        };
        Ok(DeployReport {
            success,
            deployed: true,
            summary,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The port-eviction blast-radius guard: agent preview projects are
    /// evictable, the live hub and shared infra are not.
    #[test]
    fn evictable_project_protects_the_live_hub_and_infra() {
        assert!(
            evictable_project("cox-cxa-codebase"),
            "agent preview evictable"
        );
        assert!(
            evictable_project("cox-my-project-preview"),
            "any cox-<parent>-<dir> preview evictable"
        );
        // The live hub and anything sharing its prefix are NEVER evictable —
        // downing them is the self-inflicted outage we guard against.
        assert!(!evictable_project("coxagent"), "live hub protected");
        assert!(
            !evictable_project("coxagent-gateway"),
            "hub service protected"
        );
        assert!(!evictable_project("cox-infra"), "shared infra protected");
        assert!(
            !evictable_project("cox-infra-db"),
            "shared infra child protected"
        );
        // A non-preview project on our port is a collision, not an eviction.
        assert!(
            !evictable_project("someone-elses-stack"),
            "foreign project protected"
        );
    }

    /// Regression guard for CXA-B010 + CXA-B017: every site that runs compose
    /// against this repo's secret-bearing docker-compose.yml must seed
    /// PG_PASSWORD and COXAGENT_ADMIN_PASSWORD with valid, non-blank values. If
    /// either key is dropped or emptied, `up` dies at interpolation again with
    /// exactly the CXA-B010 symptom — so assert both keys are covered by
    /// REQUIRED_SECRET_KEYS, the single source that deploy/build seeding draws on.
    #[test]
    fn required_secret_keys_cover_both_required_compose_vars() {
        for required in ["PG_PASSWORD", "COXAGENT_ADMIN_PASSWORD"] {
            assert!(
                REQUIRED_SECRET_KEYS.contains(&required),
                "{required} must be seeded on compose commands (CXA-B010)"
            );
        }
        assert_eq!(
            REQUIRED_SECRET_KEYS.len(),
            2,
            "exactly the two required secrets expected"
        );
    }

    /// The precedence rule behind a deploy (CXA-B017): a key already provided by
    /// the process env OR by an operator-authored `.env` must be left alone;
    /// only keys missing from BOTH sources get a fallback seed. This is pure over
    /// its inputs, so each precedence branch is pinned down deterministically.
    #[test]
    fn missing_required_secrets_honours_env_then_dot_env() {
        let empty = std::collections::HashSet::new();
        let dot_env: std::collections::HashSet<String> =
            ["PG_PASSWORD".to_owned()].into_iter().collect();

        // Provided via process env → never fall back.
        assert_eq!(
            missing_required_secrets(|k| k == "PG_PASSWORD", &empty),
            vec!["COXAGENT_ADMIN_PASSWORD"]
        );
        // Provided via project-dir `.env` → never override or poison it.
        assert_eq!(
            missing_required_secrets(|_| false, &dot_env),
            vec!["COXAGENT_ADMIN_PASSWORD"]
        );
        // Missing from BOTH sources → both get a fresh random fallback.
        let all_missing = missing_required_secrets(|_| false, &empty);
        assert_eq!(all_missing.len(), 2);
    }

    /// AC (COX-F005): a health endpoint that's unreachable (nothing
    /// listening — connection refused) must be treated as a failed check,
    /// bounded by a timeout, never left hanging indefinitely.
    #[tokio::test]
    async fn unreachable_health_endpoint_is_a_bounded_failure_not_a_hang() {
        // No listener is ever bound to this port by this test.
        let unreachable_port = 65_533;

        let outcome = tokio::time::timeout(
            Duration::from_secs(35),
            DockerComposeDeploy.health_check(unreachable_port),
        )
        .await
        .expect(
            "a connection-refused health endpoint must not hang past the ~30s bound \
             (COX-F005)",
        );

        assert!(
            !outcome.passed,
            "connection refused must be reported as a failed health check, not a pass: \
             {outcome:?}"
        );
    }

    /// Regression test (COX-B018): the mandatory gate must poll to the
    /// configured timeout, not stop at one probe. `health_check` itself is a
    /// single bounded probe (a fixed 5s reqwest timeout) by design — it's
    /// `wait_healthy` (the trait's default) that turns it into a poll loop.
    /// This exercises the real adapter (not a scripted double): nothing is
    /// listening on the port for the first 8s (connection refused, same as a
    /// container whose app hasn't bound its port yet), then a real TCP
    /// listener comes up and answers 200 OK — the kind of cold-start delay a
    /// container doing DB migrations can have. 8s is longer than a single
    /// probe cycle but well inside the 20s bound below.
    #[tokio::test]
    async fn slow_starting_app_within_the_bound_passes_via_the_poll_loop() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        // Reserve a port, then release it so nothing answers on it yet.
        let probe = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = probe.local_addr().expect("addr").port();
        drop(probe);

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(8)).await;
            let listener = TcpListener::bind(("127.0.0.1", port))
                .await
                .expect("rebind once the app 'finishes starting'");
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = socket.read(&mut buf).await;
                    let _ = socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                        .await;
                });
            }
        });

        let result = DockerComposeDeploy
            .wait_healthy(port, Duration::from_secs(20))
            .await;

        assert!(
            result.passed,
            "an app that binds its port within the configured timeout must pass \
             the gate, even though earlier probes hit connection refused: {result:?}"
        );
    }
}

/// Whether rustup reports `target` as installed.
async fn linux_target_installed(target: &str) -> bool {
    Command::new("rustup")
        .args(["target", "list", "--installed"])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).contains(target))
}

/// Whether `stderr` shows compose aborting at variable interpolation rather
/// than at the actual build — e.g. `${PG_PASSWORD:?...}` when the operator
/// hasn't set the secret in this shell. That is an environment gap the check
/// runs in, not evidence the code fails to compile, so it must not be reported
/// as a build error. Returns the offending line when it matches.
fn interpolation_gap(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .find(|l| l.contains("error while interpolating") || l.contains("is missing a value"))
        .map(str::trim)
        .map(ToOwned::to_owned)
}

/// Build a `CrossCheck` reporting a genuine (non-interpolation) build failure
/// from a `docker ... build` command's stderr — shared by the compose-build
/// path and the Dockerfile fallback so the two don't drift.
fn build_failure_check(text: &str) -> coxagent_application::ports::outbound::CrossCheck {
    use coxagent_application::ports::outbound::CrossCheck;
    let (errors, _) = parse_clippy(text);
    CrossCheck {
        available: true,
        reason: "verified via the Docker (Linux) image build".to_owned(),
        errors: if errors.is_empty() {
            vec![text.lines().rev().take(3).collect::<Vec<_>>().join(" | ")]
        } else {
            errors.into_iter().take(12).collect()
        },
    }
}

/// Fallback cross-check: build the project's Docker image. It compiles for
/// Linux by definition, so it catches the same class of platform breakage
/// without a cross toolchain — and it is the build that actually fails in
/// production. `None` when there is no compose file or no daemon to run it.
async fn compose_build_check(
    work_dir: &Path,
) -> Option<coxagent_application::ports::outbound::CrossCheck> {
    use coxagent_application::ports::outbound::CrossCheck;
    if !COMPOSE_FILES.iter().any(|f| work_dir.join(f).exists()) || !daemon_up().await {
        return None;
    }
    let _slot = crate::proc::heavy_slot().await;
    let proj = compose_project_name(work_dir);
    // The root docker-compose.yml interpolates PG_PASSWORD / COXAGENT_ADMIN_PASSWORD
    // eagerly (COX-C012 uses ${VAR:?} so real deployments fail fast rather than
    // boot with a known default). A `build` still interpolates those env sections,
    // so without values this cross-target check dies at interpolation before it can
    // verify anything on every secret-bearing compose file. Seed them via the SAME
    // honour-generate rule as a real deploy ([`resolve_deploy_secrets`]): an
    // operator's own value wins, otherwise a fresh random one — never a public
    // constant baked into source (CXA-B017). These are ephemeral verification-only
    // values passed to one throwaway build command — never written to config or used
    // to start services.
    let secrets = resolve_deploy_secrets(work_dir);
    let mut build = Command::new("docker");
    build.args(["compose", "-p", &proj, "build"]);
    build.current_dir(work_dir);
    seed_deploy_secrets(&mut build, &secrets);
    let out = tokio::time::timeout(DEPLOY_TIMEOUT, build.output())
        .await
        .ok()?
        .ok()?;

    if out.status.success() {
        return Some(CrossCheck {
            available: true,
            reason: String::new(),
            errors: Vec::new(),
        });
    }
    let text = String::from_utf8_lossy(&out.stderr);
    let Some(gap) = interpolation_gap(&text) else {
        return Some(build_failure_check(&text));
    };
    let dockerfile = work_dir.join("Dockerfile");
    if !dockerfile.exists() {
        return Some(CrossCheck {
            available: false,
            reason: format!(
                "cannot verify the Linux build: `docker compose build` aborted at variable \
                 interpolation ({gap}) and no Dockerfile is available to build directly — set \
                 the required env vars or add a Dockerfile"
            ),
            errors: Vec::new(),
        });
    }
    let dockerfile_build = tokio::time::timeout(
        DEPLOY_TIMEOUT,
        Command::new("docker")
            .args(["build", "-f", "Dockerfile", "."])
            .current_dir(work_dir)
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    .ok()
    .and_then(Result::ok);
    let Some(dockerfile_build) = dockerfile_build else {
        return Some(CrossCheck {
            available: false,
            reason: format!(
                "cannot verify the Linux build: `docker compose build` aborted at variable \
                 interpolation ({gap}), and the Dockerfile fallback build did not complete \
                 (timed out or failed to start) — set the required env vars to verify via compose"
            ),
            errors: Vec::new(),
        });
    };
    if dockerfile_build.status.success() {
        return Some(CrossCheck {
            available: true,
            reason: "verified via `docker build` (Dockerfile) — compose build could not run \
                     because required env vars are unset here"
                .to_owned(),
            errors: Vec::new(),
        });
    }
    Some(build_failure_check(&String::from_utf8_lossy(
        &dockerfile_build.stderr,
    )))
}

/// Split `cargo clippy` human output into its error lines and the files those
/// errors point at. Clippy prints the location on the `-->` line that follows
/// each error, so the two are paired in report order.
fn parse_clippy(text: &str) -> (Vec<String>, Vec<String>) {
    let mut errors = Vec::new();
    let mut files = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if !line.trim_start().starts_with("error") {
            continue;
        }
        errors.push(line.trim().to_owned());
        // The location follows within a couple of lines; stop at the next error
        // so an error without one never steals the following error's file.
        let mut file = String::new();
        for _ in 0..3 {
            let Some(next) = lines.peek() else { break };
            let next = (*next).trim();
            if next.starts_with("error") {
                break;
            }
            if let Some(loc) = next.strip_prefix("--> ") {
                loc.split(':')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .clone_into(&mut file);
                lines.next();
                break;
            }
            lines.next();
        }
        files.push(file);
    }
    (errors, files)
}

#[cfg(test)]
mod clippy_parse_tests {
    use super::parse_clippy;

    #[test]
    fn pairs_each_error_with_the_file_it_points_at() {
        let out = "\
error: redundant closure
  --> crates/app/src/lib.rs:12:5
   |
error: too many lines
  --> crates/domain/src/ticket.rs:99:1
   |
error: could not compile `x` due to 2 previous errors
";
        let (errors, files) = parse_clippy(out);
        assert_eq!(errors.len(), 3);
        assert_eq!(
            files,
            [
                "crates/app/src/lib.rs",
                "crates/domain/src/ticket.rs",
                // The summary line carries no location and must not borrow one.
                ""
            ]
        );
    }
}

#[cfg(test)]
mod interpolation_gap_tests {
    use super::interpolation_gap;

    #[test]
    fn a_missing_required_var_is_recognized() {
        let stderr = "error while interpolating services.db.environment.POSTGRES_PASSWORD: \
                       required variable PG_PASSWORD is missing a value: PG_PASSWORD is \
                       required — set it before running docker compose up";
        assert!(interpolation_gap(stderr).is_some());
    }

    #[test]
    fn a_genuine_compile_failure_is_not_mistaken_for_an_interpolation_gap() {
        let stderr = "error[E0433]: failed to resolve: use of undeclared crate `foo`\n \
                       --> src/main.rs:1:1";
        assert!(interpolation_gap(stderr).is_none());
    }
}

#[cfg(test)]
mod cross_check_tests {
    use super::DockerComposeDeploy;
    use coxagent_application::ports::outbound::DeployPort;

    #[tokio::test]
    async fn a_non_cargo_project_is_reported_unavailable_with_a_reason() {
        // "Unavailable" must always carry why. A blank reason is how a blind
        // spot becomes invisible again.
        let dir = std::env::temp_dir().join(format!("crosschk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let check = DockerComposeDeploy::new()
            .cross_target_check(&dir)
            .await
            .expect("check");
        assert!(!check.available);
        assert!(
            !check.reason.trim().is_empty(),
            "reason must say what to do"
        );
        assert!(check.errors.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Regression guards for CXA-B017 (never bake a source-published credential,
/// never clobber an operator's configured secret) and CXA-B028 (never persist
/// generated superuser credentials into the agent-managed source tree).
#[cfg(test)]
mod deploy_secret_tests {
    use super::{
        compose_project_name, missing_required_secrets, random_secret, read_dot_env,
        resolve_deploy_secrets_in, store_file,
    };
    use std::collections::HashSet;

    fn set(keys: &[&str]) -> HashSet<String> {
        keys.iter().map(|k| (*k).to_owned()).collect()
    }

    /// The core decision rule (CXA-B017): a key configured by the operator —
    /// via process env OR a project-dir `.env` — is left alone (never overridden
    /// with ours); only a key missing from BOTH sources gets a fallback seed.
    #[test]
    fn precedence_honours_process_env_then_dot_env_then_fallback() {
        // No config anywhere -> both keys need a fallback.
        assert_eq!(
            missing_required_secrets(|_| false, &set(&[])),
            vec!["PG_PASSWORD", "COXAGENT_ADMIN_PASSWORD"]
        );

        // Process env supplies PG_PASSWORD -> it is not our concern.
        assert_eq!(
            missing_required_secrets(|k| k == "PG_PASSWORD", &set(&[])),
            vec!["COXAGENT_ADMIN_PASSWORD"]
        );

        // A project-dir `.env` supplies COXAGENT_ADMIN_PASSWORD -> not ours.
        assert_eq!(
            missing_required_secrets(|_| false, &set(&["COXAGENT_ADMIN_PASSWORD"])),
            vec!["PG_PASSWORD"]
        );

        // Both configured -> nothing to seed at all (no clobbering).
        assert!(missing_required_secrets(
            |k| k == "PG_PASSWORD",
            &set(&["COXAGENT_ADMIN_PASSWORD"]),
        )
        .is_empty());
    }

    /// The strongest guarantee in the ticket: when nothing is configured we
    /// fall back to a RANDOM secret, never a fixed constant — so no reader of
    /// source can log in as super on any deployment. Two resolutions differ,
    /// and neither equals the old public constants.
    #[test]
    fn generated_secrets_are_random_never_baked_constants() {
        let a = random_secret();
        let b = random_secret();
        assert_ne!(a, b, "secrets must be unique per call");
        for s in [&a, &b] {
            assert_ne!(
                s.as_str(),
                "ci-verify-pg",
                "the old baked PG constant must never come back"
            );
            assert_ne!(
                s.as_str(),
                "ci-verify-admin",
                "the old baked admin constant must never come back"
            );
            assert_eq!(s.len(), 32, "generated secret length");
            assert!(
                !s.chars().any(char::is_whitespace),
                "secret feeds YAML and a DSN URL — no whitespace"
            );
            assert!(
                !s.contains([':', '@', '"', '\'']),
                "secret must not break YAML/DSN syntax"
            );
        }
    }

    #[test]
    fn generated_charset_excludes_similar_lookalikes() {
        // Excluding 0/O/1/l/I keeps secrets unambiguous when shown to a human;
        // this also proves we are not using an alphabet that reintroduces the
        // guessable lowercase-only pattern of the old constants.
        let sample: String = (0..20).map(|_| random_secret()).collect();
        for bad in ['0', 'O', '1', 'l', 'I'] {
            assert!(
                !sample.contains(bad),
                "'{bad}' should be absent from charset"
            );
        }
    }

    /// The `.env` parser that decides whether an operator already configured a
    /// secret in the project dir — it sees every assignment shape compose does,
    /// ignores comments/blanks/blank values, and never treats prose as config.
    #[test]
    fn dot_env_parser_detects_only_real_assignments() {
        let keys = read_dot_env(std::path::Path::new("/nonexistent/.env"));
        assert!(keys.is_empty(), "absent file -> no provided keys");

        let dir = std::env::temp_dir().join(format!("dotenv-t-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join(".env");
        std::fs::write(
            &path,
            "# comment\nPG_PASSWORD=a:b@c=\n\nCOXAGENT_ADMIN_PASSWORD=strong!\nexport OTHER_SECRET=x\na\nB=\n",
        )
        .expect("write");
        let keys = read_dot_env(&path);
        for present in ["PG_PASSWORD", "COXAGENT_ADMIN_PASSWORD", "OTHER_SECRET"] {
            assert!(keys.contains(present), "{present} should be detected");
        }
        // Blank value (`B=`) and valueless (`a`) lines do NOT count as config —
        // treating them as provided would either fail `${VAR:?}` or boot empty.
        assert!(!keys.contains("B"), "'B=' is blank — not real config");
        assert!(!keys.contains("a"), "'a' has no value — not real config");
    }

    /// CXA-B028 regression guard: resolving deploy secrets must NEVER persist
    /// them into the project's `.env`. In the hub flow that `.env` lives in
    /// `<project>/codebase` — the persistent agent checkout agents read diffs
    /// from and commit from; writing a live root/admin login there leaves a
    /// durable plaintext superuser credential at rest inside a source tree,
    /// exactly contradicting CXA-B017 (`no reader of source can predict a
    /// deployment's superuser password`) for unconfigured app-driven deploys.
    ///
    /// Resolution reads config only — its generated values exist as transient
    /// per-pass child-process env ([`seed_deploy_secrets`]) and are never written
    /// back out. If someone reintroduces B027-style write-back
    /// (`materialize_deploy_secrets` / `upsert_dot_env`) into resolution or deploy,
    /// this guard fails at review even though git-only gates never see an
    /// untracked preview `.env`.
    #[test]
    fn resolving_deploy_secrets_never_persists_them_to_work_dir_dot_env() {
        let dir = std::env::temp_dir().join(format!("cxab028-t-{}", std::process::id()));
        let store_root = std::env::temp_dir().join(format!("cxab028-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&store_root);
        std::fs::create_dir_all(&dir).expect("mkdir");

        // Resolve against a bare work dir (no `.env`, nothing pre-written), with an
        // isolated out-of-tree store root so tests never touch a real HOME store.
        resolve_deploy_secrets_in(&dir, &store_root);

        // Persisting generated secrets into the source tree would materialise a
        // `.env` here; its absence proves we wrote nothing back into the work dir.
        assert!(
            !dir.join(".env").exists(),
            "resolving must not create <project>/codebase/.env where agents read diffs from \
             and commit from (CXA-B028)"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&store_root);
    }

    /// CXA-B031 regression guard: generated fallback secrets are STABLE across
    /// deploy cycles of the SAME project. The original bug regenerated a fresh
    /// random secret on every pass: Postgres initialised pgdata with cycle N's
    /// value, then cycle N+1 connected with a regenerated one, breaking DB auth
    /// and admin login. Re-resolving against the same out-of-tree store must now
    /// yield identical values for one project while different projects stay distinct.
    #[test]
    fn generated_secrets_are_stable_across_cycles_for_the_same_project() {
        let proj_a = std::env::temp_dir().join(format!("cxab031-a-{}", std::process::id()));
        let proj_b = std::env::temp_dir().join(format!("cxab031-b-{}", std::process::id()));
        let secret_root =
            std::env::temp_dir().join(format!("cxab031-store-{}", std::process::id()));

        for d in [&proj_a, &proj_b] {
            let _ = std::fs::remove_dir_all(d);
            std::fs::create_dir_all(d).expect("mkdir");
        }
        let _ = std::fs::remove_dir_all(&secret_root);

        // Two deploy cycles of the same unconfigured project must agree exactly:
        // this is the whole point of CXA-B031 (no per-cycle rotation).
        let first = resolve_deploy_secrets_in(&proj_a, &secret_root);
        let second = resolve_deploy_secrets_in(&proj_a, &secret_root);
        assert_eq!(
            first, second,
            "a project's generated secrets must be stable across deploy cycles (CXA-B031)"
        );

        if !first.is_empty() {
            // Values live OUT of tree: stored under secret_root keyed per-project.
            assert!(
                !store_file(&secret_root, &proj_a).starts_with(&proj_a),
                "secrets must be stored outside the project source tree"
            );
            assert!(
                store_file(&secret_root, &proj_a).exists(),
                "generated secret must be persisted out of tree"
            );
            // A different project resolves to a different store file (isolation).
            assert_ne!(
                store_file(&secret_root, &proj_a),
                store_file(&secret_root, &proj_b),
                "distinct projects must not share a secret store file"
            );
        }

        for d in [&proj_a, &proj_b] {
            let _ = std::fs::remove_dir_all(d);
        }
        let _ = std::fs::remove_dir_all(&secret_root);
    }

    /// CXA-B032 regression guard: secret stability must follow docker's NAMED
    /// pgdata volume, not the source checkout's absolute path. Postgres data
    /// persists in a named volume keyed by compose project name (`<project>_db`),
    /// which is independent of where on disk an app was cloned/moved. Relocating
    /// an unconfigured app (a fresh checkout, or an agent slot re-cloned at a new
    /// path) must therefore resolve to the SAME store file and the SAME secrets,
    /// or a regenerated PG_PASSWORD breaks auth against already-initialized pgdata.
    ///
    /// We simulate relocation with two work dirs whose full absolute paths differ
    /// but whose parent+dir BASENAMES agree - so `compose_project_name` matches
    /// while any old path-hash key would have diverged.
    #[test]
    fn secrets_survive_relocating_the_app_between_paths() {
        // Same basename pair (`reloc/app`) at two genuinely different absolute
        // roots: `/tmp/<rand-a>/reloc/app` vs `/tmp/<rand-b>/reloc/app`.
        let root_a = std::env::temp_dir().join(format!("cxab032-root-a-{}", std::process::id()));
        let root_b = std::env::temp_dir().join(format!("cxab032-root-b-{}", std::process::id()));
        let proj_at_a = root_a.join("reloc").join("app");
        let proj_at_b = root_b.join("reloc").join("app");
        let secret_root =
            std::env::temp_dir().join(format!("cxab032-store-{}", std::process::id()));

        for d in [&proj_at_a, &proj_at_b] {
            let _ = std::fs::remove_dir_all(d);
            std::fs::create_dir_all(d).expect("mkdir");
        }
        let _ = std::fs::remove_dir_all(&secret_root);

        // Sanity: this really is the "same logical app under relocation" shape —
        // same compose project identity (what docker names its pgdata volume by)
        // despite different absolute paths.
        assert_eq!(
            compose_project_name(&proj_at_a),
            compose_project_name(&proj_at_b),
            "test premise broken: relocated clones should share a compose project name"
        );
        // First cycle writes from location A...
        let first = resolve_deploy_secrets_in(&proj_at_a, &secret_root);
        assert!(
            !first.is_empty(),
            "unconfigured app must generate fallback secrets"
        );

        // ...then the SAME logical app is re-deployed from relocated location B:
        // it must reuse A's values (same store file), never regenerate them.
        let relocated = resolve_deploy_secrets_in(&proj_at_b, &secret_root);
        assert_eq!(
            first, relocated,
            "an app's generated secrets must survive relocating its checkout \
             between paths (CXA-B032)"
        );

        for d in [&root_a, &root_b] {
            let _ = std::fs::remove_dir_all(d);
        }
        let _ = std::fs::remove_dir_all(&secret_root);
    }
}

impl DockerComposeDeploy {
    /// Run one test command and read its verdict. Shared by the full and the
    /// scoped paths so both inherit the heavy-slot gate, the nice priority,
    /// the kill-the-tree timeout and the output summarising.
    async fn run_test_command(
        &self,
        work_dir: &Path,
        cmd: &str,
        args: &[&str],
    ) -> Result<DeployReport, PortError> {
        // Host-wide gate + nice: at most COXAGENT_MAX_PARALLEL_HEAVY test
        // suites run at once across ALL projects, and each runs at background
        // priority — N projects can no longer freeze the machine together.
        let _slot = crate::proc::heavy_slot().await;
        let child = crate::proc::low_priority(cmd)
            .args(args)
            .current_dir(work_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| PortError::Backend(format!("spawn {cmd}: {e}")))?;
        let leader = child.id();
        let Ok(output) =
            // 30 minutes: a cold target dir (fresh agent branch) compiles the
            // whole workspace before a single test runs — 15 was not enough.
            tokio::time::timeout(Duration::from_secs(1800), child.wait_with_output()).await
        else {
            // Kill the whole test-runner tree, not just `nice`.
            if let Some(pid) = leader {
                crate::proc::kill_group(pid);
            }
            return Err(PortError::Backend("test run timed out".to_owned()));
        };
        let output = output.map_err(|e| PortError::Backend(format!("{cmd} wait: {e}")))?;
        let success = output.status.success();
        let tail = |b: &[u8]| -> String {
            String::from_utf8_lossy(b)
                .lines()
                .rev()
                .take(12)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n")
        };
        let summary = if success {
            format!("{cmd} {} passed", args.join(" "))
        } else {
            let out = tail(&output.stdout);
            let err = tail(&output.stderr);
            format!("{cmd} tests failed:\n{err}\n{out}")
        };
        Ok(DeployReport {
            success,
            deployed: true,
            summary,
        })
    }
}
