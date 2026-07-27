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
fn published_host_ports(compose_src: &str) -> Vec<u16> {
    let doc: serde_yaml::Value = serde_yaml::from_str(compose_src).unwrap();
    let ports = doc["services"]["coxagent"]["ports"]
        .as_sequence()
        .expect("docker-compose.yml: services.coxagent.ports must be a list");
    assert!(
        !ports.is_empty(),
        "docker-compose.yml publishes no port for the coxagent service"
    );
    ports
        .iter()
        .map(|entry| {
            let mapping = entry
                .as_str()
                .expect("port mapping must be a string like \"8101:4000\"");
            let fields: Vec<&str> = mapping.split('/').next().unwrap().split(':').collect();
            assert!(
                fields.len() >= 2,
                "port mapping `{mapping}` publishes no explicit host port"
            );
            fields[fields.len() - 2]
                .parse::<u16>()
                .unwrap_or_else(|e| panic!("port mapping `{mapping}`: bad host port ({e})"))
        })
        .collect()
}

/// The body of a markdown section, from its heading up to the next heading of
/// the same or higher level.
fn section<'a>(md: &'a str, heading: &str) -> &'a str {
    let start = md
        .find(heading)
        .unwrap_or_else(|| panic!("README.md: `{heading}` section is gone — docs guard is blind"));
    let body = &md[start + heading.len()..];
    let end = ["\n## ", "\n### "]
        .iter()
        .filter_map(|next| body.find(next))
        .min()
        .unwrap_or(body.len());
    &body[..end]
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

#[test]
fn compose_publishes_the_projects_assigned_host_port() {
    let ports = published_host_ports(&read("docker-compose.yml"));
    assert_eq!(
        ports,
        vec![ASSIGNED_HOST_PORT],
        "docker-compose.yml must publish the app on host port {ASSIGNED_HOST_PORT}"
    );
}

#[test]
fn readme_self_host_section_points_at_the_published_host_port() {
    let published = published_host_ports(&read("docker-compose.yml"));
    let readme = read("README.md");
    let documented = documented_ports(section(&readme, SELF_HOST_HEADING));

    assert!(
        !documented.is_empty(),
        "README.md `{SELF_HOST_HEADING}` documents no URL to browse to — a reader \
         has nothing to follow, and this guard has nothing to check"
    );
    for port in &documented {
        assert!(
            published.contains(port),
            "README.md `{SELF_HOST_HEADING}` sends readers to localhost:{port}, but \
             docker-compose.yml publishes {published:?} — following the README gives \
             connection refused (COX-B011)"
        );
    }
}

#[test]
fn compose_header_comment_matches_the_mapping_it_documents() {
    let compose = read("docker-compose.yml");
    let published = published_host_ports(&compose);
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
