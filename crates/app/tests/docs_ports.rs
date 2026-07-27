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

/// The guard itself, over the two texts rather than the two files: every port
/// the self-host section hands the reader must be one the deploy publishes.
///
/// `Err` carries the message the test fails with. Structural faults — no
/// self-host section, no port mapping — panic out of the helpers instead, so a
/// blinded guard is loud rather than vacuously green.
fn docs_agree_with_deploy(readme: &str, compose_src: &str) -> Result<(), String> {
    let published = published_host_ports(compose_src);
    let documented = documented_ports(section(readme, SELF_HOST_HEADING));

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

#[test]
fn readme_self_host_section_points_at_the_published_host_port() {
    if let Err(why) = docs_agree_with_deploy(&read("README.md"), &read("docker-compose.yml")) {
        panic!("{why}");
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

/// The guard, guarded. The tests above only prove the repo is consistent
/// *right now*; they say nothing about whether the comparison would still
/// notice if it stopped being consistent. These run the same comparison over
/// synthetic docs — the ticket's own repro among them — so drift on either
/// side, and a docs section or port mapping that goes missing, are each proven
/// to fail rather than assumed to.
mod the_guard_itself {
    use super::{docs_agree_with_deploy, ASSIGNED_HOST_PORT, SELF_HOST_HEADING};

    /// A README whose self-host section sends the reader to `port`.
    fn readme_pointing_at(port: u16) -> String {
        format!(
            "# CoXAgent\n\n## Quickstart\n\n{SELF_HOST_HEADING}\n\n\
             ```sh\ndocker compose up -d --build\n# → http://localhost:{port}, log in as root\n```\n\n\
             ### Enterprise\n\nSomething else entirely.\n"
        )
    }

    /// A compose file publishing `host` on the container's 4000.
    fn compose_publishing(host: u16) -> String {
        format!("services:\n  coxagent:\n    ports:\n      - \"{host}:4000\"\n")
    }

    #[test]
    fn matching_docs_and_deploy_pass() {
        assert!(docs_agree_with_deploy(
            &readme_pointing_at(ASSIGNED_HOST_PORT),
            &compose_publishing(ASSIGNED_HOST_PORT),
        )
        .is_ok());
    }

    /// The ticket's repro: the deploy moved to 8101, the docs stayed on 4000.
    #[test]
    fn stale_docs_against_a_moved_deploy_are_caught() {
        let why = docs_agree_with_deploy(
            &readme_pointing_at(4000),
            &compose_publishing(ASSIGNED_HOST_PORT),
        )
        .expect_err("README on 4000 vs compose on 8101 must not pass");
        assert!(why.contains("localhost:4000"), "unhelpful message: {why}");
        assert!(why.contains("8101"), "unhelpful message: {why}");
    }

    /// The mirror image — the docs are right and the mapping drifted. Either
    /// side moving alone has to fail; only moving both together may pass.
    #[test]
    fn a_deploy_that_moves_away_from_the_docs_is_caught() {
        docs_agree_with_deploy(
            &readme_pointing_at(ASSIGNED_HOST_PORT),
            &compose_publishing(9999),
        )
        .expect_err("compose on 9999 vs README on 8101 must not pass");
    }

    #[test]
    fn a_self_host_section_with_no_url_is_caught() {
        let readme = format!("# CoXAgent\n\n{SELF_HOST_HEADING}\n\nRun it somehow.\n");
        docs_agree_with_deploy(&readme, &compose_publishing(ASSIGNED_HOST_PORT))
            .expect_err("a section that names no URL leaves the reader nowhere to go");
    }

    /// Only the *self-host* section is in scope: the native `serve` path
    /// documents localhost:4000 legitimately and must not be dragged in.
    #[test]
    fn ports_documented_outside_the_self_host_section_are_not_compared() {
        let readme = format!(
            "# CoXAgent\n\n## Quickstart\n\ncoxagent serve   # → localhost:4000\n\n\
             {SELF_HOST_HEADING}\n\nOpen http://localhost:{ASSIGNED_HOST_PORT}.\n"
        );
        assert!(
            docs_agree_with_deploy(&readme, &compose_publishing(ASSIGNED_HOST_PORT)).is_ok(),
            "the non-Docker serve path binds 4000 directly — out of scope here"
        );
    }

    #[test]
    #[should_panic(expected = "section is gone")]
    fn a_missing_self_host_section_fails_rather_than_passes() {
        let _ = docs_agree_with_deploy(
            "# CoXAgent\n\n## Quickstart\n\nNo Docker section at all.\n",
            &compose_publishing(ASSIGNED_HOST_PORT),
        );
    }

    #[test]
    #[should_panic(expected = "ports must be a list")]
    fn a_compose_service_that_publishes_nothing_fails_rather_than_passes() {
        let _ = docs_agree_with_deploy(
            &readme_pointing_at(ASSIGNED_HOST_PORT),
            "services:\n  coxagent:\n    image: coxagent\n",
        );
    }

    /// `- "4000"` publishes to an ephemeral host port, not to 4000. Reading the
    /// container port as if it were the host port would make the guard bless a
    /// mapping no reader can reach.
    #[test]
    #[should_panic(expected = "no explicit host port")]
    fn a_mapping_with_no_host_port_fails_rather_than_passes() {
        let _ = docs_agree_with_deploy(
            &readme_pointing_at(4000),
            "services:\n  coxagent:\n    ports:\n      - \"4000\"\n",
        );
    }
}
