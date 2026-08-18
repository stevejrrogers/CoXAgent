//! COX-C012 security gate: compose files must not contain insecure defaults.
//!
//! Three rules, checked as pure functions over `(path, contents)` pairs so
//! synthetic fixtures can prove the guard bites before it is pointed at the
//! real files.
//!
//! **Rule 1 — a credential must carry the required-marker `${VAR:?msg}`.**
//! A compose interpolation `${VAR:-literal}` silently boots with `literal` when
//! the operator forgets to set `VAR`, and a bare `${VAR}` resolves to an empty
//! string — either way Postgres/Redis/Mongo boot with blank or known credentials
//! instead of failing loudly (CXA-B029). For credentials that is a silent auth
//! bypass. Require `${VAR:?msg}` so compose fails fast with a clear error.
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

/// Split an interpolation body (`PG_PASSWORD`, `PG_PASSWORD:-x`,
/// `PG_PASSWORD:?msg`) into its variable name and two-character modifier
/// (`""`, `":-"`, or `":?"`).
fn interp_parts(inner: &str) -> (&str, &str) {
    match inner.find(':') {
        Some(i) => (&inner[..i], inner.get(i..i + 2).unwrap_or("")),
        None => (inner.trim(), ""),
    }
}

/// Report why a single credential interpolation is unsafe for Rule 1.
///
/// A secret reference may ONLY use the `${VAR:?message}` required-marker form:
///
///   - bare `${VAR}` resolves to an empty string when unset, so Postgres / Redis /
///     Mongo boot with BLANK credentials instead of failing loudly — the CXA-B029
///     gap. `.env.example` warns this Redis blank-password case is "a straight
///     authentication bypass".
///   - `${VAR:-nonempty}` boots with a known public-repo visible fallback.
///
/// Returns the human-readable reason when unsafe; None for the safe `:?` form,
/// an empty-fallback `${VAR:-}`, or anything that isn't a credential reference.
fn insecure_secret_reason(token_body: &str) -> Option<String> {
    let (name, modifier) = interp_parts(token_body);
    if !is_password_key(name) {
        return None;
    }
    match modifier {
        ":?" => None,
        ":-" => {
            // Non-empty fallback only; an empty one behaves like bare-blank but
            // carries no leaked literal, so treat it as needing :? instead.
            let after = &token_body[name.len() + 2..];
            if after.is_empty() {
                Some(format!(
                    "bare `${name}` has no required-marker — compose boots blank \
                     when unset. Use `${{{name}:?...}}` to fail fast"
                ))
            } else {
                Some(format!(
                    "compose will boot with default `{after}` when ${name} is unset \
                     — never use known defaults for credentials"
                ))
            }
        }
        _ => Some(format!(
            "bare `${name}` has no required-marker — compose boots blank \
             when unset. Use `${{{name}:?...}}` to fail fast"
        )),
    }
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

                // Scan every `${...}` interpolation on this key. A credential
                // variable must use the `${VAR:?msg}` required-marker form;
                // bare `${VAR}` and `:-fallback`s are flagged whether they appear
                // as the whole value or embedded inside a _DSN/_URL string.
                let mut rest = val;
                while let Some(start) = rest.find("${") {
                    let token_src = &rest[start + 2..];
                    let end = match token_src.find('}') {
                        Some(i) => i,
                        None => token_src.len(),
                    };
                    if is_password_key(key)
                        || is_password_key(&token_src[..end])
                        || key.ends_with("_DSN")
                        || key.ends_with("_URL")
                    {
                        if let Some(reason) = insecure_secret_reason(&token_src[..end]) {
                            findings.push(Finding {
                                file: path.to_string(),
                                context: format!("services.{svc_name}.environment.{key}"),
                                why: format!(
                                    "`{key}` carries {reason}. Use `${{VAR:?message}}` \
                                     so compose fails fast instead of booting blank"
                                ),
                            });
                        }
                    }
                    rest = &token_src[end..];
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
// Rule 3: deploy/docker-compose.cxa.yml must require its credentials.
//
// CXA-B016 — unlike Rule 1 (which polices `${VAR:-literal}`), this rule catches
// a *bare* `${VAR}` with no modifier at all. Before the fix cxa.yml used plain
// `${PG_USER}` / `${PG_PASSWORD}` / `${REDIS_PASSWORD}`, so running it without
// an .env silently started Postgres/Redis with EMPTY passwords instead of
// failing loudly like root docker-compose.yml does.
//
// Each required credential must be referenced through the required form
// `${VAR:...}` (compose errors on unset). A bare `${VAR}`, or a credential that
// is never referenced at all, is a finding.
//
// This rule is intentionally scoped to cxa-backend's own file — it asserts the
// exact contract this compose file promises — and is not folded into the shared
// `check()` because other compose files legitimately arrange their variables
// differently.
// ---------------------------------------------------------------------------

/// Credentials deploy/docker-compose.cxa.yml requires to boot without silently
/// using blank values. Order matters only for error reporting.
const CXA_REQUIRED_VARS: &[&str] = &["PG_USER", "PG_PASSWORD", "REDIS_PASSWORD"];

/// True when an interpolation token uses the required form (`${VAR:...}`),
/// which makes compose fail fast on an unset variable. A bare `${VAR}`
/// (no colon) resolves to blank and must be flagged.
fn is_required_form(token: &str) -> bool {
    match token.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        Some(inner) => inner.contains([':', '?']),
        None => false,
    }
}

/// Collect every interpolation token that references `var` (e.g. matching both
/// `${PG_USER?msg}` and `${PG_USER}`), preserving source order. The match is on
/// a full variable name: the character after the name must be a modifier (`:`)
/// or the closing brace — so `${REDIS_PASSWORD_LEGACY}` is NOT matched when
/// looking for `REDIS_PASSWORD`.
fn tokens_for_var(src: &str, var: &str) -> Vec<String> {
    let prefix = format!("${{{var}");
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(start) = rest.find("${") {
        let token_src = &rest[start..];
        let end = match token_src.find('}') {
            Some(i) => i + 1,
            None => break,
        };
        if token_src.starts_with(&prefix) {
            // Variable-name boundary: if the character right after the matched
            // name continues an identifier (`[A-Za-z0-9_]`) it belongs to a
            // longer differently-named variable; otherwise it terminates ours.
            // Accepts any terminate/modifier char (`:`/`?`/`-`/`}`).
            if matches!(
                token_src.as_bytes().get(prefix.len()),
                Some(b'_' | b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9')
            ) {
                rest = &token_src[end..];
                continue;
            }
            out.push(token_src[..end].to_string());
        }
        rest = &token_src[end..];
    }
    out
}

/// Check that every CXA_REQUIRED_VAR in `deploy/docker-compose.cxa.yml` is used
/// through its required form; report a bare reference or a missing reference as
/// a finding.
fn check_cxa_required_vars(path: &str, src: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    for var in CXA_REQUIRED_VARS {
        let refs = tokens_for_var(src, var);
        if refs.is_empty() {
            findings.push(Finding {
                file: path.to_string(),
                context: format!("variable {var}"),
                why: format!(
                    "{var} is required by cxa-backend but never referenced; \
                     compose would boot with it blank"
                ),
            });
            continue;
        }
        for tok in refs.iter().filter(|t| !is_required_form(t)) {
            findings.push(Finding {
                file: path.to_string(),
                context: format!("variable {var}"),
                why: format!(
                    "{var} is referenced as {tok}, not ${{{var}:?...}}; when unset compose \
                     silently boots with blank credentials (CXA-B016). Use \
                     ${{{var}:{var} is required}} to fail fast"
                ),
            });
        }
    }
    findings
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn read_file(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
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

#[test]
fn cxa_backend_compose_enforces_required_vars() {
    // CXA-B016 regression: PG_USER / PG_PASSWORD / REDIS_PASSWORD must be wired
    // through ${VAR:?msg} so compose fails fast on an unset variable instead of
    // silently booting Postgres/Redis with blank credentials. Fails against the
    // pre-fix file (bare ${VAR}); passes against the required-form file.
    let path = "deploy/docker-compose.cxa.yml";
    let src = read_file(path);
    let findings = check_cxa_required_vars(path, &src);
    assert!(
        findings.is_empty(),
        "deploy/docker-compose.cxa.yml does not enforce its required credentials (CXA-B016):\n{}",
        findings
            .iter()
            .map(|f| format!("  {}  [{}]  {}", f.file, f.context, f.why))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// ---------------------------------------------------------------------------
// CXA-B065 — root web-app stack must self-recover so port 8101 stays up.
//
// Regression guard for an incident where a host/Docker restart (or a SIGKILL,
// exit 137) left the app container down for ~11h because neither `db` nor
// `coxagent` declared a restart policy and autoheal only covered cxa-backend.
// Every service on the host-published web-app stack must opt into Docker's own
// auto-restart (`restart: unless-stopped`) so it recovers without manual ops.
//
// A plain value-check over parsed YAML — no evaluation of Docker semantics —
// which is exactly right for a gate that reads source files rather than live
// containers: it FAILS if anyone removes or weakens the policy in this file.
// ---------------------------------------------------------------------------

/// Return every service in `src` whose top-level `restart` is not set to
/// `unless-stopped`, keyed by service name.
fn services_without_unless_stopped(src: &str) -> Vec<String> {
    let doc: serde_yaml::Value = match serde_yaml::from_str(src) {
        Ok(v) => v,
        Err(_) => return vec!["<unparseable yaml>".to_string()],
    };
    let Some(services) = doc.get("services").and_then(serde_yaml::Value::as_mapping) else {
        return vec!["no services map".to_string()];
    };
    services
        .iter()
        .filter_map(|(name, body)| {
            if body.get("restart").and_then(serde_yaml::Value::as_str) == Some("unless-stopped") {
                None
            } else {
                Some(name.as_str().unwrap_or("<unnamed>").to_string())
            }
        })
        .collect()
}

#[test]
fn root_compose_every_service_self_recovers() {
    // CXA-B065 regression — fails against any edit that drops or weakens the
    // restart policy that keeps port 8101 alive across host/Docker restarts.
    let offenders = services_without_unless_stopped(&read_file("docker-compose.yml"));
    assert!(
        offenders.is_empty(),
        "root docker-compose.yml services must declare `restart: unless-stopped` \
         (CXA-B065): {offenders:?}"
    );
}

#[test]
fn missing_unless_stopped_is_reported() {
    // Prove the guard bites: a service with no restart policy is flagged, while
    // one with `restart: unless-stopped` is accepted.
    let src = "services:\n  db:\n    image: postgres\n  coxagent:\n\
               \x20   image: coxagent\n\
               \x20   restart: unless-stopped\n";
    assert_eq!(services_without_unless_stopped(src), vec!["db".to_string()]);
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
    let src = db_compose(
        "${COXAGENT_ADMIN_PASSWORD:-changeme}",
        "127.0.0.1:5432:5432",
    );
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
fn bare_secret_reference_is_caught() {
    // CXA-B029: `${VAR}` with no marker resolves to an empty string when unset,
    // so Postgres/Redis boot with BLANK credentials. This is exactly the gap
    // Rule 1 must close — a bare credential reference is now flagged.
    let src = db_compose("${PG_PASSWORD}", "127.0.0.1:5432:5432");
    let findings = check("docker-compose.yml", &src);
    assert!(
        !findings.is_empty(),
        "a bare `${{VAR}}` on a PASSWORD key must be caught"
    );
    let why = &findings[0].why;
    assert!(
        why.contains("POSTGRES_PASSWORD"),
        "finding must name the key: {why}"
    );
}

#[test]
fn non_secret_bare_reference_passes_rule1() {
    // A non-secret variable referenced without a marker is not a credential
    // (e.g. POSTGRES_DB) — leaving it blank isn't an auth bypass.
    let src = "services:\n  db:\n    image: postgres:16-alpine\n\
         \x20   environment:\n\
         \x20     POSTGRES_DB: ${{PG_DB}}\n\
         \x20   ports:\n      - \"127.0.0.1:5432:5432\"\n"
        .to_string();
    let findings = check("docker-compose.yml", &src);
    assert!(
        findings.is_empty(),
        "a bare reference on a non-secret key must not be flagged: {findings:?}"
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
    assert!(
        why.contains("0.0.0.0"),
        "finding must mention 0.0.0.0: {why}"
    );
    assert!(
        why.contains("127.0.0.1"),
        "finding must suggest 127.0.0.1: {why}"
    );
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
    let src = db_compose(
        "${PG_PASSWORD:?PG_PASSWORD is required}",
        "127.0.0.1:5432:5432",
    );
    let findings = check("docker-compose.yml", &src);
    assert!(
        findings.is_empty(),
        "a fully hardened compose must produce no findings: {findings:?}"
    );
}

// --- Rule 3 synthetic fixtures (CXA-B016) ------------------------------------

/// Minimal cxa-backend-style compose with a configurable set of credentials.
fn cxa_compose(pg_user: &str, pg_password: &str, redis_password: &str) -> String {
    format!(
        "services:\n  db:\n    image: postgres\n\
         \x20   environment:\n      POSTGRES_USER: {pg_user}\n\
         \x20     POSTGRES_PASSWORD: {pg_password}\n\
         \x20   ports:\n      - \"127.0.0.1:5433:5432\"\n\
         \x20 redis:\n    image: redis\n\
         \x20   command:\n      - \"--requirepass\"\n      - \"{redis_password}\"\n"
    )
}

/// The exact pre-fix shape that shipped the bug (CXA-B016): every credential is
/// a bare `${VAR}` with no modifier, so compose silently boots blank.
#[test]
fn bare_required_vars_are_caught() {
    let src = cxa_compose("${PG_USER}", "${PG_PASSWORD}", "${REDIS_PASSWORD}");
    let findings = check_cxa_required_vars("deploy/docker-compose.cxa.yml", &src);
    assert_eq!(
        findings.len(),
        3,
        "all three bare refs must be flagged: {findings:#?}"
    );
    for var in CXA_REQUIRED_VARS {
        assert!(
            findings.iter().any(|f| f.context.contains(var)),
            "a finding must exist for {var}: {findings:#?}"
        );
        assert!(
            findings
                .iter()
                .any(|f| f.context.contains(var) && f.why.contains(":?")),
            "{var} finding must recommend the required form `${{VAR:?}}`: {findings:#?}"
        );
    }
}

/// If a required credential is referenced through `${VAR:?...}` it must not be
/// flagged — this is the fixed form and the real cxa.yml after CXA-B016.
#[test]
fn required_form_passes() {
    let src = cxa_compose(
        "${PG_USER?PG_USER is required}",
        "${PG_PASSWORD?PG_PASSWORD is required}",
        "${REDIS_PASSWORD?REDIS_PASSWORD is required}",
    );
    let findings = check_cxa_required_vars("deploy/docker-compose.cxa.yml", &src);
    assert!(
        findings.is_empty(),
        "required-form credentials must pass cleanly (CXA-B016): {findings:#?}"
    );
}

/// A credential dropped entirely from the file must be caught — otherwise it
/// would boot with a blank value and nobody could notice.
#[test]
fn missing_required_var_is_caught() {
    // PG_USER referenced, PG_PASSWORD + REDIS_PASSWORD absent entirely.
    let src = cxa_compose("${PG_USER}", "", "");
    let findings = check_cxa_required_vars("deploy/docker-compose.cxa.yml", &src);
    let missing_passwd = findings.iter().any(|f| f.context.contains("PG_PASSWORD"));
    let missing_redis = findings
        .iter()
        .any(|f| f.context.contains("REDIS_PASSWORD"));
    assert!(
        missing_passwd && missing_redis,
        "dropping PG_PASSWORD / REDIS_PASSWORD entirely must be caught (only \
         found {len}): {findings:#?}",
        len = findings.len()
    );
}

/// The rule keys off full variable names and must not flag an unrelated
/// variable that merely shares a prefix (e.g. REDIS_PASSWORD_LEGACY), as long
/// as all three required credentials are present in their required form.
#[test]
fn prefix_sibling_does_not_cause_false_positive() {
    let src = cxa_compose(
        "${PG_USER?required}",
        "${PG_PASSWORD?required}",
        "${REDIS_PASSWORD?required}",
    );
    // Add an extra environment key referencing a longer-named sibling var in
    // bare form. It is NOT one of CXA_REQUIRED_VARS, so Rule 3 must ignore it.
    let src = format!("{src}     OTHER_REDIS_PASSWORD: ${{REDIS_PASSWORD_LEGACY}}\n");
    let findings = check_cxa_required_vars("deploy/docker-compose.cxa.yml", &src);
    assert!(
        findings.is_empty(),
        "a differently-named sibling var must not be flagged: {findings:#?}"
    );
}
