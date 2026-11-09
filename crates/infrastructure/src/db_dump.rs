//! Logical database backups for the hub's external stores.
//!
//! `coxagent backup` archives the file-backed stores, but the AUTHORITATIVE
//! data — project state, accounts (Postgres), and wiki/docs pages (MongoDB) —
//! lives in databases that archive cannot capture (it only printed a "run the
//! dump tool yourself" warning). This module dumps each configured store so one
//! backup run protects the databases too. Each dumper tries a host tool first
//! (the standard product posture — the operator installs the client), then
//! falls back to a `docker exec` into the store's container so a containerized
//! deployment with no host client is still protected. If neither is available
//! the caller gets a `Skipped` reason to log — a missing dump tool is never a
//! hard failure of the whole backup.
//!
//! Secrets, Postgres: the password reaches the child only through the
//! `PGPASSWORD` env var — never on argv (which `ps` exposes). For `docker exec`
//! that means `-e PGPASSWORD` with NO `=value` (docker forwards it from our
//! environment). Dumps use pg_dump's custom format (`-Fc`), restorable with
//! `pg_restore`.
//!
//! Secrets, Mongo: `mongodump` has no `PGPASSWORD` equivalent, so when the URI
//! carries a password the host path hands it to the child through a 0600
//! `--config` file and puts the password-stripped URI on argv. Dumps use
//! mongodump's gzipped `--archive` format, restorable with `mongorestore
//! --gzip --archive=<file>`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

/// What a dump attempt produced.
#[derive(Debug)]
pub enum DumpStatus {
    /// Archive written to this path.
    Dumped(PathBuf),
    /// No dump path was available; the string says why, for the operator log.
    Skipped(String),
}

/// The pieces of a libpq URL DSN a dump needs.
#[derive(Debug, PartialEq, Eq)]
pub struct DsnParts {
    pub user: String,
    pub password: String,
    pub dbname: String,
}

/// Parse `scheme://user:pass@host[:port]/dbname[?params]`. Pure, no
/// percent-decoding (the hub's own DSNs are plain). `None` when the shape is
/// not a userinfo+path URL or user/dbname is empty.
#[must_use]
pub fn parse_dsn(dsn: &str) -> Option<DsnParts> {
    let after = dsn.split("://").nth(1)?;
    let (userinfo, hostpath) = after.split_once('@')?;
    let (user, password) = userinfo.split_once(':')?;
    let (_, path) = hostpath.split_once('/')?;
    let dbname = path.split(['?', '/']).next().unwrap_or(path);
    if user.is_empty() || dbname.is_empty() {
        return None;
    }
    Some(DsnParts {
        user: user.to_owned(),
        password: password.to_owned(),
        dbname: dbname.to_owned(),
    })
}

/// The DSN with `:password` stripped from the userinfo, so it can sit on argv
/// (host `pg_dump --dbname`) without leaking the secret to `ps`.
#[must_use]
pub fn strip_password(dsn: &str) -> String {
    if let Some((scheme, after)) = dsn.split_once("://") {
        if let Some((userinfo, rest)) = after.split_once('@') {
            let user = userinfo.split_once(':').map_or(userinfo, |(u, _)| u);
            return format!("{scheme}://{user}@{rest}");
        }
    }
    dsn.to_owned()
}

/// Is a host `pg_dump` on PATH?
async fn host_pg_dump_available() -> bool {
    Command::new("pg_dump")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
}

/// Dump the database named by `dsn` to `out` in pg_dump custom format. Host
/// `pg_dump` first, then `docker exec` into `container` if given.
///
/// # Errors
/// Only when a dump path WAS available but the dump itself failed. A total
/// absence of any dump path is `Ok(Skipped(..))`, not an error.
pub async fn pg_dump(dsn: &str, out: &Path, container: Option<&str>) -> Result<DumpStatus, String> {
    let Some(parts) = parse_dsn(dsn) else {
        return Ok(DumpStatus::Skipped("unparseable DSN".to_owned()));
    };
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }

    // Path 1 — host pg_dump by (password-stripped) DSN; secret via PGPASSWORD.
    if host_pg_dump_available().await {
        let status = Command::new("pg_dump")
            .env("PGPASSWORD", &parts.password)
            .args(["--dbname", &strip_password(dsn), "-Fc", "-f"])
            .arg(out)
            .status()
            .await
            .map_err(|e| format!("host pg_dump: {e}"))?;
        return if status.success() {
            Ok(DumpStatus::Dumped(out.to_path_buf()))
        } else {
            Err(format!("host pg_dump exited {status}"))
        };
    }

    // Path 2 — docker exec; pg_dump writes the archive to stdout, captured into
    // the host file. `-e PGPASSWORD` (name only) forwards it from our env, so
    // the password never appears in docker's argv.
    if let Some(c) = container {
        let file =
            std::fs::File::create(out).map_err(|e| format!("create {}: {e}", out.display()))?;
        let status = Command::new("docker")
            .env("PGPASSWORD", &parts.password)
            .args([
                "exec",
                "-e",
                "PGPASSWORD",
                c,
                "pg_dump",
                "-U",
                &parts.user,
                "-Fc",
                &parts.dbname,
            ])
            .stdout(Stdio::from(file))
            .stderr(Stdio::null())
            .status()
            .await
            .map_err(|e| format!("docker exec pg_dump: {e}"))?;
        return if status.success() {
            Ok(DumpStatus::Dumped(out.to_path_buf()))
        } else {
            let _ = std::fs::remove_file(out); // never leave a truncated archive
            Err(format!("docker exec pg_dump exited {status}"))
        };
    }

    Ok(DumpStatus::Skipped(
        "no host pg_dump and no DB container configured".to_owned(),
    ))
}

/// The password from a `mongodb://user:pass@host/…` URI, if one is present and
/// non-empty. `None` for an auth-less URI (`mongodb://host:port`).
#[must_use]
pub fn mongo_uri_password(uri: &str) -> Option<String> {
    let after = uri.split("://").nth(1)?;
    let (userinfo, _) = after.split_once('@')?;
    let (_, pass) = userinfo.split_once(':')?;
    (!pass.is_empty()).then(|| pass.to_owned())
}

/// Is a host `mongodump` on PATH?
async fn host_mongodump_available() -> bool {
    Command::new("mongodump")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
}

/// Dump the Mongo database at `uri` to `out` as a gzipped `mongodump` archive.
/// Host `mongodump` first, then `docker exec` into `container` if given.
///
/// # Errors
/// Only when a dump path WAS available but the dump itself failed. A total
/// absence of any dump path, or an unparseable URI, is `Ok(Skipped(..))`.
pub async fn mongo_dump(
    uri: &str,
    out: &Path,
    container: Option<&str>,
) -> Result<DumpStatus, String> {
    if uri.split("://").nth(1).is_none() {
        return Ok(DumpStatus::Skipped("unparseable Mongo URI".to_owned()));
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }

    // Path 1 — host mongodump. Keep any password off argv via a 0600 --config
    // file; an auth-less URI needs none and goes straight on --uri.
    if host_mongodump_available().await {
        let mut cmd = Command::new("mongodump");
        let cfg_guard = if let Some(pw) = mongo_uri_password(uri) {
            let cfg = out.with_extension("dumpcfg");
            write_mongo_config(&cfg, &pw)?;
            cmd.arg(format!("--uri={}", strip_password(uri)));
            cmd.arg(format!("--config={}", cfg.display()));
            Some(cfg)
        } else {
            cmd.arg(format!("--uri={uri}"));
            None
        };
        let status = cmd
            .arg(format!("--archive={}", out.display()))
            .arg("--gzip")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        if let Some(cfg) = cfg_guard {
            let _ = std::fs::remove_file(cfg); // the password must not linger on disk
        }
        let status = status.map_err(|e| format!("host mongodump: {e}"))?;
        return if status.success() {
            Ok(DumpStatus::Dumped(out.to_path_buf()))
        } else {
            let _ = std::fs::remove_file(out);
            Err(format!("host mongodump exited {status}"))
        };
    }

    // Path 2 — docker exec; mongodump writes the archive to stdout, captured
    // into the host file. A host --config file is unreachable from inside the
    // container, so the URI (auth-less in the standard local deployment) goes
    // on argv, exposed only to processes within that container.
    if let Some(c) = container {
        let file =
            std::fs::File::create(out).map_err(|e| format!("create {}: {e}", out.display()))?;
        let status = Command::new("docker")
            .args(["exec", c, "mongodump", &format!("--uri={uri}"), "--archive", "--gzip"])
            .stdout(Stdio::from(file))
            .stderr(Stdio::null())
            .status()
            .await
            .map_err(|e| format!("docker exec mongodump: {e}"))?;
        return if status.success() {
            Ok(DumpStatus::Dumped(out.to_path_buf()))
        } else {
            let _ = std::fs::remove_file(out);
            Err(format!("docker exec mongodump exited {status}"))
        };
    }

    Ok(DumpStatus::Skipped(
        "no host mongodump and no Mongo container configured".to_owned(),
    ))
}

/// Write a 0600 mongodump `--config` file carrying just the password (its only
/// officially supported field), so the secret never touches argv.
fn write_mongo_config(path: &Path, password: &str) -> Result<(), String> {
    // Single-quote and escape for YAML; mongodump reads `password: <value>`.
    let escaped = password.replace('\'', "''");
    std::fs::write(path, format!("password: '{escaped}'\n"))
        .map_err(|e| format!("write mongo config {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_url_dsn() {
        let p = parse_dsn("postgres://coxagent:s3cr3t@127.0.0.1:5433/coxagent").unwrap();
        assert_eq!(p.user, "coxagent");
        assert_eq!(p.password, "s3cr3t");
        assert_eq!(p.dbname, "coxagent");
    }

    #[test]
    fn dbname_drops_the_query_string() {
        let p = parse_dsn("postgres://u:p@db:5432/mydb?sslmode=require").unwrap();
        assert_eq!(p.dbname, "mydb");
    }

    #[test]
    fn rejects_a_shapeless_dsn() {
        assert!(parse_dsn("not a dsn").is_none());
        assert!(parse_dsn("postgres://nopassword@db/x").is_none());
    }

    #[test]
    fn strip_password_removes_only_the_secret() {
        assert_eq!(
            strip_password("postgres://u:p@h:5432/db?x=1"),
            "postgres://u@h:5432/db?x=1"
        );
        // Nothing to strip → unchanged.
        assert_eq!(strip_password("host=db dbname=x"), "host=db dbname=x");
    }

    #[tokio::test]
    async fn skips_gracefully_with_no_tool_and_no_container() {
        // An unparseable DSN can never dump → Skipped, never Err.
        let out = std::env::temp_dir().join("cox-db-dump-probe.dump");
        let st = pg_dump("garbage", &out, None).await.unwrap();
        assert!(matches!(st, DumpStatus::Skipped(_)));
    }

    #[test]
    fn mongo_password_only_when_present() {
        assert_eq!(
            mongo_uri_password("mongodb://u:s3cr3t@127.0.0.1:27017/db"),
            Some("s3cr3t".to_owned())
        );
        // Auth-less local URI — the common case — has no password.
        assert_eq!(mongo_uri_password("mongodb://127.0.0.1:27017"), None);
        // Username but empty password is not a password.
        assert_eq!(mongo_uri_password("mongodb://u:@host/db"), None);
    }

    #[test]
    fn strip_password_handles_a_mongo_uri() {
        assert_eq!(
            strip_password("mongodb://u:p@127.0.0.1:27017/db"),
            "mongodb://u@127.0.0.1:27017/db"
        );
    }

    #[tokio::test]
    async fn mongo_dump_skips_an_unparseable_uri() {
        let out = std::env::temp_dir().join("cox-mongo-dump-probe.dump");
        let st = mongo_dump("garbage", &out, None).await.unwrap();
        assert!(matches!(st, DumpStatus::Skipped(_)));
    }
}
