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

/// Ephemeral verification-only resolution used by [`compose_build_check`]. Honors
/// operator config (process env or project-dir `.env`) exactly like a real deploy,
/// then falls back to a fresh random value for whatever remains — but NEVER consults
/// or writes the persistent secret store nor mutates any host state. Builds must stay
/// side-effect-free regarding pinning/store files, so this is deliberately separate
/// from the durable [`resolve_deploy_secrets`].
fn ephemeral_resolve(work_dir: &std::path::Path) -> Vec<(String, String)> {
    let dot_env = read_dot_env(&work_dir.join(".env"));
    missing_required_secrets(
        |key| std::env::var(key).is_ok_and(|v| !v.trim().is_empty()),
        &dot_env,
    )
    .into_iter()
    .map(|key| (key.to_owned(), random_secret()))
    .collect()
}

/// Durable/stabilizing resolution used ONLY by real deploys ([`DockerComposeDeploy::deploy`]).
///
/// Precedence (never clobber anyone):
/// 1. An operator-configured value wins if present EITHER as process env OR as an already
///    assigned non-blank KEY=VALUE in `<workdir>/.env`. Such keys are left entirely alone.
/// 2. Otherwise consult a persistent hub-private store keyed by `compose_project_name`
///    (`$COXAGENT_DEPLOY_SECRET_DIR/<proj>.secrets`, else `$HOME/.coxagent-deploy/<proj>.secrets`)
///    OUTSIDE any source tree / repo checkout / workdir subtree; reuse a value we generated and
///    persisted on an earlier cycle unchanged so it stays stable across cycles (CXA-B030).
/// 3. Only when neither exists generate a fresh random via [`random_secret()`] AND persist it so
///    future cycles reuse it instead of rotating it.
///
/// Values are recomputed each invocation strictly from what's missing after filters, so an
/// operator configuring a key externally AFTER an earlier pinned cycle wins precedence on later
/// passes — stale pins never leak back once externally configured. Persistence is best-effort:
/// if writing fails we log a tracing warning about lost cross-cycle stability but still return the
/// resolved values so deploys don't fail outright.
fn resolve_deploy_secrets(work_dir: &std::path::Path) -> Vec<(String, String)> {
    let proj = compose_project_name(work_dir);
    let store_path = deploy_secret_store_path(&proj);

    // Recompute each invocation strictly from what's missing after filters, so an
    // operator configuring a key externally AFTER an earlier pinned cycle wins
    // precedence on later passes — stale pins never leak back once configured.
    let dot_env = read_dot_env(&work_dir.join(".env"));
    let missing = missing_required_secrets(
        |key| std::env::var(key).is_ok_and(|v| !v.trim().is_empty()),
        &dot_env,
    );

    // Adopt-and-remove any SUPERSEDED path-keyed store this same project left
    // behind under an earlier software version's SHA-of-path filename BEFORE the
    // empty-missing shortcut below, so obsolete duplicates are cleaned up on every
    // successful post-upgrade resolution pass — even one where the operator now
    // supplies every required secret externally (CXA-B040). Its live credentials must
    // still be adopted so they are never regenerated against already-initialized pgdata.
    reconcile_superseded_path_keyed_store(work_dir, &store_path);

    if missing.is_empty() {
        return Vec::new();
    }

    // Precedence 2: reuse pinned values we persisted on an earlier cycle unchanged.
    let prior = read_dot_env_values(&store_path);
    let mut resolved: Vec<(String, String)> = Vec::with_capacity(missing.len());
    for key in missing {
        if let Some(existing) = prior.get(key).filter(|v| !v.trim().is_empty()) {
            resolved.push((key.to_owned(), existing.clone()));
        } else {
            // Precedence 3: generate fresh AND persist for future cycles.
            resolved.push((key.to_owned(), random_secret()));
        }
    }

    // Best-effort persistence off-repo so cross-cycle stability survives; a write
    // failure must not fail the deploy itself (matches B027's behavior).
    if let Err(e) = persist_generated_deploy_secrets(&store_path, &resolved) {
        tracing::warn!(
            "could not persist generated deploy secrets to {} — cross-cycle stability lost \
             (this deploy still proceeds): {e}",
            store_path.display()
        );
    }

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

/// Like [`read_dot_env`] but keeps the assigned VALUES, not just key presence. Uses
/// the same loose dot-env grammar: skip `#` comments and blank lines, tolerate an
/// optional leading `export`, split on the FIRST `=` so values containing `=` survive,
/// and only a non-blank value counts as provided. Returns an empty map for an absent
/// or unreadable file.
fn read_dot_env_values(path: &std::path::Path) -> std::collections::HashMap<String, String> {
    use std::collections::HashMap;
    let Ok(contents) = std::fs::read_to_string(path) else {
        return HashMap::new();
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
            Some((key, v.to_owned()))
        })
        .collect()
}

/// The hub-private path where generated deploy secrets are persisted off-repo,
/// keyed per compose project: `$COXAGENT_DEPLOY_SECRET_DIR/<proj>.secrets` when that
/// env var is set (an override that lets hermetic tests point at a temp dir), else
/// `$HOME/.coxagent-deploy/<proj>.secrets`. Lives OUTSIDE any source tree / repo
/// checkout / workdir subtree so live superuser creds never land in git-managed source.
fn deploy_secret_store_path(proj: &str) -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("COXAGENT_DEPLOY_SECRET_DIR") {
        if !dir.trim().is_empty() {
            return std::path::PathBuf::from(dir).join(format!("{proj}.secrets"));
        }
    }
    // HOME convention mirrors crates/infrastructure/src/proc.rs (~line 114).
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    home.join(".coxagent-deploy")
        .join(format!("{proj}.secrets"))
}

/// The on-disk root where CXA-B031-era software persisted each project's fallback
/// secrets under a PATH-hash filename: `$HOME/.local/share/coxagent/deploy-secrets`
/// (or an explicit `COXAGENT_DEPLOY_SECRETS_DIR` override, used by hermetic tests).
///
/// That scheme was superseded by the per-compose-project name-keyed store
/// ([`deploy_secret_store_path`]); this root is read ONLY as a migration source by
/// [`reconcile_superseded_path_keyed_store`] so legacy values can be adopted and their
/// files removed, and is never a destination for new writes (CXA-B040).
fn legacy_cxb031_store_root() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("COXAGENT_DEPLOY_SECRETS_DIR") {
        if !dir.is_empty() {
            return std::path::PathBuf::from(dir);
        }
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

/// Filename an earlier software version (CXA-B031) kept one project's fallback secrets
/// under: SHA-256 of the canonicalised absolute work dir, in `<root>/<hex>.env`. Reconstructed
/// byte-for-byte — including the canonicalise-with-path-fallback on error — so an ALREADY-DEPLOYED
/// unconfigured app whose live PG_PASSWORD sits at that obsolete filename can still be located and
/// reconciled during the post-upgrade window. It is only a read/migration source; new values are
/// never written here.
fn legacy_cxb031_store_file(work_dir: &std::path::Path) -> std::path::PathBuf {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(
        work_dir
            .canonicalize()
            .unwrap_or_else(|_| work_dir.to_path_buf())
            .to_string_lossy()
            .as_bytes(),
    );
    legacy_cxb031_store_root().join(format!("{:x}.env", hasher.finalize()))
}

/// Reconcile-and-remove any SUPERSEDED path-keyed secret store left behind for THIS project by an
/// earlier software version.
///
/// CXA-B032/B033 moved secret persistence onto per-compose-project NAME-keyed files; anything an old
/// build stored under its obsolete SHA-of-absolute-path filename is therefore unreferenced yet still
/// holds live DB/admin credentials at rest forever — a duplicate plaintext copy that neither rotates,
/// relates to, nor discloses differently than its twin (CXA-B040). When such a file exists for this same
/// checkout we adopt any value our canonical store does not already know — so pgdata-initialized creds are
/// not regenerated against, preserving cross-cycle stability — persist them into `<proj>.secrets`, then DELETE the obsolete file so no duplicate credential remains on disk.
///
/// Deletion is gated on durable adoption into the canonical store: only after `persist_generated_deploy_secrets`
/// succeeds do we remove the legacy file, so we can never lose live credentials before they are owned by our new location.
///
/// Best-effort like all persistence here: failures are logged via [`tracing`], never fatal to a deploy.
fn reconcile_superseded_path_keyed_store(work_dir: &std::path::Path, canonical: &std::path::Path) {
    let legacy_path = legacy_cxb031_store_file(work_dir);

    // Nothing superseded anywhere? No-op without touching host state.
    if !legacy_path.exists() {
        return;
    }

    // Merge every legacy value into our canonical store WITHOUT ever overriding a
    // value we already own there — operator-configured or previously-adopted creds win.
    let mut merged = read_dot_env_values(canonical);
    for (k, v) in read_dot_env_values(&legacy_path) {
        merged.entry(k).or_insert(v);
    }
    let mut adopted: Vec<(String, String)> = merged.into_iter().collect();
    adopted.sort();

    // Persist durably first; only once our canonical store owns the adopted values is it
    // safe to remove the superseded duplicate — deleting first could strand live creds.
    match persist_generated_deploy_secrets(canonical, &adopted) {
        Ok(()) => match std::fs::remove_file(&legacy_path) {
            Ok(()) => tracing::info!(
                "removed superseded path-keyed deploy secret store {} after adopting its \
                 values into {}",
                legacy_path.display(),
                canonical.display()
            ),
            Err(e) => tracing::warn!(
                "adopted values from superseded secret store {} but could not remove it: {e}",
                legacy_path.display()
            ),
        },
        Err(e) => tracing::warn!(
            "could not adopt values from superseded secret store {} into {} ({e}); leaving \
             it in place until adoption succeeds so no live credential is lost",
            legacy_path.display(),
            canonical.display()
        ),
    }
}

/// Persist generated deploy secrets for one project to the off-repo store. Preserves
/// prior stored values for OTHER keys verbatim while upserting the given entries
/// idempotently; creates parent dirs best-effort; on Unix sets restrictive perms
/// (~0600 file, ~0700 dir) since these are live superuser creds at rest off-repo.
fn persist_generated_deploy_secrets(
    path: &std::path::Path,
    entries: &[(String, String)],
) -> std::io::Result<()> {
    use std::collections::HashMap;

    // Preserve prior stored values for OTHER keys verbatim while upserting given
    // entries idempotently.
    let mut merged: HashMap<String, String> = read_dot_env_values(path);
    for (k, v) in entries {
        merged.insert(k.clone(), v.clone());
    }

    // Restrictive perms on live superuser creds at rest off-repo. Best-effort: a
    // chmod failure is not fatal — we still write the values.
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    if let Err(e) = std::fs::create_dir_all(parent) {
        tracing::warn!(
            "could not create deploy secret dir {}: {e}",
            parent.display()
        );
        return Err(e);
    }

    // Serialize as loose dot-env (same grammar read_dot_env_values understands),
    // one KEY=VALUE per line, sorted for determinism.
    let mut keys: Vec<&String> = merged.keys().collect();
    keys.sort();
    let mut out = String::new();
    for k in keys {
        out.push_str(k);
        out.push('=');
        out.push_str(&merged[k]);
        out.push('\n');
    }

    // Atomic-ish write: write then restrict perms so live superuser creds are not
    // left world-readable on disk (CXA-B033). Best-effort per B027's contract: a
    // chmod failure is logged via [`restrict_store_to_owner`], never allowed to
    // fail the deploy.
    std::fs::write(path, out)?;
    #[cfg(unix)]
    restrict_store_to_owner(path);
    Ok(())
}

/// Restrict an off-repo deploy-secret store FILE (~0600) and its PARENT DIR (~0700)
/// to owner-only once written (CXA-B033). Without this an umask of 022 leaves a fresh
/// `.secrets` file at world-readable 0644 — live PG/admin credentials readable by any
/// other local user; hardening the enclosing directory as well keeps even filenames,
/// sizes and existence unobservable to them and protects against future files dropped
/// there with looser modes.
///
/// Both steps are best-effort like every other piece of persistence here — failures are
/// logged so a security regression stays visible in the deploy trace without aborting it.
#[cfg(unix)]
fn restrict_store_to_owner(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    fn set(dir: &std::path::Path, mode: u32) -> std::io::Result<()> {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode))
    }

    // File owner-only; a failure here is the regression this ticket fixes, so it
    // gets its own message naming the file. Dir hardening is logged separately.
    if let Err(e) = set(path, 0o600) {
        tracing::warn!(
            "could not restrict deploy secret store {} to owner-only (0600): {e}",
            path.display()
        );
    }
    if let Some(parent) = path.parent().filter(|p| p.as_os_str() != ".") {
        if let Err(e) = set(parent, 0o700) {
            tracing::warn!(
                "could not restrict deploy secret dir {} to owner-only (0700): {e}",
                parent.display()
            );
        }
    }
}

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
    // honour-generate rule as a real deploy ([`ephemeral_resolve`]): an operator's
    // own value wins, otherwise a fresh random one — never a public constant baked
    // into source (CXA-B017). These are ephemeral verification-only values passed to
    // one throwaway build command — deliberately NOT persisted to the off-repo store.
    let secrets = ephemeral_resolve(work_dir);
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

/// Regression guard for CXA-B017: an app-driven deploy must NEVER bake a
/// public, source-published credential into long-running services, and must
/// honour an operator's own configured secrets rather than clobbering them.
#[cfg(test)]
mod deploy_secret_tests {
    use super::{
        compose_project_name, deploy_secret_store_path, ephemeral_resolve,
        legacy_cxb031_store_file, missing_required_secrets, persist_generated_deploy_secrets,
        random_secret, read_dot_env, read_dot_env_values, reconcile_superseded_path_keyed_store,
        resolve_deploy_secrets,
    };
    use std::collections::HashSet;

    fn set(keys: &[&str]) -> HashSet<String> {
        keys.iter().map(|k| (*k).to_owned()).collect()
    }

    /// Serialises every test that mutates the process-global
    /// COXAGENT_DEPLOY_SECRET_DIR so concurrent threads cannot race each other's
    /// env set/restore while resolving secret-store paths.
    ///
    /// A key — whether supplied via process env OR a project-dir `.env` — is
    /// left alone (never overridden with ours); only a key missing from BOTH
    /// sources gets a fallback seed.
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

    /// Serialises every secret-store-dependent test in this module. Those tests
    /// mutate process-GLOBAL state — the `COXAGENT_DEPLOY_SECRET_DIR` env var and
    /// the single off-repo store directory it points at — so running them on
    /// parallel threads lets one test wipe/re-point another's just-persisted pins
    /// mid-body (see CXA-B030 regression where a concurrent `SecretStoreGuard`
    /// erased an earlier pass's pin before it was read back). Holding this lock
    /// for the whole body keeps those global mutations atomic per test.
    static SECRET_STORE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Points [`deploy_secret_store_path`] at a fresh temp dir for one test, then
    /// restores any prior value on drop so hermetic tests never touch a real $HOME.
    struct SecretStoreGuard {
        prev: Option<std::ffi::OsString>,
        _serialized: std::sync::MutexGuard<'static, ()>,
    }
    impl SecretStoreGuard {
        fn new() -> Self {
            let serialization = SECRET_STORE_TEST_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let prev = std::env::var_os("COXAGENT_DEPLOY_SECRET_DIR");
            let dir = std::env::temp_dir().join(format!("cxa-b030-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("mkdir store dir");
            std::env::set_var("COXAGENT_DEPLOY_SECRET_DIR", &dir);
            // The lock guard must outlive this constructor so every other
            // secret-store test stays serialised until after our env restore on
            // drop. It is stored solely for its RAII lifetime (never read again),
            // hence an underscore-prefixed field which also silences dead-code.
            SecretStoreGuard {
                prev,
                _serialized: serialization,
            }
        }
    }
    impl Drop for SecretStoreGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("COXAGENT_DEPLOY_SECRET_DIR", v),
                None => std::env::remove_var("COXAGENT_DEPLOY_SECRET_DIR"),
            }
        }
    }

    /// Points [`legacy_cxb031_store_root`] at a fresh temp dir for one test, then
    /// restores any prior value on drop — so CXA-B040 tests never touch a real
    /// `$HOME/.local/share/coxagent/deploy-secrets`. Serialised under the same lock
    /// as [`SecretStoreGuard`] so env mutations stay atomic across concurrent tests.
    struct LegacyStoreGuard {
        prev: Option<std::ffi::OsString>,
        _serialized: std::sync::MutexGuard<'static, ()>,
    }
    impl LegacyStoreGuard {
        fn new() -> Self {
            let serialization = SECRET_STORE_TEST_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let prev = std::env::var_os("COXAGENT_DEPLOY_SECRETS_DIR");
            let dir = std::env::temp_dir().join(format!("cxa-b040-legacy-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("mkdir legacy store dir");
            std::env::set_var("COXAGENT_DEPLOY_SECRETS_DIR", &dir);
            LegacyStoreGuard {
                prev,
                _serialized: serialization,
            }
        }
    }
    impl Drop for LegacyStoreGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("COXAGENT_DEPLOY_SECRETS_DIR", v),
                None => std::env::remove_var("COXAGENT_DEPLOY_SECRETS_DIR"),
            }
        }
    }

    /// A throwaway per-test work dir (named by a distinct tag so concurrently
    /// running tests never clobber each other's project files). Mirrors the
    /// temp-dir idiom used by [`dot_env_parser_detects_only_real_assignments`].
    fn work_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cxa-b030-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    /// Tests that resolve secrets treat the project as UNCONFIGURED: drop any
    /// ambient operator-set copies of our required keys from process env so a
    /// dev machine exporting them cannot skew resolution counts.
    fn clear_required_env() {
        for k in ["PG_PASSWORD", "COXAGENT_ADMIN_PASSWORD"] {
            std::env::remove_var(k);
        }
    }

    /// CXA-B030 core: an UNCONFIGURED project resolves the same two secrets on
    /// every call — resolution is stable across cycles (no rotation between
    /// passes), so restarting a deploy does not orphan its persisted DB creds.
    #[test]
    fn unconfigured_project_resolves_the_same_secrets_across_cycles() {
        clear_required_env();
        let _guard = SecretStoreGuard::new();
        let work = work_dir("stable");
        let pass1 = resolve_deploy_secrets(&work);
        assert_eq!(pass1.len(), 2);
        let pass2 = resolve_deploy_secrets(&work);
        assert_eq!(pass2, pass1); // CYCLE STABILITY == CXA-B030 core
    }

    /// CXA-B028 guard: real resolution persists generated creds to the OFF-REPO
    /// store only and must NEVER materialise a `.env` inside the source tree.
    #[test]
    fn generated_secrets_persist_outside_the_source_tree_only() {
        let _guard = SecretStoreGuard::new();
        let work = work_dir("offtree");
        resolve_deploy_secrets(&work);
        assert!(
            !work.join(".env").exists(),
            "resolution must never materialise .env inside project dir (CXA-B028)"
        );
    }

    /// CXA-B033 regression guard: generated superuser creds at rest MUST be
    /// owner-only (~0600), not world-readable (0644). Without an explicit chmod a
    /// default umask of 022 leaves live PG/admin passwords readable by every other
    /// local user on the host.
    #[test]
    fn persisted_deploy_secrets_are_owner_only() {
        let _guard = SecretStoreGuard::new();
        let proj = compose_project_name(&work_dir("perms"));
        let store_path = deploy_secret_store_path(&proj);
        persist_generated_deploy_secrets(
            &store_path,
            &[("PG_PASSWORD".to_owned(), "s3cret".to_owned())],
        )
        .expect("persist");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&store_path)
                .expect("store file exists")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "store must be owner-only (0600), got {mode:o}"
            );
            assert_eq!(mode & 0o044, 0, "group+other read bits must be cleared");
        }
    }

    /// The verification-only resolver must be side-effect free: it never writes
    /// to the off-repo store file nor touches anything in the work dir.
    #[test]
    fn ephemeral_resolve_writes_nothing_to_store_or_workdir() {
        let _guard = SecretStoreGuard::new();
        let work = work_dir("eph");
        let sd = std::path::PathBuf::from(
            std::env::var("COXAGENT_DEPLOY_SECRET_DIR").expect("guard set"),
        );
        let expected_store_file = sd.join(format!("{}.secrets", compose_project_name(&work)));
        ephemeral_resolve(&work);
        assert!(!expected_store_file.exists());
        assert!(!work.join(".env").exists());
    }

    /// Once an operator configures a key in `<proj>/.env` AFTER an earlier pinned
    /// cycle, later passes must not resurrect that stale pin — external config wins.
    #[test]
    fn operator_configuration_wins_over_a_stale_pin_on_later_passes() {
        clear_required_env();
        let _guard = SecretStoreGuard::new();
        let work = work_dir("opin");
        let _pass1 = resolve_deploy_secrets(&work);

        // Pass one pinned PG_PASSWORD into the off-repo store; grab that stale value.
        let proj = compose_project_name(&work);
        let store_path = deploy_secret_store_path(&proj);
        let stored_before = read_dot_env_values(&store_path)
            .get("PG_PASSWORD")
            .cloned()
            .expect("pinned on pass1");

        // Operator pins PG_PASSWORD via the project-dir `.env` (the only route
        // that does NOT mutate global process env under parallel tests).
        std::fs::write(work.join(".env"), "PG_PASSWORD=<opsecret>\n").expect("write");

        // Later pass: externally configured keys are no longer our concern, so the
        // stale off-repo pin must NOT leak back into resolution.
        let result = resolve_deploy_secrets(&work);
        assert!(
            !result.contains(&("PG_PASSWORD".to_owned(), stored_before.clone())),
            "external operator config must override any prior stale pin"
        );
    }

    /// CXA-B040 core: a SUPERSEDED path-keyed store left behind for THIS project by an
    /// earlier software version must have its live value adopted (so pgdata-initialized
    /// creds are not regenerated) AND its duplicate plaintext file REMOVED — leaving no
    /// unreferenced second copy of active credentials at rest forever.
    #[test]
    fn superseded_path_keyed_store_is_adopted_then_removed() {
        clear_required_env();
        let _canonical = SecretStoreGuard::new();
        let _legacy = LegacyStoreGuard::new();
        let work = work_dir("b040-adopt");

        // Simulate an upgraded host: only the OLD path-hash store holds the value that
        // initialized pgdata; our canonical `<proj>.secrets` does not exist yet.
        let legacy_path = legacy_cxb031_store_file(&work);
        std::fs::write(
            &legacy_path,
            "PG_PASSWORD=stable-b031-password\nCOXAGENT_ADMIN_PASSWORD=stable-b031-admin\n",
        )
        .expect("write legacy store");
        assert!(legacy_path.exists(), "premise: legacy store present");

        // Resolving the unconfigured app must reuse B031's persisted value — never mint a
        // fresh one against already-initialized pgdata (CXA-B036 concern).
        let resolved = resolve_deploy_secrets(&work);
        assert_eq!(
            resolved
                .iter()
                .find(|(k, _)| k == "PG_PASSWORD")
                .map(|(_, v)| v.as_str()),
            Some("stable-b031-password"),
            "adopted value must flow into resolution instead of a fresh random"
        );

        // And the obsolete duplicate must be GONE once reconciled.
        assert!(
            !legacy_path.exists(),
            "superseded path-keyed store must be removed after adoption (CXA-B040)"
        );
    }

    /// CXA-B040 guard: when NO superseded path-keyed store exists, reconciliation is a
    /// strict no-op — it creates nothing and removes nothing outside the canonical store.
    #[test]
    fn no_superseded_store_is_a_no_op() {
        clear_required_env();
        let _canonical = SecretStoreGuard::new();
        let _legacy = LegacyStoreGuard::new();
        let work = work_dir("b040-noop");

        let legacy_root = std::env::var_os("COXAGENT_DEPLOY_SECRETS_DIR")
            .map(std::path::PathBuf::from)
            .unwrap();
        reconcile_superseded_path_keyed_store(
            &work,
            &deploy_secret_store_path(&compose_project_name(&work)),
        );

        // Nothing pre-existing under the legacy root -> nothing created or removed.
        assert!(
            legacy_root.read_dir().map_or(0, Iterator::count) == 0,
            "reconciliation must not fabricate a superseded store when none exists"
        );
    }

    /// CXA-B040 guard: even when our canonical store ALREADY holds a value (so nothing new is
    /// adopted), an obsolete path-keyed duplicate present on disk is still reconciled away —
    /// removing the unreferenced second plaintext copy while never overriding existing creds.
    #[test]
    fn existing_canonical_value_wins_and_superseded_copy_is_still_removed() {
        clear_required_env();
        let _canonical = SecretStoreGuard::new();
        let _legacy = LegacyStoreGuard::new();
        let work = work_dir("b040-dupe");

        // Canonical store already owns PG_PASSWORD from an earlier pass...
        persist_generated_deploy_secrets(
            &deploy_secret_store_path(&compose_project_name(&work)),
            &[("PG_PASSWORD".to_owned(), "already-owned".to_owned())],
        )
        .expect("persist canonical");

        // ...but an upgraded host ALSO carries an obsolete path-keyed duplicate holding a
        // DIFFERENT stale value for the same key.
        let legacy_path = legacy_cxb031_store_file(&work);
        std::fs::write(&legacy_path, "PG_PASSWORD=stale-legacy-password\n").expect("write");
        assert!(legacy_path.exists());

        reconcile_superseded_path_keyed_store(
            &work,
            &deploy_secret_store_path(&compose_project_name(&work)),
        );

        // The canonical value must survive untouched; only the superseded file goes away.
        assert_eq!(
            read_dot_env_values(&deploy_secret_store_path(&compose_project_name(&work)))
                .get("PG_PASSWORD")
                .map(String::as_str),
            Some("already-owned"),
            "existing canonical value must never be overridden by adoption"
        );
        assert!(
            !legacy_path.exists(),
            "superseded path-keyed store must still be removed (CXA-B040)"
        );
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
