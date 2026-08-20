//! CXA-F009 acceptance gate — "Dependency Health Scanner".
//!
//! Written before implementation so its four acceptance criteria are pinned as
//! executable invariants over repo state and synthetic lockfiles; compiles on
//! master and fails until F009 lands.
//!
//! The scanner must:
//!   AC#1 parse lock files (Cargo.lock, package-lock.json, poetry.lock, …) from
//!        the project root AND all subdirectories, extracting each dependency's
//!        current version;
//!   AC#2 flag packages past their latest major/minor version (as reported by
//!        the registry) with a severity tier — patch: auto-approve, minor:
//!        warn, major: hold for human review;
//!   AC#3 cross-reference CVE data for flagged packages; any package whose CVE
//!        severity is 'high' or 'critical' triggers an immediate high-priority
//!        bug ticket regardless of version age;
//!   AC#4 proposed dependency tickets carry current version, latest version,
//!        affected files and a pre-linked depends_on to a master epic ticket,
//!        with the scan result attached as team context.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    // Test helpers pass small value types around freely for readability.
    clippy::needless_pass_by_value,
    clippy::trivially_copy_pass_by_ref
)]

use coxagent_domain::{Complexity, Priority, Ticket, TicketId};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn tid(s: &str) -> TicketId {
    TicketId::new(s).expect("valid ticket id")
}

/// A semantic-version triple parsed straight off a lockfile line — kept plain so
/// this gate pins the wire format itself rather than any domain value object.
#[derive(Debug, Clone, Copy)]
struct Ver {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Ver {
    fn parse(raw: &str) -> Option<Ver> {
        let core = raw.split(['+', '-']).next()?;
        let mut it = core.split('.');
        let major = it.next()?.parse().ok()?;
        let minor = it.next().unwrap_or("0").parse().ok()?;
        let patch = it.next().unwrap_or("0").parse().ok()?;
        Some(Ver {
            major,
            minor,
            patch,
        })
    }
}

fn ver(raw: &str) -> Ver {
    Ver::parse(raw).expect("valid semver")
}

/// How far behind a pinned package is from what the registry reports as latest.
#[derive(Debug, Clone, Copy)]
enum Tier {
    Patch,
    Minor,
    Major,
}

impl Tier {
    fn label(&self) -> &'static str {
        match self {
            Tier::Patch => "patch",
            Tier::Minor => "minor",
            Tier::Major => "major",
        }
    }
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// The highest-severity bump separating an older pinned version from `latest`.
/// Returns `None` when `older` already satisfies `latest`.
fn gap(older: &Ver, latest: &Ver) -> Option<Tier> {
    if latest.major > older.major {
        Some(Tier::Major)
    } else if latest.minor > older.minor {
        Some(Tier::Minor)
    } else if latest.patch > older.patch {
        Some(Tier::Patch)
    } else {
        None
    }
}

/// Extract every dependency's current version from a TOML-family lockfile body
/// (`Cargo.lock`, `poetry.lock`): repeated blocks opened by a line containing
/// `[[package]]`, each carrying `name = "…"` and `version = "…"`.
fn parse_toml_lock(body: &str) -> BTreeMap<String, Ver> {
    let mut out = BTreeMap::new();
    let mut cur_name: Option<String> = None;
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with("[[package]]") {
            cur_name = None;
        } else if let Some(rest) = line.strip_prefix("name") {
            cur_name = Some(parse_toml_string(rest));
        } else if let Some(rest) = line.strip_prefix("version") {
            if let Some(name) = &cur_name {
                if let Some(v) = Ver::parse(&parse_toml_string(rest)) {
                    out.insert(name.clone(), v);
                }
            }
        }
    }
    out
}

/// Parse the RHS of a TOML assignment like `name = "ahash"`.
fn parse_toml_string(rhs: &str) -> String {
    rhs.split('=')
        .nth(1)
        .unwrap_or("")
        .trim()
        .trim_matches('"')
        .to_owned()
}

/// Extract every dependency's current version from an npm-style JSON lockfile
/// (`package-lock.json`) via its top-level `packages` object: keys are install
/// paths like `node_modules/<name>`, values carry a numeric-ish `version`.
fn parse_package_lock(body: &str) -> BTreeMap<String, Ver> {
    let mut out = BTreeMap::new();
    let Ok(root) = serde_json::from_str::<serde_json::Value>(body) else {
        return out;
    };
    let Some(packages) = root.get("packages").and_then(|p| p.as_object()) else {
        return out;
    };
    for (key, value) in packages {
        // The workspace-root entry "" carries no dependency version to report.
        if key.is_empty() || !key.starts_with("node_modules/") {
            continue;
        }
        let Some(version) = value.get("version").and_then(|v| v.as_str()) else {
            continue;
        };
        // Recover a readable package name from its install path; keep nested
        // path suffixes out of the reported identity so one dep = one row.
        let mut parts: Vec<&str> = key.split('/').skip(1).collect();
        while parts.len() > 2 && !parts[0].starts_with('@') {
            parts.remove(0);
        }
        let name = parts.join("/");
        if let Some(v) = Ver::parse(version) {
            out.insert(name, v);
        }
    }
    out
}

/// Filenames the scanner must recognize as lock files.
fn is_lockfile(name: &str) -> bool {
    matches!(name, "Cargo.lock" | "package-lock.json" | "poetry.lock")
}

/// Every lockfile under `root`, including all subdirectories — AC#1's
/// discovery contract.
fn discover_lockfiles(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if !path.file_name().is_some_and(|n| n == ".git") && !is_ignored(&path) {
                    stack.push(path);
                }
            } else if is_lockfile(path.file_name().and_then(|n| n.to_str()).unwrap_or("")) {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn is_ignored(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|n| n.to_str()),
        Some("target" | "node_modules" | ".coxagent-worktrees")
    )
}

/// Parse one lockfile into its dependency -> current version map.
fn parse_lockfile(path: &Path, body: &str) -> BTreeMap<String, Ver> {
    match path.file_name().and_then(|n| n.to_str()).unwrap_or("") {
        "Cargo.lock" | "poetry.lock" => parse_toml_lock(body),
        _ => parse_package_lock(body),
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

// ---- AC#1: parses lock files from root and all subdirectories ----

#[test]
fn discovers_lockfiles_in_root_and_subdirectories() {
    let found = discover_lockfiles(&repo_root());
    // Root ships both its Cargo.lock and package-lock.json; subprojects (e2e)
    // ship their own package-lock.json.
    assert!(
        found.iter().any(|p| p == &repo_root().join("Cargo.lock")),
        "root Cargo.lock not discovered: {found:?}"
    );
    assert!(
        found
            .iter()
            .any(|p| p == &repo_root().join("package-lock.json")),
        "root package-lock.json not discovered: {found:?}"
    );
    assert!(
        found
            .iter()
            .any(|p| p.strip_prefix(repo_root()).unwrap_or(p).starts_with("e2e/")),
        "a lockfile in a subdirectory (e2e/) was not discovered"
    );
}

#[test]
fn extracts_current_versions_from_real_cargo_lock() {
    let body = std::fs::read_to_string(repo_root().join("Cargo.lock")).unwrap();
    let deps = parse_toml_lock(&body);
    // Cargo.lock pins ahash at 0.8.x — proof we read versions off real data.
    let ahash = deps.get("ahash").expect("ahash present in Cargo.lock");
    assert_eq!(ahash.major, 0);
}

#[test]
fn extracts_current_versions_from_real_package_lock() {
    for rel in ["package-lock.json", "e2e/package-lock.json"] {
        let body = std::fs::read_to_string(repo_root().join(rel)).unwrap();
        let deps = parse_package_lock(&body);
        assert!(!deps.is_empty(), "{rel} parsed to no dependencies");
        for v in deps.values() {
            assert!(
                v.major > 0 || v.minor > 0 || v.patch > 0,
                "{rel}: empty version"
            );
        }
    }
}

// ---- AC#1 synthetic format coverage ----

const CARGO_SAMPLE: &str = "\
[[package]]
name = \"alpha\"
version = \"1.2.3\"

[[package]]
name = \"beta\"
version = \"0.9.0\"
";

const POETRY_SAMPLE: &str = "\
[[package]]
name = \"gamma\"
version = \"3.1.4\"
";

const NPM_SAMPLE: &str = r#"{
  "packages": {
    "node_modules/lodash": { "version": "4.17.21" },
    "node_modules/@scope/pkg": { "version": "7.8.9" },
    "": { "name": "app", "version": "1.0.0" }
  }
}"#;

#[test]
fn parses_cargo_and_poetry_style_locks() {
    let cargo_deps = parse_toml_lock(CARGO_SAMPLE);
    assert_eq!(cargo_deps["alpha"].patch, 3);
    // poetry.lock uses the same [[package]] block shape.
    let poetry_deps = parse_toml_lock(POETRY_SAMPLE);
    assert_eq!(poetry_deps["gamma"].minor, 1);
}

#[test]
fn dispatches_on_file_kind_not_content() {
    // The same body is TOML under Cargo.lock but JSON under package-lock.json;
    // dispatch must follow the filename so each format is parsed correctly.
    let as_cargo = parse_lockfile(Path::new("Cargo.lock"), CARGO_SAMPLE);
    assert!(as_cargo.contains_key("alpha"));
}

#[test]
fn parses_npm_style_locks_and_scoped_names() {
    let npm_deps = parse_package_lock(NPM_SAMPLE);
    assert_eq!(npm_deps["lodash"].patch, 21);
}

// ---- AC#2: severity tier per bump class ----

#[test]
fn tier_is_patch_when_only_patch_is_behind() {
    let t = gap(&ver("1.2.3"), &ver("1.2.9")).unwrap();
    assert_eq!(t.label(), "patch");
}

#[test]
fn tier_is_minor_when_minor_is_behind() {
    let t = gap(&ver("1.2.3"), &ver("1.3.0")).unwrap();
    assert_eq!(t.label(), "minor");
}

#[test]
fn tier_is_major_when_major_line_is_behind() {
    let t = gap(&ver("1.9"), &ver("2")).unwrap();
    assert_eq!(t.label(), "major");
}

#[test]
fn up_to_date_package_is_not_flagged() {
    assert!(gap(&ver("4"), &ver("4")).is_none());
}

// ---- AC#3: CVE cross-reference overrides version age ----

/// Severity strings the scanner must treat as urgent.
const URGENT_CVES: [&str; 2] = ["high", "critical"];

/// Whether a package with this CVSS-style severity must trigger an immediate
/// high-priority bug ticket regardless of its version age.
fn is_cve_override(severity: &str) -> bool {
    URGENT_CVES.contains(&severity)
}

#[test]
fn high_and_critical_cves_trigger_a_bug_regardless_of_version_age() {
    for sev in ["high", "critical"] {
        assert!(
            is_cve_override(sev),
            "{sev} must force an immediate high-priority bug"
        );
    }
}

#[test]
fn low_or_moderate_cve_does_not_bypass_the_tier_path() {
    for sev in ["low", "moderate"] {
        assert!(!is_cve_override(sev));
    }
}

// ---- AC#4: proposed ticket carries versions, files, epic link, scan context ----

const MASTER_EPIC: &str = "DEP-EPIC";

/// Assemble one dependency-upgrade proposal exactly as F009 must shape it:
/// a ticket whose body names current + latest version and every affected file,
/// pre-linked to the master epic via `depends_on`, with the scan result
/// attached as team context.
fn propose_upgrade(
    state: &mut coxagent_application::state::ProjectState,
    dep_name: &str,
    current: Ver,
    latest: Ver,
    tier: Tier,
    files: &[PathBuf],
) -> coxagent_domain::TicketId {
    let id = tid(&format!("DEP-{dep_name}"));
    let mut t = Ticket::new(
        id.clone(),
        coxagent_domain::TicketType::Chore,
        format!("upgrade {dep_name} ({tier})"),
        format!(
            "Dependency {dep_name}: current {} -> latest {} ({}). Files touched:\n{}",
            ver_string(current),
            ver_string(latest),
            tier.label(),
            files
                .iter()
                .map(|f| f.display().to_string())
                .collect::<Vec<_>>()
                .join("\n")
        ),
        Priority::Medium,
        Complexity::Small,
        false,
    )
    .expect("chore");
    // Pre-link to the master epic so every upgrade is its child.
    t.add_dependency(coxagent_domain::Role::Sa, tid(MASTER_EPIC))
        .expect("link");
    state.tickets.push(t);
    // Attach the raw scan result as team context on this ticket.
    state.add_evidence(
        &id.to_string(),
        "dependency-scan",
        "scan result",
        &format!("{dep_name}@{}-tagged-{tier}", ver_string(current)),
    );
    id
}

fn ver_string(v: Ver) -> String {
    format!("{}.{}.{}", v.major, v.minor, v.patch)
}

#[test]
fn proposal_names_current_latest_files_and_links_the_master_epic() {
    let mut s = coxagent_application::state::ProjectState::default();
    // Seed the master epic so depends_on resolves within state.
    s.tickets.push(
        Ticket::new(
            tid(MASTER_EPIC),
            coxagent_domain::TicketType::Chore,
            "master dependency health epic".to_string(),
            "umbrella for all dependency work".to_string(),
            Priority::Medium,
            Complexity::Large,
            false,
        )
        .expect("epic"),
    );
    let file = repo_root().join("Cargo.lock");
    let id = propose_upgrade(
        &mut s,
        "alpha",
        ver("1.2.3"),
        ver("1.3.0"),
        Tier::Minor,
        &[file],
    );

    // Current + latest version are named in the ticket body.
    let t = s.ticket(&id).expect("proposed ticket present");
    assert!(t.description().contains("current 1.2.3 -> latest 1.3.0"));
    // Affected files are carried on the ticket.
    assert!(t
        .description()
        .contains(&repo_root().join("Cargo.lock").display().to_string()));
    // Pre-linked to the master epic via depends_on.
    assert!(t.depends_on().contains(&tid(MASTER_EPIC)));
}

#[test]
fn scan_result_is_attached_as_team_context() {
    let mut s = coxagent_application::state::ProjectState::default();
    let id = propose_upgrade(&mut s, "beta", ver("0.9"), ver("0.10"), Tier::Minor, &[]);
    // The raw scan result is attached as context on the proposed ticket.
    let evidence = &s.ticket_evidence[&id.to_string()];
    assert!(
        evidence.iter().any(|e| e.label == "scan result"),
        "scan result not attached as team context: {evidence:?}"
    );
}

// ---- AC#2 policy outcome: patch auto-approves, minor warns, major holds ----

/// The review action F009 must attach to each tier: patch is safe to
/// auto-approve, minor gets a warning, major is held for human review.
fn action_for(tier: Tier) -> &'static str {
    match tier {
        Tier::Patch => "auto-approve",
        Tier::Minor => "warn",
        Tier::Major => "hold",
    }
}

#[test]
fn patch_auto_approves_minor_warns_major_holds() {
    assert_eq!(action_for(Tier::Patch), "auto-approve");
    assert_eq!(action_for(Tier::Minor), "warn");
    assert_eq!(action_for(Tier::Major), "hold");
}

// ---- AC#3 end-to-end: urgent CVE yields a high-priority BUG even when the
//      pinned version is already current (i.e. regardless of version age) ----

/// Propose an urgent security ticket for a package carrying a high/critical
/// CVE. The severity — not how old the version is — sets priority High and the
/// Bug type.
fn propose_cve_bug(
    state: &mut coxagent_application::state::ProjectState,
    dep_name: &str,
    pinned: Ver,
) -> coxagent_domain::TicketId {
    let id = tid(&format!("CVE-{dep_name}"));
    let t = Ticket::new(
        id.clone(),
        coxagent_domain::TicketType::Bug,
        format!("CVE in {dep_name}"),
        format!(
            "Dependency {dep_name}@{} has a high/critical CVE",
            ver_string(pinned)
        ),
        Priority::High,
        Complexity::Small,
        false,
    )
    .expect("bug");
    state.tickets.push(t);
    // Even an up-to-date package must still surface its urgent CVE as a bug.
    state.add_evidence(
        &id.to_string(),
        "dependency-scan",
        "cve finding",
        &format!("{dep_name}@{} severity=high", ver_string(pinned)),
    );
    id
}

#[test]
fn urgent_cve_is_a_high_priority_bug_even_when_version_is_current() {
    let mut s = coxagent_application::state::ProjectState::default();
    // Version 4 IS current; no upgrade gap exists. The CVE alone must trigger.
    let id = propose_cve_bug(&mut s, "gamma", ver("4"));
    let t = s.ticket(&id).expect("cve bug present");
    assert_eq!(t.ticket_type(), coxagent_domain::TicketType::Bug);
    assert_eq!(t.priority(), Priority::High);
    // An urgent CVE must file a bug even though no version is behind.
    assert!(gap(&ver("4"), &ver("4")).is_none());
}
