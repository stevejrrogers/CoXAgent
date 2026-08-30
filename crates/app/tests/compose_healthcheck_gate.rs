//! CXA-B115 regression guard: the root compose's db healthcheck must
//! authenticate, not merely poll availability.
//!
//! The bug: `pg_isready -U postgres` never sends credentials — it reports
//! "accepting connections" for any server, whatever password the stored role
//! carries. A pgdata volume initialized with an older PG_PASSWORD therefore
//! deployed "healthy": `POSTGRES_PASSWORD` only applies on first volume init,
//! every TCP login failed with `password authentication failed`, and the hub
//! silently degraded (audit sink → memory, KV → local file) behind log lines
//! nobody was watching.
//!
//! Three properties make a healthcheck able to catch that mismatch:
//!   1. it runs `psql` (pg_isready cannot authenticate, by design);
//!   2. it presents `POSTGRES_PASSWORD` — the credential the app itself uses;
//!   3. it dials the container's docker-network address (`-h "$(hostname
//!      -i)"`): the image's pg_hba keeps the unix socket AND 127.0.0.1/32 on
//!      `trust` — only the entrypoint-appended `host all all all
//!      scram-sha-256` catch-all, which matches the non-loopback address the
//!      app itself dials, requires the stored password.
//!
//! The guard is a pure function over compose YAML source that returns `Err`
//! instead of panicking; the tests run it against the real file and against
//! synthetic drift to prove each rule bites independently.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// The guard: pure functions over compose YAML source.
// ---------------------------------------------------------------------------

/// The healthcheck command of the service that carries `POSTGRES_PASSWORD`
/// (the Postgres primary). Handles both `test: ["CMD-SHELL", "<cmd>"]` and the
/// plain string form by joining every element into one searchable command.
/// `Err` covers the structural failures: no password-carrying service (the
/// guard would be blind) or a dropped/empty healthcheck.
fn db_healthcheck_command(compose_src: &str) -> Result<String, String> {
    let doc: serde_yaml::Value =
        serde_yaml::from_str(compose_src).map_err(|e| format!("docker-compose.yml: {e}"))?;
    let services = doc["services"]
        .as_mapping()
        .ok_or_else(|| "docker-compose.yml: no services map".to_string())?;
    for (name, svc) in services {
        let name = name.as_str().unwrap_or("<unnamed>");
        let carries_password = svc["environment"]["POSTGRES_PASSWORD"].as_str().is_some();
        if !carries_password {
            continue;
        }
        let test = svc["healthcheck"]["test"]
            .as_sequence()
            .map(|seq| {
                seq.iter()
                    .filter_map(serde_yaml::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .or_else(|| svc["healthcheck"]["test"].as_str().map(str::to_string));
        return test.filter(|s| !s.is_empty()).ok_or_else(|| {
            format!(
                "services.{name}: the db healthcheck is gone — without an \
                 authenticating check a pgdata volume whose stored password \
                 differs from PG_PASSWORD deploys 'healthy' while every login \
                 fails (CXA-B115)"
            )
        });
    }
    Err(
        "docker-compose.yml: no service carries POSTGRES_PASSWORD — the healthcheck \
         guard is blind"
            .to_string(),
    )
}

/// True when the first `-h <arg>` in `test` names a trust path: no usable `-h`
/// at all (unix socket), a socket directory (`/…`), or a loopback address. All
/// three hit pg_hba `trust` lines in the image, so none can detect a
/// stored-password mismatch; only a non-loopback dial — the address the app
/// itself uses, e.g. `-h "$(hostname -i)"` — reaches the entrypoint's
/// `host all all all scram-sha-256` catch-all, which requires the password.
///
/// `-h` must start its own token: a `-h` inside a longer flag (`--host=…`)
/// is skipped, not parsed as the dial target. The attached form `-h<arg>` is
/// a real psql dial and IS parsed.
fn dials_a_trust_path(test: &str) -> bool {
    let mut rest = test;
    while let Some(at) = rest.find("-h") {
        let starts_token =
            at == 0 || rest[..at].chars().last().is_some_and(|c| c.is_ascii_whitespace());
        let after = &rest[at + 2..];
        // split_whitespace already ignores leading whitespace.
        let arg = after.split_whitespace().next().unwrap_or("");
        if starts_token {
            if arg.is_empty() {
                return true; // bare `-h` with no target: psql falls back to the socket
            }
            let a = arg.trim_matches(|c| c == '"' || c == '\'');
            return a.starts_with('/') || a == "localhost" || a == "::1" || a.starts_with("127.");
        }
        rest = after; // `-h` inside a longer flag — keep scanning
    }
    true // no -h at all: psql dials the trust-authenticated unix socket
}

/// Why `test` cannot detect a stored-password mismatch; `None` when it can.
fn undetectable_mismatch_reason(test: &str) -> Option<String> {
    if !test.contains("psql") {
        let hint = if test.contains("pg_isready") {
            "pg_isready never sends credentials — it reports 'accepting connections' \
             for any server, whatever password the stored role carries"
        } else {
            "the check runs no authenticated query"
        };
        return Some(format!(
            "db healthcheck cannot detect a password mismatch (CXA-B115): {hint}. \
             Use psql with $POSTGRES_PASSWORD over the docker-network address"
        ));
    }
    if !test.contains("POSTGRES_PASSWORD") {
        return Some(
            "db healthcheck runs psql but never presents POSTGRES_PASSWORD (CXA-B115): \
             over the network address that fails auth on every probe (db never \
             healthy), over the socket it authenticates nothing. Use \
             PGPASSWORD=\"$POSTGRES_PASSWORD\""
                .to_string(),
        );
    }
    if dials_a_trust_path(test) {
        return Some(
            "db healthcheck presents the password but dials a trust path — the unix \
             socket AND 127.0.0.1/32 are `trust` in the image's pg_hba, so the check \
             succeeds even when the stored password differs (CXA-B115). Dial the \
             container's docker-network address (-h \"$(hostname -i)\") so the \
             entrypoint's `host all all all scram-sha-256` catch-all forces the \
             password check"
                .to_string(),
        );
    }
    None
}

/// The whole CXA-B115 gate over one compose document: `Ok` only when the db
/// healthcheck would catch a stored-password mismatch.
fn mismatch_is_detectable(compose_src: &str) -> Result<(), String> {
    let test = db_healthcheck_command(compose_src)?;
    match undetectable_mismatch_reason(&test) {
        Some(why) => Err(why),
        None => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// The guard, run against the real repo files.
// ---------------------------------------------------------------------------

#[test]
fn root_compose_db_healthcheck_authenticates() {
    if let Err(why) = mismatch_is_detectable(&read("docker-compose.yml")) {
        panic!("docker-compose.yml: {why}");
    }
}

#[test]
fn compose_header_documents_the_rotation_recovery() {
    // The "clear message" half of CXA-B115: the header must tell the operator
    // why db went unhealthy and how to recover without data loss.
    let header: String = read("docker-compose.yml")
        .lines()
        .take_while(|l| l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    for needle in ["FIRST initialized", "ALTER USER postgres PASSWORD"] {
        assert!(
            header.contains(needle),
            "docker-compose.yml header must carry the pgdata volume / password-rotation \
             warning (CXA-B115): missing `{needle}`"
        );
    }
}

#[test]
fn deployment_docs_document_the_volume_password_rotation() {
    let md = read("DEPLOYMENT.md");
    for needle in [
        "first initialized",
        "ALTER USER postgres PASSWORD",
        "pgdata",
    ] {
        assert!(
            md.contains(needle),
            "DEPLOYMENT.md must document the pgdata volume / password-rotation \
             recovery for the root compose (CXA-B115): missing `{needle}`"
        );
    }
}

// ---------------------------------------------------------------------------
// Synthetic drift — proves each rule bites independently.
// ---------------------------------------------------------------------------

/// A minimal compose whose db service carries a POSTGRES_PASSWORD and the
/// given healthcheck command (the whole point of the gate).
fn compose_with_healthcheck(cmd: &str) -> String {
    format!(
        "services:\n  db:\n    image: postgres:16-alpine\n\
         \x20   environment:\n      POSTGRES_PASSWORD: ${{PG_PASSWORD:?required}}\n\
         \x20   healthcheck:\n      test:\n        - CMD-SHELL\n        - {cmd}\n"
    )
}

/// The exact pre-fix shape that shipped the bug: available ≠ authenticatable.
#[test]
fn bare_pg_isready_is_caught() {
    let why =
        mismatch_is_detectable(&compose_with_healthcheck("pg_isready -U postgres")).unwrap_err();
    assert!(why.contains("pg_isready"), "unhelpful message: {why}");
    assert!(why.contains("CXA-B115"), "message must cite the ticket: {why}");
}

/// A healthcheck dropped entirely is structural blindness, not a pass.
#[test]
fn dropped_healthcheck_is_caught() {
    let src = "services:\n  db:\n    image: postgres:16-alpine\n\
               \x20   environment:\n      POSTGRES_PASSWORD: ${PG_PASSWORD:?required}\n";
    let why = mismatch_is_detectable(src).unwrap_err();
    assert!(
        why.contains("healthcheck is gone"),
        "unhelpful message: {why}"
    );
}

/// psql over the socket without a password: authenticates nothing twice over.
#[test]
fn socket_psql_without_password_is_caught() {
    let why = mismatch_is_detectable(&compose_with_healthcheck(
        "psql -U postgres -c 'select 1'",
    ))
    .unwrap_err();
    assert!(
        why.contains("POSTGRES_PASSWORD"),
        "unhelpful message: {why}"
    );
}

/// psql over the network address but passwordless: db would never turn
/// healthy on a GOOD deploy — also a bug the gate must catch.
#[test]
fn network_dial_without_password_is_caught() {
    let why = mismatch_is_detectable(&compose_with_healthcheck(
        "psql -h \"$(hostname -i)\" -U postgres -c 'select 1'",
    ))
    .unwrap_err();
    assert!(
        why.contains("POSTGRES_PASSWORD"),
        "unhelpful message: {why}"
    );
}

/// The subtle wrong fix: right password, wrong transport. The socket is
/// `trust`, so this check succeeds even against a mismatched volume.
#[test]
fn password_over_the_socket_is_caught() {
    let why = mismatch_is_detectable(&compose_with_healthcheck(
        "PGPASSWORD=\"$POSTGRES_PASSWORD\" psql -U postgres -c 'select 1'",
    ))
    .unwrap_err();
    assert!(why.contains("socket"), "unhelpful message: {why}");
    assert!(why.contains("trust"), "unhelpful message: {why}");
}

/// The trap this ticket was missed by twice: 127.0.0.1 LOOKS like a real TCP
/// dial, but the image's pg_hba keeps `127.0.0.1/32 trust` ahead of the
/// scram-sha-256 catch-all, so a loopback check succeeds regardless of the
/// stored password.
#[test]
fn loopback_dial_is_caught() {
    let why = mismatch_is_detectable(&compose_with_healthcheck(
        "PGPASSWORD=\"$POSTGRES_PASSWORD\" psql -h 127.0.0.1 -U postgres -c 'select 1'",
    ))
    .unwrap_err();
    assert!(why.contains("trust"), "unhelpful message: {why}");
    assert!(why.contains("127.0.0.1"), "unhelpful message: {why}");
}

/// `-h /var/run/...` names a socket directory, not a network host — the
/// address rule must not be fooled by the flag's mere presence.
#[test]
fn socket_directory_via_h_flag_is_caught() {
    let why = mismatch_is_detectable(&compose_with_healthcheck(
        "PGPASSWORD=\"$POSTGRES_PASSWORD\" psql -h /var/run/postgresql -U postgres -c 'select 1'",
    ))
    .unwrap_err();
    assert!(why.contains("socket"), "unhelpful message: {why}");
}

/// The attached psql form `-h<host>` is a real dial and must be parsed as one:
/// `-h127.0.0.1` dials the trust loopback exactly like the spaced form.
#[test]
fn attached_loopback_form_is_caught() {
    let why = mismatch_is_detectable(&compose_with_healthcheck(
        "PGPASSWORD=\"$POSTGRES_PASSWORD\" psql -h127.0.0.1 -U postgres -c 'select 1'",
    ))
    .unwrap_err();
    assert!(why.contains("trust"), "unhelpful message: {why}");
    assert!(why.contains("127.0.0.1"), "unhelpful message: {why}");
}

/// A `-h` that is merely a fragment of a longer flag (`--host=…`) is not a
/// dial target — the scan must skip it rather than misread `ost=…` as the
/// host and accidentally bless whatever follows. The gate holds healthchecks
/// to the canonical `-h` dial; anything else is flagged with instructions.
#[test]
fn h_fragment_inside_a_longer_flag_is_not_a_dial() {
    let why = mismatch_is_detectable(&compose_with_healthcheck(
        "PGPASSWORD=\"$POSTGRES_PASSWORD\" psql --host=\"$(hostname -i)\" -U postgres -c 'select 1'",
    ))
    .unwrap_err();
    assert!(why.contains("trust"), "unhelpful message: {why}");
}

/// The shipped, fixed shape must pass cleanly.
#[test]
fn authenticating_network_dial_passes() {
    let ok = mismatch_is_detectable(&compose_with_healthcheck(
        "PGPASSWORD=\"$POSTGRES_PASSWORD\" psql -h \"$(hostname -i)\" -U postgres -c 'select 1'",
    ));
    assert_eq!(ok, Ok(()), "the fixed healthcheck must pass: {ok:?}");
}

/// No service carrying POSTGRES_PASSWORD would leave the guard blind — it
/// must fail loudly rather than bless nothing.
#[test]
fn compose_without_pg_password_service_is_reported() {
    let src = "services:\n  coxagent:\n    image: coxagent\n";
    let why = mismatch_is_detectable(src).unwrap_err();
    assert!(why.contains("blind"), "unhelpful message: {why}");
}
