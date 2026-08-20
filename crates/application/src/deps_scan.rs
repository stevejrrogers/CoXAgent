//! Dependency Health Scanner (CXA-F009) — pure core.
//!
//! Lock files (`Cargo.lock`, `package-lock.json`, `poetry.lock`) are the source
//! of truth for what a project ships, not manifest files (a manifest says what
//! *may* be used; a lock says what *is* used). This module parses those locks,
//! compares each pinned version against a registry snapshot of known-latest
//! versions plus any CVE data supplied for it, classifies how far behind each
//! package is (patch / minor / major), and proposes remediation tickets: a
//! low-priority chore linked to a master 'Dependency Audit' epic for routine
//! upgrades, or an immediate high-priority bug when a package carries an urgent
//! CVE regardless of its version age.
//!
//! This module is PURE: every function takes already-read content strings and
//! lookup maps and returns data or mutates [`ProjectState`]. IO — discovering
//! lockfiles under the tree and reading them — lives in an adapter per the
//! hexagonal rule (`GitPort::working_tree` shows the pattern). No network calls
//! reach out here: registry "latest" versions and CVEs are supplied by callers as
//! snapshots, keeping this deterministic across platforms and letting private /
//! credential-less packages be skipped without leaking secrets.

use std::collections::BTreeMap;

use coxagent_domain::{Complexity, Priority, Role, Ticket as DomainTicket, TicketId, TicketType};

use crate::state::ProjectState;

/// A parsed MAJOR.MINOR.PATCH triple straight off a lockfile line.
#[derive(Debug, Clone, Copy)]
pub struct Ver {
    /// Leading compatibility line.
    pub major: u64,
    /// Feature line.
    pub minor: u64,
    /// Bug-fix line.
    pub patch: u64,
}

impl Ver {
    /// Parse from a possibly-decorated semver string. Build metadata / pre-release
    /// suffixes are dropped so comparison works on decorated pins; unknown forms
    /// return `None` rather than aborting a scan.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let core = raw.split(['+', '-']).next().unwrap_or(raw);
        let mut it = core.split('.');
        let major = it.next()?.parse().ok()?;
        let minor = it.next().unwrap_or("0").parse().ok()?;
        let patch = it.next().unwrap_or("0").parse().ok()?;
        Some(Self {
            major,
            minor,
            patch,
        })
    }
}

impl std::fmt::Display for Ver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Highest-severity bump separating an older pinned version from its latest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BumpTier {
    Patch,
    Minor,
    Major,
}

impl BumpTier {
    /// Stable wire label used in ticket titles/bodies.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            BumpTier::Patch => "patch",
            BumpTier::Minor => "minor",
            BumpTier::Major => "major",
        }
    }

    /// The review action attached to each tier: patch auto-approves (safe),
    /// minor warns but may proceed automatically with awareness surfaced through
    /// team context on the ticket itself; major holds for human review because a
    /// breaking change can need API-compatibility care no agent should take alone.
    #[must_use]
    pub fn action(self) -> &'static str {
        match self {
            BumpTier::Patch => "auto-approve",
            BumpTier::Minor => "warn",
            BumpTier::Major => "hold",
        }
    }
}

/// How far behind `older` is from `latest`, if at all — highest class wins:
/// leading-line lag is Major even when later segments also lag; middle-line lag
/// alone is Minor; last-segment lag alone is Patch. Returns `None` when `older`
/// already satisfies or exceeds `latest`.
#[must_use]
fn gap(older: Ver, latest: Ver) -> Option<BumpTier> {
    if latest.major > older.major {
        Some(BumpTier::Major)
    } else if latest.minor > older.minor {
        Some(BumpTier::Minor)
    } else if latest.patch > older.patch {
        Some(BumpTier::Patch)
    } else {
        None
    }
}

/// Whether a CVE severity warrants an unconditional bug ticket even when no upgrade gap exists yet.
#[must_use]
pub fn is_urgent_cve(severity: &str) -> bool {
    let norm = severity.trim().to_ascii_lowercase();
    matches!(norm.as_str(), "high" | "critical")
}

/// Filenames recognised as lock files (the scanner's discovery contract).
pub const LOCK_FILENAMES: [&str; 3] = ["Cargo.lock", "package-lock.json", "poetry.lock"];

/// Whether `filename` is a lock file the scanner recognises.
#[must_use]
pub fn is_lockfile(filename: &str) -> bool {
    LOCK_FILENAMES.contains(&filename)
}

fn toml_string_value(rhs: &str) -> String {
    rhs.split('=')
        .nth(1)
        .unwrap_or("")
        .trim()
        .trim_matches('"')
        .to_owned()
}

/// Parse a TOML-family lock body (`Cargo.lock`, `poetry.lock`) into its pinned
/// dependency map. Repeated blocks are opened by a line containing `[[package]]`;
/// each carries bare-assignment `name = "…"` and `version = "…"`.
fn parse_toml_lock(body: &str) -> BTreeMap<String, Ver> {
    let mut out = BTreeMap::new();
    let mut cur_name: Option<String> = None;
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with("[[package]]") {
            cur_name = None;
        } else if let Some(rest) = line.strip_prefix("name") {
            cur_name = Some(toml_string_value(rest));
        } else if let Some(rest) = line.strip_prefix("version") {
            if let Some(name) = &cur_name {
                if let Some(v) = Ver::parse(&toml_string_value(rest)) {
                    out.insert(name.clone(), v);
                }
            }
        }
    }
    out
}

/// Parse an npm-style JSON lock (`package-lock.json`) via its top-level
/// `packages` object: keys are install paths like `node_modules/<name>`, values
/// carry a version string.
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
        // Recover a readable package name from its install path; keep nested path
        // suffixes and scopes together so one dependency maps to one row.
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

/// Which family of parser a filename dispatches to — dispatch follows the file,
/// never the content, because Cargo vs npm formats can share bytes.
fn parser_for(filename: &str) -> fn(&str) -> BTreeMap<String, Ver> {
    match filename {
        "Cargo.lock" | "poetry.lock" => parse_toml_lock,
        _ => parse_package_lock,
    }
}

/// Parse one lockfile body into its dependency -> current version map,
/// dispatching on filename rather than content.
#[must_use]
pub fn parse_lockfile(filename: &str, body: &str) -> BTreeMap<String, Ver> {
    parser_for(filename)(body)
}

/// One actionable finding for a single dependency across every lockfile it appears in —
/// current version, what the registry reports as newest (if known), how far behind that leaves
/// us (the bump tier), and whether an urgent CVE overrides all of it.
#[derive(Debug)]
pub struct ScanFinding {
    pub package: String,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub tier: Option<BumpTier>,
    /// Set when this package carries a high/critical CVE — forces an immediate high-priority bug regardless of version age.
    pub urgent_cve_severity: Option<String>,
    /// Lockfiles (as display paths) pinning this dependency at its current version.
    pub affected_files: Vec<String>,
}

/// Aggregate pinned versions and affected files across every supplied lockfile,
/// keyed by dependency name. `lockfiles` are `(display_path, body)` pairs; they
/// are sorted internally so output is deterministic regardless of caller order.
fn aggregate_locks(lockfiles: &[(String, String)]) -> BTreeMap<String, (Ver, Vec<String>)> {
    let mut by_name: BTreeMap<String, (Option<Ver>, Vec<String>)> = BTreeMap::new();
    let mut ordered: Vec<&(String, String)> = lockfiles.iter().collect();
    ordered.sort_by(|a, b| a.0.cmp(&b.0));
    for (path_display, body) in ordered {
        let parsed = parse_lockfile(path_display.as_str(), body);
        for (name, ver) in parsed {
            let entry = by_name
                .entry(name.clone())
                .or_insert_with(|| (None, Vec::new()));
            if entry.0.is_none() {
                entry.0 = Some(ver);
            }
            if !entry.1.iter().any(|f| f == path_display) {
                entry.1.push(path_display.clone());
            }
        }
    }
    by_name
        .into_iter()
        .filter_map(|(name, (ver_opt, files))| ver_opt.map(|v| (name, (v, files))))
        .collect()
}

/// Cross-reference every parsed lockfile against a registry snapshot and CVE data
/// to produce one [`ScanFinding`] per package that needs attention.
///
/// * `lockfiles` — `(display_path, body)` pairs for every recognised lockfile found in the tree.
/// * `registry_latest_by_package` — package name -> newest release as reported by the registry.
///   A package with no entry (unknown or private-registry with no credential available) is
///   skipped: without a known latest version there is nothing to upgrade toward.
/// * `cve_severity_by_package` — package name -> highest known CVE severity (e.g. "high").
///
/// A finding is emitted when the package has an upgrade gap at any tier, OR it carries an
/// urgent CVE (which fires regardless of version age). Packages fully up to date with no
/// urgent CVE produce no finding.
#[must_use]
pub fn scan_locks(
    lockfiles: &[(String, String)],
    registry_latest_by_package: &BTreeMap<String, String>,
    cve_severity_by_package: &BTreeMap<String, String>,
) -> Vec<ScanFinding> {
    let mut findings = Vec::new();
    for (name, (pinned, files)) in aggregate_locks(lockfiles) {
        let current_string = pinned.to_string();
        let latest_ver = registry_latest_by_package
            .get(&name)
            .and_then(|s| Ver::parse(s));
        let tier = latest_ver.and_then(|latest| gap(pinned, latest));
        let urgent_cve = cve_severity_by_package
            .get(&name)
            .filter(|sev| is_urgent_cve(sev))
            .cloned();
        if tier.is_none() && urgent_cve.is_none() {
            continue;
        }
        findings.push(ScanFinding {
            package: name,
            current_version: current_string,
            latest_version: latest_ver.map(|v| v.to_string()),
            tier,
            urgent_cve_severity: urgent_cve,
            affected_files: files,
        });
    }
    findings.sort_by(|a, b| a.package.cmp(&b.package));
    findings
}

/// The single master 'Dependency Audit' epic every remediation ticket links to
/// via `depends_on`, so all supply-chain work lands under one umbrella.
pub const MASTER_EPIC_ID: &str = "DEP-AUDIT-001";

/// A unique, deterministic ticket id for a remediation — one per dependency so
/// re-running a scan never duplicates an already-filed proposal.
///
/// The mapping must be INJECTIVE over every possible package name: two distinct
/// dependencies can never share an id regardless of which ones happen to appear
/// together in one pass or any earlier pass has filed ([CXA-B089]). A lossy rule
/// that replaces every non-alphanumeric char with '-' collapses unrelated scoped /
/// nested / dotted names onto the same id (`@scope/pkg`, dotted and hyphenated
/// spellings all become indistinguishable), so whichever finding sorts first wins,
/// every colliding sibling is silently skipped as-if-duplicate even though it was
/// flagged, and none of its evidence is ever recorded under its own key.
///
/// To stay collision-free while keeping ordinary crate / npm / poetry identifiers —
/// which are overwhelmingly ASCII alphanumerics — readable on sight, letters and
/// digits are emitted verbatim and every other byte becomes an unambiguous,
/// fixed-width token that cannot be produced by joining other outputs together.
fn ticket_id_for(dep: &str) -> String {
    let mut out = String::from("DEP");
    for b in dep.as_bytes() {
        if b.is_ascii_alphanumeric() {
            out.push(*b as char);
        } else {
            out.push('<');
            push_hex_byte(&mut out, *b);
            out.push('>');
        }
    }
    out
}

/// Append one byte as two fixed-width lowercase hex digits.
fn push_hex_byte(out: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let nibbles = [byte >> 4, byte & 0x0f];
    for nibble in nibbles {
        out.push(HEX[nibble as usize] as char);
    }
}

/// Create the master epic once, so every remediation links to the same umbrella.
fn ensure_master_epic(state: &mut ProjectState) {
    if state
        .tickets
        .iter()
        .any(|t| t.id().as_str() == MASTER_EPIC_ID)
    {
        return;
    }
    let Ok(id) = TicketId::new(MASTER_EPIC_ID.to_string()) else {
        return;
    };
    let Ok(epic) = DomainTicket::new(
        id,
        TicketType::Chore,
        "Dependency Audit master epic".to_string(),
        "Umbrella for all dependency-health remediation work.".to_string(),
        Priority::Medium,
        Complexity::Large,
        false,
    ) else {
        return;
    };
    state.tickets.push(epic);
}

fn link_to_epic(ticket: &mut DomainTicket) {
    if let Ok(id) = TicketId::new(MASTER_EPIC_ID.to_string()) {
        let _ = ticket.add_dependency(Role::Sa, id);
    }
}

/// File one finding as its proper ticket and attach the scan result as team context.
///
/// An urgent CVE becomes a high-priority Bug regardless of version age; any other flagged package
/// becomes a low-priority Chore carrying current/latest versions and every affected lockfile.
/// Both are pre-linked to the master epic via `depends_on`.
fn propose_one(state: &mut ProjectState, finding: &ScanFinding) -> Option<TicketId> {
    let Ok(id) = TicketId::new(ticket_id_for(&finding.package)) else {
        return None;
    };
    if state.tickets.iter().any(|t| t.id() == &id) {
        return None;
    }
    let files_block = finding.affected_files.join("\n");
    let tier_label = finding.tier.map_or("unknown", |t| t.label());
    let tier_action = finding.tier.map_or("review", |t| t.action());
    let latest = finding.latest_version.as_deref().unwrap_or("unknown");

    if let Some(sev) = &finding.urgent_cve_severity {
        let description = format!(
			"Dependency {}@{} has a {}/critical CVE. Upgrade or vendor-patch required. Affected lockfiles:\n{}",
			finding.package,
			finding.current_version,
			sev,
			files_block
		);
        let Ok(mut bug) = DomainTicket::new(
            id.clone(),
            TicketType::Bug,
            format!("CVE in {}", finding.package),
            description,
            Priority::High,
            Complexity::Small,
            false,
        ) else {
            return None;
        };
        link_to_epic(&mut bug);
        let created_id = bug.id().clone();
        state.tickets.push(bug);
        state.add_evidence(
            created_id.to_string().as_str(),
            "dependency-scan",
            "cve finding",
            &format!(
                "{}@{} severity={}",
                finding.package, finding.current_version, sev
            ),
        );
        return Some(created_id);
    }

    let description = format!(
        "Dependency {}: current {} -> latest {} ({}, {}). Affected lockfiles:\n{}",
        finding.package,
        finding.current_version,
        latest,
        tier_label,
        tier_action,
        files_block.trim()
    );
    let Ok(mut chore) = DomainTicket::new(
        id.clone(),
        TicketType::Chore,
        format!("upgrade {} ({})", finding.package, tier_label),
        description,
        Priority::Medium,
        Complexity::Small,
        false,
    ) else {
        return None;
    };
    link_to_epic(&mut chore);
    let created_id = chore.id().clone();
    state.tickets.push(chore);
    state.add_evidence(
        created_id.to_string().as_str(),
        "dependency-scan",
        "scan result",
        &format!(
            "{}@{}->{} tagged {}",
            finding.package, finding.current_version, latest, tier_label
        ),
    );
    Some(created_id)
}

/// Run one full remediation pass over findings: ensure the master epic exists once,
/// then file each unique proposal under it (idempotent — duplicates are skipped).
#[must_use]
pub fn apply_findings(state: &mut ProjectState, findings: &[ScanFinding]) -> Vec<TicketId> {
    ensure_master_epic(state);
    findings
        .iter()
        .filter_map(|f| propose_one(state, f))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(raw: &str) -> Ver {
        Ver::parse(raw).expect("valid version")
    }

    const CARGO_SAMPLE: &str = "\
[[package]]
name = \"alpha\"
version = \"1.2.3\"

[[package]]
name = \"beta\"
version = \"0.9.0\"
";

    const NPM_SAMPLE: &str = r#"{
  "packages": {
    "node_modules/lodash": { "version": "4.17.21" },
    "node_modules/@scope/pkg": { "version": "7.8.9" },
    "": { "name": "app", "version": "1.0.0" }
  }
}"#;

    #[test]
    fn parses_toml_family_locks_by_name() {
        let cargo = parse_lockfile("Cargo.lock", CARGO_SAMPLE);
        assert_eq!(cargo["alpha"].patch, 3);
        let poetry = parse_lockfile("poetry.lock", CARGO_SAMPLE);
        assert_eq!(poetry["beta"].minor, 9);
    }

    #[test]
    fn parses_npm_locks_and_scoped_names() {
        let npm = parse_package_lock(NPM_SAMPLE);
        assert_eq!(npm["lodash"].patch, 21);
        assert_eq!(npm["@scope/pkg"].major, 7);
    }

    #[test]
    fn gap_classifies_patch_minor_major_and_up_to_date() {
        assert_eq!(gap(v("1.2.3"), v("1.2.9")), Some(BumpTier::Patch));
        assert_eq!(gap(v("1.2"), v("1.3")), Some(BumpTier::Minor));
        assert_eq!(gap(v("1"), v("2")), Some(BumpTier::Major));
        assert_eq!(gap(v("4"), v("4")), None);
    }

    #[test]
    fn urgent_cve_detection_case_insensitive() {
        assert!(is_urgent_cve("high"));
        assert!(is_urgent_cve("Critical"));
        assert!(!is_urgent_cve("low"));
        assert!(!is_urgent_cve("moderate"));
    }

    #[test]
    fn scan_flags_behind_packages_and_skips_unknown_registry_packages() {
        let locks = vec![("Cargo.lock".to_string(), CARGO_SAMPLE.to_string())];
        let mut registry = BTreeMap::new();
        registry.insert("alpha".to_string(), "2.0.0".to_string());
        let cves = BTreeMap::new();
        let findings = scan_locks(&locks, &registry, &cves);
        // alpha lags 1.x -> 2.x (major); beta has no registry entry so it is skipped.
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].package, "alpha");
        assert_eq!(findings[0].tier, Some(BumpTier::Major));
        assert_eq!(findings[0].affected_files, vec!["Cargo.lock".to_string()]);
    }

    #[test]
    fn urgent_cve_yields_a_bug_even_when_version_is_current() {
        let locks = vec![("Cargo.lock".to_string(), CARGO_SAMPLE.to_string())];
        let mut registry = BTreeMap::new();
        registry.insert("alpha".to_string(), "1.2.3".to_string());
        let mut cves = BTreeMap::new();
        cves.insert("alpha".to_string(), "high".to_string());
        let findings = scan_locks(&locks, &registry, &cves);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].tier, None);
        assert_eq!(findings[0].urgent_cve_severity.as_deref(), Some("high"));

        let mut state = ProjectState::default();
        let ids = apply_findings(&mut state, &findings);
        assert_eq!(ids.len(), 1);
        let t = state.ticket(&ids[0]).expect("filed bug");
        assert_eq!(t.ticket_type(), TicketType::Bug);
        assert_eq!(t.priority(), Priority::High);
    }

    #[test]
    fn proposal_is_idempotent() {
        let locks = vec![("Cargo.lock".to_string(), CARGO_SAMPLE.to_string())];
        let mut registry = BTreeMap::new();
        registry.insert("alpha".to_string(), "2.0.0".to_string());
        let findings = scan_locks(&locks, &registry, &BTreeMap::new());

        let mut state = ProjectState::default();
        assert_eq!(apply_findings(&mut state, &findings).len(), 1);
        // A second pass must not duplicate an already-filed proposal.
        assert!(apply_findings(&mut state, &findings).is_empty());
    }

    #[test]
    fn distinct_deps_whose_lossy_sanitized_ids_collide_each_file_their_own_ticket() {
        // Regression for CXA-B089. Under the old lossy sanitizer, '.' and '-'
        // both became '-', so 'a.b' and 'a-b' collapsed onto one id and whichever
        // finding sorted first won it — the other dependency's remediation (and
        // its evidence) was silently dropped every pass even though it was flagged.
        let locks = vec![(
            "package-lock.json".to_string(),
            r#"{
  "packages": {
    "node_modules/a.b": { "version": "1.0.0" },
    "node_modules/a-b": { "version": "1.0.0" }
  }
}"#
            .to_string(),
        )];
        let mut registry = BTreeMap::new();
        registry.insert("a.b".to_string(), "2.0.0".to_string());
        registry.insert("a-b".to_string(), "3.0.0".to_string());
        let findings = scan_locks(&locks, &registry, &BTreeMap::new());
        assert_eq!(findings.len(), 2, "both flagged deps must surface");

        let mut state = ProjectState::default();
        let ids = apply_findings(&mut state, &findings);

        assert_eq!(
            ids.len(),
            2,
            "each colliding dependency must get its own ticket"
        );
        // Distinct dependencies must never share a ticket id...
        assert_ne!(ids[0], ids[1]);
        // ...and each id must map back to exactly its own package (never cross-wired).
        for id in &ids {
            let t = state.ticket(id).expect("filed ticket");
            let title = t.title();
            assert!(
                title.contains("a.b") || title.contains("a-b"),
                "unexpected title {title:?}"
            );
            if title.contains("a.b") {
                assert!(!title.contains("a-b"), "{title:?} cross-wired two deps");
            } else {
                assert!(!title.contains("a.b"), "{title:?} cross-wired two deps");
            }
            assert!(
                state.ticket_evidence.contains_key(id.as_str()),
                "evidence recorded under its own key for {id}"
            );
        }
    }

    #[test]
    fn scoped_names_get_distinct_ids_from_flat_spellings() {
        // '@scope/pkg' and a plain dotted/hyphenated spelling once shared '-' runs.
        let pkg_a = ticket_id_for("@scope/pkg");
        let pkg_b = ticket_id_for("-scope-pkg");
        assert_ne!(pkg_a, pkg_b);
    }
}
