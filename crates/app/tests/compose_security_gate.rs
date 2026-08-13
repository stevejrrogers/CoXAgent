//! COX-C012 security gate: compose files must not contain insecure defaults.
//!
//! Three rules, checked as pure functions over `(path, contents)` pairs so
//! synthetic fixtures can prove the guard bites before it is pointed at the
//! real files.
//!
//! **Rule 1 — no insecure fallback on a password key.**
//! A compose interpolation `${VAR:-literal}` silently boots with `literal` when
//! the operator forgets to set `VAR`. For credentials that means the service
//! starts with a known, public-repo-visible password. Replace with `${VAR:?msg}`
//! so compose fails fast with a clear error instead.
//!
//! **Rule 2 — no bare host-port binding on a datastore service.**
//! A mapping like `"5432:5432"` (no host-IP prefix) binds to `0.0.0.0`, putting
//! Postgres/Redis/Mongo/MinIO reachable from the LAN. These services must use
//! `"127.0.0.1:<host>:<container>"` so only the local machine can reach them.
//! Non-datastore app services (coxagent, gateway, caddy, …) may publish bare
//! host ports — the intentional `"8101:4000"` is legal.
//!
//! The `check` function is a pure function over YAML source and returns `Err`
//! instead of panicking; the tests at the bottom feed it synthetic fixtures to
//! prove each rule fires independently before the guard is run against the real
//! tree.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Datastore service names that must not publish bare host ports.
// ---------------------------------------------------------------------------

const DATASTORE_SERVICES: &[&str] = &["db", "postgres", "redis", "mongo", "minio"];

// ---------------------------------------------------------------------------
// A single security finding.
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
struct Finding {
    file: String,
    /// YAML path or context, e.g. "services.db.environment.POSTGRES_PASSWORD"
    context: String,
    why: String,
}

// ---------------------------------------------------------------------------
// Rule 1: insecure fallback on a password key.
// ---------------------------------------------------------------------------

/// Returns true if `key` looks like a secret/credential name.
fn is_password_key(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    k.contains("PASSWORD") || k.contains("SECRET") || k.contains("PASSWD")
}

/// Returns `Some(fallback)` when `value` is a compose interpolation with a
/// non-empty default using the `:-` form: `${VAR:-something}`.
/// The `:?` form (error on unset) is safe and must NOT be flagged.
fn insecure_fallback(value: &str) -> Option<&str> {
    let inner = value.strip_prefix("${")?.strip_suffix('}')?;
    let (_, after_colon) = inner.split_once(":-")?;
    // An empty fallback is harmless (blank default).
    if after_colon.is_empty() {
        return None;
    }
    Some(after_colon)
}

/// Scan a compose YAML document (as raw text) for Rule 1 violations.
/// Returns findings with `file` set to `path`.
fn check_insecure_fallbacks(path: &str, src: &str) -> Vec<Finding> {
    let doc: serde_yaml::Value = match serde_yaml::from_str(src) {
        Ok(v) => v,
        Err(e) => {
            return vec![Finding {
                file: path.to_string(),
                context: "parse".to_string(),
                why: format!("could not parse YAML: {e}"),
            }];
        }
    };

    let mut findings = Vec::new();
    let Some(services) = doc["services"].as_mapping() else {
        return findings;
    };

    for (svc_name, svc) in services {
        let svc_name = svc_name.as_str().unwrap_or("?");
        if let Some(env) = svc["environment"].as_mapping() {
            for (k, v) in env {
                let key = k.as_str().unwrap_or("");
                let val = v.as_str().unwrap_or("");

                // Check password keys for direct insecure fallback.
                if is_password_key(key) {
                    if let Some(fallback) = insecure_fallback(val) {
                        findings.push(Finding {
                            file: path.to_string(),
                            context: format!("services.{svc_name}.environment.{key}"),
                            why: format!(
                                "`{key}` uses `${{VAR:-{fallback}}}` — compose will boot \
                                 with `{fallback}` when the variable is unset. \
                                 Use `${{VAR:?{key} is required}}` to fail fast instead"
                            ),
                        });
                    }
                }

                // Check DSN/URL keys for embedded insecure fallbacks (passwords
                // carried inside a connection string).
                if key.ends_with("_DSN") || key.ends_with("_URL") {
                    let mut rest = val;
                    while let Some(start) = rest.find("${") {
                        let token_src = &rest[start..];
                        let end = match token_src.find('}') {
                            Some(i) => i + 1,
                            None => token_src.len(),
                        };
                        let token = &token_src[..end];
                        if let Some(fallback) = insecure_fallback(token) {
                            findings.push(Finding {
                                file: path.to_string(),
                                context: format!("services.{svc_name}.environment.{key}"),
                                why: format!(
                                    "`{key}` embeds `{token}` — a password carried in a \
                                     DSN/URL will use the fallback `{fallback}` when unset. \
                                     Use `${{VAR:?message}}` to fail fast instead"
                                ),
                            });
                        }
                        rest = &token_src[end..];
                    }
                }
            }
        }
    }
    findings
}

// ---------------------------------------------------------------------------
// Rule 2: bare host-port binding on a datastore service.
// ---------------------------------------------------------------------------

/// Returns true when `mapping` binds to all interfaces (no explicit host IP).
/// A mapping is `[host_ip:]host_port:container_port[/proto]`.
/// Only two-segment mappings (no host IP) are bare.
fn is_bare_binding(mapping: &str) -> bool {
    // Strip trailing /proto.
    let m = mapping.split('/').next().unwrap_or(mapping);
    // `127.0.0.1:5432:5432` — 3 colons, not bare.
    // `5432:5432`            — 1 colon, bare.
    // `5432`                 — 0 colons, container-only, not a host binding.
    m.chars().filter(|&c| c == ':').count() == 1
}

/// Scan a compose YAML document for Rule 2 violations on datastore services.
fn check_bare_datastore_ports(path: &str, src: &str) -> Vec<Finding> {
    let doc: serde_yaml::Value = match serde_yaml::from_str(src) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    let mut findings = Vec::new();
    let Some(services) = doc["services"].as_mapping() else {
        return findings;
    };

    for (svc_name, svc) in services {
        let svc_name = svc_name.as_str().unwrap_or("?");
        if !DATASTORE_SERVICES.contains(&svc_name) {
            continue; // only police datastores
        }
        let Some(ports) = svc["ports"].as_sequence() else {
            continue;
        };
        for entry in ports {
            let mapping = match entry {
                serde_yaml::Value::String(s) => s.as_str(),
                serde_yaml::Value::Number(_) => {
                    // e.g. `- 5432` — container-only, no host binding.
                    continue;
                }
                _ => continue,
            };
            if is_bare_binding(mapping) {
                findings.push(Finding {
                    file: path.to_string(),
                    context: format!("services.{svc_name}.ports"),
                    why: format!(
                        "`{svc_name}` publishes `{mapping}` without a host-IP prefix — \
                         Docker binds this to 0.0.0.0, putting {svc_name} on the LAN. \
                         Use `\"127.0.0.1:{mapping}\"` to restrict access to this host only"
                    ),
                });
            }
        }
    }
    findings
}

// ---------------------------------------------------------------------------
// Combined check: run both rules, collect all findings.
// ---------------------------------------------------------------------------

fn check(path: &str, src: &str) -> Vec<Finding> {
    let mut findings = check_insecure_fallbacks(path, src);
    findings.extend(check_bare_datastore_ports(path, src));
    findings
}

// ---------------------------------------------------------------------------
// Real-file helpers.
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn read_file(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// Guard run against the real repo files — must pass clean after the fix.
// ---------------------------------------------------------------------------

#[test]
fn root_compose_has_no_insecure_defaults() {
    let src = read_file("docker-compose.yml");
    let findings = check("docker-compose.yml", &src);
    assert!(
        findings.is_empty(),
        "docker-compose.yml has insecure defaults (COX-C012):\n{}",
        findings
            .iter()
            .map(|f| format!("  {}  [{}]  {}", f.file, f.context, f.why))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn local_infra_compose_has_no_insecure_defaults() {
    let src = read_file("deploy/local-infra/docker-compose.yml");
    let findings = check("deploy/local-infra/docker-compose.yml", &src);
    assert!(
        findings.is_empty(),
        "deploy/local-infra/docker-compose.yml has insecure defaults (COX-C012):\n{}",
        findings
            .iter()
            .map(|f| format!("  {}  [{}]  {}", f.file, f.context, f.why))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn split_compose_has_no_insecure_defaults() {
    let src = read_file("deploy/docker-compose.split.yml");
    let findings = check("deploy/docker-compose.split.yml", &src);
    assert!(
        findings.is_empty(),
        "deploy/docker-compose.split.yml has insecure defaults (COX-C012):\n{}",
        findings
            .iter()
            .map(|f| format!("  {}  [{}]  {}", f.file, f.context, f.why))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn deploy_compose_has_no_insecure_defaults() {
    let src = read_file("deploy/docker-compose.yml");
    let findings = check("deploy/docker-compose.yml", &src);
    assert!(
        findings.is_empty(),
        "deploy/docker-compose.yml has insecure defaults (COX-C012):\n{}",
        findings
            .iter()
            .map(|f| format!("  {}  [{}]  {}", f.file, f.context, f.why))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// ---------------------------------------------------------------------------
// Synthetic fixtures — prove each rule bites independently.
// ---------------------------------------------------------------------------

/// A minimal compose string with a single db service, configurable password
/// and port mapping.
fn db_compose(password_val: &str, port: &str) -> String {
    format!(
        "services:\n  db:\n    image: postgres:16-alpine\n\
         \x20   environment:\n      POSTGRES_PASSWORD: {password_val}\n\
         \x20   ports:\n      - \"{port}\"\n"
    )
}

/// A minimal compose string with a coxagent (non-datastore) service.
fn app_compose(port: &str) -> String {
    format!(
        "services:\n  coxagent:\n    image: coxagent\n\
         \x20   ports:\n      - \"{port}\"\n"
    )
}

// --- Rule 1 synthetic tests ------------------------------------------------

#[test]
fn insecure_fallback_on_password_key_is_caught() {
    let src = db_compose("${PG_PASSWORD:-coxagent_dev}", "127.0.0.1:5432:5432");
    let findings = check("docker-compose.yml", &src);
    assert!(
        !findings.is_empty(),
        "a `:-fallback` on a PASSWORD key must be caught"
    );
    let why = &findings[0].why;
    assert!(
        why.contains("coxagent_dev"),
        "finding must name the fallback value: {why}"
    );
    assert!(
        why.contains("POSTGRES_PASSWORD"),
        "finding must name the key: {why}"
    );
}

#[test]
fn changeme_fallback_is_caught() {
    let src = db_compose("${COXAGENT_ADMIN_PASSWORD:-changeme}", "127.0.0.1:5432:5432");
    let findings = check("docker-compose.yml", &src);
    assert!(
        !findings.is_empty(),
        "`:-changeme` on a PASSWORD key must be caught"
    );
}

#[test]
fn safe_required_form_passes_rule1() {
    // ${VAR:?message} — fails compose if unset, no fallback. Must not be flagged.
    let src = db_compose(
        "${PG_PASSWORD:?PG_PASSWORD is required}",
        "127.0.0.1:5432:5432",
    );
    let findings = check("docker-compose.yml", &src);
    assert!(
        findings.is_empty(),
        "`${{VAR:?msg}}` must not be flagged: {findings:?}"
    );
}

#[test]
fn env_reference_without_fallback_passes_rule1() {
    // ${VAR} with no default — unset will leave it blank (compose may warn,
    // but no silent insecure boot). Not flagged by Rule 1.
    let src = db_compose("${PG_PASSWORD}", "127.0.0.1:5432:5432");
    let findings = check("docker-compose.yml", &src);
    assert!(
        findings.is_empty(),
        "`${{VAR}}` with no fallback must not be flagged: {findings:?}"
    );
}

#[test]
fn dsn_with_embedded_insecure_fallback_is_caught() {
    let src = "services:\n  coxagent:\n    image: coxagent\n\
         \x20   environment:\n\
         \x20     COXAGENT_DB_DSN: postgres://postgres:${{PG_PASSWORD:-coxagent_dev}}@db:5432/coxagent\n\
         \x20   ports:\n      - \"8101:4000\"\n"
        .to_string();
    let findings = check("docker-compose.yml", &src);
    assert!(
        !findings.is_empty(),
        "a `:-fallback` embedded in a _DSN value must be caught"
    );
    assert!(
        findings[0].why.contains("coxagent_dev"),
        "finding must name the fallback: {:?}",
        findings[0]
    );
}

#[test]
fn non_password_key_with_fallback_passes_rule1() {
    // A non-secret key with a fallback is fine (e.g. POSTGRES_DB).
    let src = "services:\n  db:\n    image: postgres:16-alpine\n\
         \x20   environment:\n\
         \x20     POSTGRES_DB: ${{PG_DB:-coxagent}}\n\
         \x20   ports:\n      - \"127.0.0.1:5432:5432\"\n"
        .to_string();
    let findings = check("docker-compose.yml", &src);
    assert!(
        findings.is_empty(),
        "a fallback on a non-secret key must not be flagged: {findings:?}"
    );
}

// --- Rule 2 synthetic tests ------------------------------------------------

#[test]
fn bare_host_port_on_datastore_is_caught() {
    let src = db_compose("${PG_PASSWORD:?required}", "5432:5432");
    let findings = check("docker-compose.yml", &src);
    assert!(
        !findings.is_empty(),
        "a bare `5432:5432` on a datastore must be caught"
    );
    let why = &findings[0].why;
    assert!(why.contains("0.0.0.0"), "finding must mention 0.0.0.0: {why}");
    assert!(why.contains("127.0.0.1"), "finding must suggest 127.0.0.1: {why}");
}

#[test]
fn loopback_prefixed_port_passes_rule2() {
    let src = db_compose("${PG_PASSWORD:?required}", "127.0.0.1:5432:5432");
    let findings = check("docker-compose.yml", &src);
    assert!(
        findings.is_empty(),
        "`127.0.0.1:`-prefixed port on a datastore must not be flagged: {findings:?}"
    );
}

#[test]
fn bare_port_on_non_datastore_service_passes_rule2() {
    // The coxagent service publishes 8101:4000 bare — intentional, must not be flagged.
    let src = app_compose("8101:4000");
    let findings = check("docker-compose.yml", &src);
    assert!(
        findings.is_empty(),
        "a bare host port on a non-datastore service must not be flagged: {findings:?}"
    );
}

#[test]
fn bare_redis_port_is_caught() {
    let src = "services:\n  redis:\n    image: redis:7-alpine\n\
         \x20   ports:\n      - \"6379:6379\"\n"
        .to_string();
    let findings = check("docker-compose.yml", &src);
    assert!(
        !findings.is_empty(),
        "bare `6379:6379` on redis must be caught"
    );
}

#[test]
fn bare_mongo_port_is_caught() {
    let src = "services:\n  mongo:\n    image: mongo:7\n\
         \x20   ports:\n      - \"27017:27017\"\n"
        .to_string();
    let findings = check("docker-compose.yml", &src);
    assert!(
        !findings.is_empty(),
        "bare `27017:27017` on mongo must be caught"
    );
}

#[test]
fn bare_minio_port_is_caught() {
    let src = "services:\n  minio:\n    image: minio/minio\n\
         \x20   environment:\n\
         \x20     MINIO_ROOT_USER: coxagent\n\
         \x20     MINIO_ROOT_PASSWORD: ${{MINIO_ROOT_PASSWORD:?required}}\n\
         \x20   ports:\n      - \"9000:9000\"\n"
        .to_string();
    let findings = check("docker-compose.yml", &src);
    // The bare port finding; the password key is clean.
    assert!(
        findings
            .iter()
            .any(|f| f.context.contains("minio") && f.context.contains("ports")),
        "bare `9000:9000` on minio must be caught: {findings:?}"
    );
}

// --- Both rules clean simultaneously ----------------------------------------

#[test]
fn clean_compose_passes_both_rules() {
    let src = db_compose("${PG_PASSWORD:?PG_PASSWORD is required}", "127.0.0.1:5432:5432");
    let findings = check("docker-compose.yml", &src);
    assert!(
        findings.is_empty(),
        "a fully hardened compose must produce no findings: {findings:?}"
    );
}
