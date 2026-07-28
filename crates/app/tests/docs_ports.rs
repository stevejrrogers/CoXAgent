//! COX-B011 regression guard: the docs must point at the port the deploy
//! actually publishes.
//!
//! The bug: COX-B001 moved the published host port from 4000 to 8101 in
//! docker-compose.yml, but README's "Self-host with Docker" section kept
//! telling readers to open `http://localhost:4000`. Following the README
//! verbatim gives connection refused — the same "app is down" symptom
//! COX-B001 fixed, reproduced through stale docs instead of a bad mapping.
//!
//! `deploy_smoke` proves the stack answers on the published port, but it is
//! `#[ignore]`d (it builds an image) and it reads the port from a constant,
//! not from the docs. This test is cheap, always runs, and ties the three
//! sources of truth together: the assigned port, the compose mapping, and
//! every URL the README hands the reader.
//!
//! The comparison lives in `docs_agree_with_deploy`, a pure function over the
//! two file bodies that returns `Err` instead of panicking. The tests at the
//! bottom feed it synthetic docs/compose pairs to prove the guard actually
//! bites: moving either side alone is caught, and a missing section or a
//! missing port mapping fails rather than silently passing.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

/// This project's assigned deploy port — fixed so it never collides with
/// another hub on the same docker host. See docker-compose.yml's header.
const ASSIGNED_HOST_PORT: u16 = 8101;

/// The README section that documents the Docker bring-up.
const SELF_HOST_HEADING: &str = "### Self-host with Docker";

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

/// Host-side ports the `coxagent` service publishes. A compose mapping is
/// `[host_ip:]host:container[/proto]`, so the host port is the field just
/// before the container port.
fn published_host_ports(compose_src: &str) -> Result<Vec<u16>, String> {
    let doc: serde_yaml::Value =
        serde_yaml::from_str(compose_src).map_err(|e| format!("docker-compose.yml: {e}"))?;
    let ports = doc["services"]["coxagent"]["ports"]
        .as_sequence()
        .ok_or_else(|| "docker-compose.yml: services.coxagent.ports must be a list".to_string())?;
    if ports.is_empty() {
        return Err("docker-compose.yml publishes no port for the coxagent service".to_string());
    }
    ports
        .iter()
        .map(|entry| {
            let mapping = entry
                .as_str()
                .ok_or_else(|| "port mapping must be a string like \"8101:4000\"".to_string())?;
            let fields: Vec<&str> = mapping.split('/').next().unwrap().split(':').collect();
            if fields.len() < 2 {
                return Err(format!(
                    "port mapping `{mapping}` publishes no explicit host port"
                ));
            }
            fields[fields.len() - 2]
                .parse::<u16>()
                .map_err(|e| format!("port mapping `{mapping}`: bad host port ({e})"))
        })
        .collect()
}

/// The body of a markdown section, from its heading up to the next heading of
/// the same or higher level.
fn section<'a>(md: &'a str, heading: &str) -> Result<&'a str, String> {
    let start = md
        .find(heading)
        .ok_or_else(|| format!("README.md: `{heading}` section is gone — docs guard is blind"))?;
    let body = &md[start + heading.len()..];
    let end = ["\n## ", "\n### "]
        .iter()
        .filter_map(|next| body.find(next))
        .min()
        .unwrap_or(body.len());
    Ok(&body[..end])
}

/// Every port a reader would type, from `localhost:<port>` / `127.0.0.1:<port>`
/// occurrences in `text`.
fn documented_ports(text: &str) -> Vec<u16> {
    let mut out = Vec::new();
    for host in ["localhost:", "127.0.0.1:"] {
        let mut rest = text;
        while let Some(at) = rest.find(host) {
            rest = &rest[at + host.len()..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if let Ok(port) = digits.parse::<u16>() {
                out.push(port);
            }
        }
    }
    out
}

/// The whole COX-B011 check: every URL the self-host section hands the reader
/// must resolve to a port the compose file actually publishes.
fn docs_agree_with_deploy(compose_src: &str, readme: &str) -> Result<(), String> {
    let published = published_host_ports(compose_src)?;
    let documented = documented_ports(section(readme, SELF_HOST_HEADING)?);

    if documented.is_empty() {
        return Err(format!(
            "README.md `{SELF_HOST_HEADING}` documents no URL to browse to — a reader \
             has nothing to follow, and this guard has nothing to check"
        ));
    }
    for port in &documented {
        if !published.contains(port) {
            return Err(format!(
                "README.md `{SELF_HOST_HEADING}` sends readers to localhost:{port}, but \
                 docker-compose.yml publishes {published:?} — following the README gives \
                 connection refused (COX-B011)"
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The guard, run against the real files.
// ---------------------------------------------------------------------------

#[test]
fn compose_publishes_the_projects_assigned_host_port() {
    let ports = published_host_ports(&read("docker-compose.yml")).unwrap();
    assert_eq!(
        ports,
        vec![ASSIGNED_HOST_PORT],
        "docker-compose.yml must publish the app on host port {ASSIGNED_HOST_PORT}"
    );
}

#[test]
fn readme_self_host_section_points_at_the_published_host_port() {
    if let Err(why) = docs_agree_with_deploy(&read("docker-compose.yml"), &read("README.md")) {
        panic!("{why}");
    }
}

#[test]
fn compose_header_comment_matches_the_mapping_it_documents() {
    let compose = read("docker-compose.yml");
    let published = published_host_ports(&compose).unwrap();
    let comments: String = compose
        .lines()
        .take_while(|l| l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");

    for port in documented_ports(&comments) {
        assert!(
            published.contains(&port),
            "docker-compose.yml's header says localhost:{port} but it publishes {published:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The guard, run against synthetic drift — proves it fails when it should.
// ---------------------------------------------------------------------------

fn compose_publishing(mapping: &str) -> String {
    format!("services:\n  coxagent:\n    image: coxagent\n    ports:\n      - \"{mapping}\"\n")
}

fn readme_documenting(self_host_body: &str) -> String {
    format!(
        "# CoXAgent\n\n### Single project (local)\n\n`serve` → localhost:4000\n\n\
         {SELF_HOST_HEADING}\n\n{self_host_body}\n\n## Engines\n\nnot a port section.\n"
    )
}

#[test]
fn matching_docs_and_deploy_pass() {
    let ok = docs_agree_with_deploy(
        &compose_publishing("8101:4000"),
        &readme_documenting("# → http://localhost:8101, log in as root"),
    );
    assert_eq!(ok, Ok(()), "matching docs and deploy must not be flagged");
}

#[test]
fn a_deploy_that_moves_away_from_the_docs_is_caught() {
    // Deploy side moves alone: compose republishes on 9999, docs still say 8101.
    let why = docs_agree_with_deploy(
        &compose_publishing("9999:4000"),
        &readme_documenting("# → http://localhost:8101, log in as root"),
    )
    .unwrap_err();
    assert!(why.contains("localhost:8101"), "unhelpful message: {why}");
    assert!(why.contains("9999"), "unhelpful message: {why}");
}

#[test]
fn docs_that_move_away_from_the_deploy_are_caught() {
    // Docs side moves alone: the original COX-B011 bug, README stuck on 4000.
    let why = docs_agree_with_deploy(
        &compose_publishing("8101:4000"),
        &readme_documenting("# → http://localhost:4000, log in as root"),
    )
    .unwrap_err();
    assert!(why.contains("localhost:4000"), "unhelpful message: {why}");
    assert!(why.contains("COX-B011"), "unhelpful message: {why}");
}

#[test]
fn ports_documented_outside_the_self_host_section_are_not_compared() {
    // `serve` legitimately runs on 4000 outside a container; only the
    // self-host section is held to the published mapping.
    let ok = docs_agree_with_deploy(
        &compose_publishing("8101:4000"),
        &readme_documenting("# → http://127.0.0.1:8101"),
    );
    assert_eq!(
        ok,
        Ok(()),
        "the `serve` line's localhost:4000 must not be compared against the mapping"
    );
}

#[test]
fn a_missing_self_host_section_fails_rather_than_passes() {
    let readme = "# CoXAgent\n\n## Engines\n\nno self-host docs at all.\n";
    let why = docs_agree_with_deploy(&compose_publishing("8101:4000"), readme).unwrap_err();
    assert!(why.contains(SELF_HOST_HEADING), "unhelpful message: {why}");
}

#[test]
fn a_self_host_section_with_no_url_is_caught() {
    let why = docs_agree_with_deploy(
        &compose_publishing("8101:4000"),
        &readme_documenting("Run `docker compose up -d --build`. Somehow."),
    )
    .unwrap_err();
    assert!(why.contains("no URL"), "unhelpful message: {why}");
}

#[test]
fn a_compose_service_that_publishes_nothing_fails_rather_than_passes() {
    let compose = "services:\n  coxagent:\n    image: coxagent\n";
    let why = docs_agree_with_deploy(
        compose,
        &readme_documenting("# → http://localhost:8101, log in as root"),
    )
    .unwrap_err();
    assert!(why.contains("ports must be a list"), "unhelpful: {why}");
}

#[test]
fn a_mapping_with_no_host_port_fails_rather_than_passes() {
    // `- "4000"` publishes the container port on a random host port — a
    // reader has no URL to trust, so the guard must reject it.
    let why = docs_agree_with_deploy(
        &compose_publishing("4000"),
        &readme_documenting("# → http://localhost:8101, log in as root"),
    )
    .unwrap_err();
    assert!(why.contains("no explicit host port"), "unhelpful: {why}");
}
