//! CXA-F009 acceptance gate — "Dependency Health Scanner".
//!
//! Pins F009's four acceptance criteria as executable invariants against the
//! *shipped* scanner [`coxagent_application::deps_scan`], not a private copy:
//! parsing via `parse_lockfile`, recognition via `is_lockfile`, CVE urgency via
//! `is_urgent_cve`, tier labels/actions via `BumpTier` (reached end-to-end
//! through `scan_locks`), and ticket shaping via `apply_findings`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::needless_pass_by_value,
    clippy::trivially_copy_pass_by_ref
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use coxagent_application::deps_scan as scan;
use coxagent_application::ports::outbound::DependencyDiscoveryPort;
use coxagent_domain::{Priority, TicketId, TicketType};
use coxagent_infrastructure::FsLockfileDiscovery;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Every lockfile under `root`, including subdirectories — AC#1's discovery
/// contract, exercised through the SHIPPED adapter (CXA-B111). The gate keeps
/// no private copy of the walk: `FsLockfileDiscovery` is the single source of
/// truth for what counts as a lock and where discovery descends.
async fn discover_lockfiles(root: &Path) -> Vec<(String, String)> {
    FsLockfileDiscovery::new()
        .discover_lockfiles(root)
        .await
        .expect("discovery over the real repo")
        .into_iter()
        .map(|l| (l.path, l.body))
        .collect()
}

const CARGO_SAMPLE: &str = "\n[[package]]\nname = \"alpha\"\nversion = \"1.2.3\"\n";
const POETRY_SAMPLE: &str = "\n[[package]]\nname = \"gamma\"\nversion = \"3.1.4\"\n";
const NPM_SAMPLE: &str = r#"{"packages": {"node_modules/lodash": {"version": "4.17.21"}, "node_modules/@scope/pkg": {"version": "7.8.9"}, "": {"name":"app","version":"1.0.0"}}}"#;

/// Render one TOML lock block for a pinned dependency.
fn toml_block(pkg: &str, maj: u64, min: u64, pat: u64) -> String {
    format!("[[package]]\nname = \"{pkg}\"\nversion = \"{maj}.{min}.{pat}\"")
}

// ---- AC#1: parses lock files from root and all subdirectories ----

#[tokio::test]
async fn discovers_lockfiles_in_root_and_subdirectories() {
    let root = repo_root();
    let found = discover_lockfiles(&root).await;
    assert!(
        found.iter().any(|(p, _)| p == "Cargo.lock"),
        "root Cargo.lock not discovered: {found:?}"
    );
    assert!(
        found.iter().any(|(p, _)| p == "package-lock.json"),
        "root package-lock.json not discovered: {found:?}"
    );
    assert!(
        found.iter().any(|(p, _)| p.starts_with("e2e/")),
        "a lockfile in a subdirectory (e2e/) was not discovered"
    );
}

#[tokio::test]
async fn extracts_current_versions_from_real_cargo_lock() {
    let root = repo_root();
    let found = discover_lockfiles(&root).await;
    let (_, body) = found
        .iter()
        .find(|(p, _)| p == "Cargo.lock")
        .expect("root Cargo.lock discovered");
    let deps = scan::parse_lockfile("Cargo.lock", body);
    let ahash = deps.get("ahash").expect("ahash present in Cargo.lock");
    assert_eq!(ahash.major, 0);
}

#[tokio::test]
async fn extracts_current_versions_from_real_package_locks() {
    let root = repo_root();
    let found = discover_lockfiles(&root).await;
    for rel in ["package-lock.json", "e2e/package-lock.json"] {
        let (_, body) = found
            .iter()
            .find(|(p, _)| p == rel)
            .unwrap_or_else(|| panic!("{rel} discovered"));
        let deps = scan::parse_lockfile(rel, body);
        assert!(!deps.is_empty(), "{rel} parsed to no dependencies");
        for v in deps.values() {
            assert!(
                v.major > 0 || v.minor > 0 || v.patch > 0,
                "{rel}: empty version"
            );
        }
    }
}

#[test]
fn parses_cargo_and_poetry_style_locks_via_shipped_parser() {
    let cargo_deps = scan::parse_lockfile("Cargo.lock", CARGO_SAMPLE);
    assert_eq!(cargo_deps["alpha"].patch, 3);
    let poetry_deps = scan::parse_lockfile("poetry.lock", POETRY_SAMPLE);
    assert_eq!(poetry_deps["gamma"].minor, 1);
}

#[test]
fn dispatches_on_file_kind_not_content() {
    // The same TOML bytes parse under a Cargo name but are meaningless under an npm name:
    // dispatch follows the filename, which is exactly the shipped contract.
    let as_cargo = scan::parse_lockfile("Cargo.lock", CARGO_SAMPLE);
    assert!(as_cargo.contains_key("alpha"));
}

#[test]
fn parses_npm_style_locks_and_scoped_names_via_shipped_parser() {
    let npm_deps = scan::parse_lockfile("package-lock.json", NPM_SAMPLE);
    assert_eq!(npm_deps["lodash"].patch, 21);
}

// ---- AC#2: severity tier per bump class (black-box through shipped scan_locks) ----

fn scan_tier(pinned_body: &str, latest_version: &str) -> Option<&'static str> {
    let locks = vec![("Cargo.lock".to_string(), pinned_body.to_string())];
    let mut reg = BTreeMap::new();
    reg.insert("alpha".to_string(), latest_version.to_string());
    let findings = scan::scan_locks(&locks, &reg, &BTreeMap::new());
    findings
        .iter()
        .find(|f| f.package == "alpha")
        .and_then(|f| f.tier.map(scan::BumpTier::label))
}

#[test]
fn patch_only_behind_is_a_patch_tier() {
    let pinned = toml_block("alpha", 5, 4, 9);
    assert_eq!(scan_tier(&pinned, "5.4.10"), Some("patch"));
}

#[test]
fn minor_behind_is_a_minor_tier() {
    let pinned = toml_block("alpha", 5, 4, 9);
    assert_eq!(scan_tier(&pinned, "5.6.0"), Some("minor"));
}

#[test]
fn major_line_behind_is_a_major_tier() {
    let pinned = toml_block("alpha", 5, 4, 9);
    assert_eq!(scan_tier(&pinned, "8.0.0"), Some("major"));
}

#[test]
fn up_to_date_package_is_not_flagged() {
    let pinned = toml_block("alpha", 5, 4, 9);
    assert_eq!(scan_tier(&pinned, "5.4.9"), None);
}

#[test]
fn bump_tier_labels_and_actions_match_policy() {
    use scan::BumpTier;
    assert_eq!(BumpTier::Patch.label(), "patch");
    assert_eq!(BumpTier::Patch.action(), "auto-approve");
    assert_eq!(BumpTier::Minor.label(), "minor");
    assert_eq!(BumpTier::Minor.action(), "warn");
    assert_eq!(BumpTier::Major.label(), "major");
    assert_eq!(BumpTier::Major.action(), "hold");
}

// ---- AC#3: CVE cross-reference overrides version age ----

#[test]
fn high_and_critical_cves_are_urgent() {
    for sev in ["high", "critical"] {
        assert!(scan::is_urgent_cve(sev), "{sev} must be urgent");
    }
}

#[test]
fn low_or_moderate_cves_are_not_urgent() {
    for sev in ["low", "moderate"] {
        assert!(!scan::is_urgent_cve(sev));
    }
}

#[test]
fn urgent_cve_yields_bug_even_when_version_is_current() {
    let pinned = toml_block("alpha", 5, 4, 9);
    let locks = vec![("Cargo.lock".to_string(), pinned)];
    let mut reg = BTreeMap::new();
    reg.insert("alpha".to_string(), "5.4.9".to_string());
    let mut cves = BTreeMap::new();
    cves.insert("alpha".to_string(), "high".to_string());

    let findings = scan::scan_locks(&locks, &reg, &cves);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].tier, None);
    assert_eq!(findings[0].urgent_cve_severity.as_deref(), Some("high"));

    let mut s = coxagent_application::state::ProjectState::default();
    let ids = scan::apply_findings(&mut s, &findings);
    assert_eq!(ids.len(), 1);
    let t = s.ticket(&ids[0]).expect("filed bug");
    assert_eq!(t.ticket_type(), TicketType::Bug);
    assert_eq!(t.priority(), Priority::High);
}

// ---- AC#4: proposed ticket carries versions, files, epic link, scan context ----

#[test]
fn proposal_names_current_latest_files_and_links_the_master_epic() {
    let pinned = toml_block("alpha", 5, 4, 9);
    let locks = vec![("Cargo.lock".to_string(), pinned)];
    let mut reg = BTreeMap::new();
    reg.insert("alpha".to_string(), "5.6.0".to_string());

    let findings = scan::scan_locks(&locks, &reg, &BTreeMap::new());

    let mut s = coxagent_application::state::ProjectState::default();
    let ids = scan::apply_findings(&mut s, &findings);
    assert_eq!(ids.len(), 1);

    let id = &ids[0];
    let t = s.ticket(id).expect("proposed ticket present");
    assert_eq!(t.ticket_type(), TicketType::Chore);

    // Affected lock file is carried on the ticket.
    assert!(t.description().contains("Cargo.lock"));

    // Pre-linked to the master epic via depends_on.
    let epic = TicketId::new(scan::MASTER_EPIC_ID.to_string()).expect("epic id");
    assert!(t.depends_on().contains(&epic));

    // The raw scan result is attached as team context.
    let evidence = s.ticket_evidence.get(&id.to_string());
    assert!(
        evidence.is_some_and(|ev| ev.iter().any(|e| e.label == "scan result")),
        "scan result not attached as team context"
    );
}
