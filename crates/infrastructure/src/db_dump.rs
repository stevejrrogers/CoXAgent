//! Logical Postgres backups for the hub's databases.
//!
//! `coxagent backup` archives the file-backed stores, but the AUTHORITATIVE
//! data — project state, accounts — lives in Postgres, which that archive
//! cannot capture (it only printed a "run pg_dump yourself" warning). This
//! module dumps each configured DSN so one backup run protects the databases
//! too. It tries a host `pg_dump` first (the standard product posture — the
//! operator installs postgresql-client), then falls back to a `docker exec`
//! into the DB container so a containerized deployment with no host client is
//! still protected. If neither is available the caller gets a `Skipped` reason
//! to log — a missing dump tool is never a hard failure of the whole backup.
//!
//! Secrets: the password reaches the child only through the `PGPASSWORD` env
//! var — never on argv (which `ps` exposes). For `docker exec` that means
//! `-e PGPASSWORD` with NO `=value` (docker forwards it from our environment).
//! Dumps use pg_dump's custom format (`-Fc`), restorable with `pg_restore`.

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
}
