//! `ScanDependenciesUseCase` — the production caller for the dependency-health
//! scanner (CXA-F009 / CXA-B111). Discovers the workspace's lockfiles through
//! the [`DependencyDiscoveryPort`], cross-references them against a registry
//! snapshot plus CVE data, and files remediation tickets into project state via
//! [`crate::deps_scan::apply_findings`].
//!
//! The registry/CVE maps are caller-supplied snapshots by design: the pure core
//! performs no network IO, so whoever triggers the scan (an operator, a CI job,
//! the dashboard) hands in what the registry currently reports. That keeps the
//! whole pass deterministic and lets private, credential-less packages be
//! skipped without leaking secrets. An empty snapshot still exercises the full
//! discovery + parse path — it simply flags nothing.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;

use crate::deps_scan::{self, BumpTier, ScanFinding};
use crate::error::PortError;
use crate::ports::outbound::{mutate_state, DependencyDiscoveryPort, StateStorePort};

/// What the caller supplies for one scan pass.
#[derive(Debug, Default, Clone)]
pub struct ScanDepsInput {
    /// package name -> newest release as reported by the registry. Packages
    /// with no entry are skipped: without a known latest there is nothing to
    /// upgrade toward.
    pub registry_latest: BTreeMap<String, String>,
    /// package name -> highest known CVE severity (e.g. `"high"`). Only
    /// high/critical severities force the immediate bug ticket.
    pub cve_severity: BTreeMap<String, String>,
}

/// One flagged package as the scan reports it over the wire.
#[derive(Debug, Serialize)]
pub struct FindingReport {
    pub package: String,
    pub current_version: String,
    pub latest_version: Option<String>,
    /// Highest-severity bump separating us from latest (`patch`/`minor`/`major`).
    pub tier: Option<String>,
    /// The review action the tier maps to (`auto-approve`/`warn`/`hold`).
    pub action: Option<String>,
    /// Set when the package carries a high/critical CVE.
    pub urgent_cve_severity: Option<String>,
    /// Lockfiles pinning this dependency at its current version.
    pub affected_files: Vec<String>,
}

/// Summary of one full scan pass.
#[derive(Debug, Serialize)]
pub struct ScanDepsOutcome {
    /// Lockfiles the scan read, repo-relative, sorted.
    pub scanned_files: Vec<String>,
    /// Every package the scan flagged, filed or not.
    pub findings: Vec<FindingReport>,
    /// Ids of tickets filed THIS pass.
    pub filed: Vec<String>,
    /// Flagged packages that already had their ticket or a recorded
    /// remediation — the idempotency proof for repeat scans (CXA-B093/B099
    /// dedupe, surfaced instead of silently swallowed).
    pub suppressed: usize,
}

/// Runs one dependency-health pass over a project's workspace.
pub struct ScanDependenciesUseCase<S: StateStorePort + ?Sized, D: DependencyDiscoveryPort + ?Sized> {
    store: Arc<S>,
    discovery: Arc<D>,
    work_dir: PathBuf,
}

impl<S: StateStorePort + ?Sized, D: DependencyDiscoveryPort + ?Sized>
    ScanDependenciesUseCase<S, D>
{
    pub fn new(store: Arc<S>, discovery: Arc<D>, work_dir: PathBuf) -> Self {
        Self {
            store,
            discovery,
            work_dir,
        }
    }

    /// Discover, scan, file. The decision (which packages are flagged, which
    /// tickets survive dedupe) is the pure scanner's; this use case only moves
    /// data between the ports and persists the result atomically.
    ///
    /// # Errors
    /// - [`PortError::Backend`] when lockfiles cannot be discovered or read.
    /// - [`PortError::Conflict`] when concurrent writers keep beating the save.
    pub async fn execute(&self, input: ScanDepsInput) -> Result<ScanDepsOutcome, PortError> {
        let locks = self.discovery.discover_lockfiles(&self.work_dir).await?;
        let pairs: Vec<(String, String)> = locks
            .iter()
            .map(|l| (l.path.clone(), l.body.clone()))
            .collect();
        let scanned_files: Vec<String> = pairs.iter().map(|(p, _)| p.clone()).collect();
        let scanned_count = scanned_files.len();

        let findings = deps_scan::scan_locks(&pairs, &input.registry_latest, &input.cve_severity);
        let findings_len = findings.len();

        let mut filed: Vec<String> = Vec::new();
        mutate_state(self.store.as_ref(), |s| {
            // Re-runs on save conflicts start from freshly loaded state, and
            // apply_findings dedupes against it — so `filed` reflects exactly
            // the pass that actually persisted.
            filed = deps_scan::apply_findings(s, &findings)
                .into_iter()
                .map(|id| id.to_string())
                .collect();
            s.log_activity(
                "COX",
                &format!(
                    "dependency scan: {scanned_count} lockfile(s), {findings_len} finding(s), {} ticket(s) filed",
                    filed.len()
                ),
                None,
            );
            Ok(())
        })
        .await?;

        let suppressed = findings_len.saturating_sub(filed.len());
        Ok(ScanDepsOutcome {
            scanned_files,
            findings: findings.iter().map(finding_report).collect(),
            filed,
            suppressed,
        })
    }
}

/// Project a pure [`ScanFinding`] onto its wire shape.
fn finding_report(f: &ScanFinding) -> FindingReport {
    FindingReport {
        package: f.package.clone(),
        current_version: f.current_version.clone(),
        latest_version: f.latest_version.clone(),
        tier: f.tier.map(BumpTier::label).map(str::to_owned),
        action: f.tier.map(BumpTier::action).map(str::to_owned),
        urgent_cve_severity: f.urgent_cve_severity.clone(),
        affected_files: f.affected_files.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::outbound::Lockfile;
    use crate::state::ProjectState;
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// In-memory store — no filesystem in the use-case test.
    #[derive(Default)]
    struct MemStore {
        state: Mutex<ProjectState>,
    }

    #[async_trait]
    impl StateStorePort for MemStore {
        async fn load(&self) -> Result<ProjectState, PortError> {
            Ok(self.state.lock().expect("lock").clone())
        }
        async fn save(&self, s: &ProjectState) -> Result<(), PortError> {
            s.validate().map_err(PortError::Corrupt)?;
            *self.state.lock().expect("lock") = s.clone();
            Ok(())
        }
    }

    /// Discovery double serving fixed lockfiles — the adapter's job is IO, the
    /// use case's decisions are what these tests pin.
    struct FakeDiscovery {
        locks: Vec<Lockfile>,
        fails: bool,
    }

    #[async_trait]
    impl DependencyDiscoveryPort for FakeDiscovery {
        async fn discover_lockfiles(
            &self,
            _root: &std::path::Path,
        ) -> Result<Vec<Lockfile>, PortError> {
            if self.fails {
                return Err(PortError::Backend("unreadable lockfile".to_owned()));
            }
            Ok(self.locks.clone())
        }
    }

    fn cargo_lock(packages: &[(&str, &str)]) -> String {
        let mut out = String::new();
        for (n, v) in packages {
            out.push_str("[[package]]\nname = \"");
            out.push_str(n);
            out.push_str("\"\nversion = \"");
            out.push_str(v);
            out.push_str("\"\n");
        }
        out
    }

    fn discovery_with(path: &str, body: String) -> FakeDiscovery {
        FakeDiscovery {
            locks: vec![Lockfile {
                path: path.to_owned(),
                body,
            }],
            fails: false,
        }
    }

    fn registry(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[tokio::test]
    async fn files_a_ticket_for_a_flagged_dependency_and_persists_it() {
        let store = Arc::new(MemStore::default());
        let uc = ScanDependenciesUseCase::new(
            Arc::clone(&store) as Arc<dyn StateStorePort>,
            Arc::new(discovery_with("Cargo.lock", cargo_lock(&[("alpha", "1.2.3")]))),
            PathBuf::from("/work"),
        );

        let out = uc
            .execute(ScanDepsInput {
                registry_latest: registry(&[("alpha", "2.0.0")]),
                cve_severity: BTreeMap::new(),
            })
            .await
            .expect("scan");

        assert_eq!(out.scanned_files, vec!["Cargo.lock".to_owned()]);
        assert_eq!(out.filed.len(), 1);
        assert_eq!(out.suppressed, 0);
        assert_eq!(out.findings.len(), 1);
        assert_eq!(out.findings[0].tier.as_deref(), Some("major"));
        assert_eq!(out.findings[0].action.as_deref(), Some("hold"));

        // The filed ticket really landed in persisted state, epic-linked.
        let s = store.load().await.expect("load");
        let id = coxagent_domain::TicketId::new(out.filed[0].clone()).expect("id");
        let t = s.ticket(&id).expect("filed ticket");
        assert_eq!(t.ticket_type(), coxagent_domain::TicketType::Chore);
        assert_eq!(t.priority(), coxagent_domain::Priority::Medium);
        assert!(
            t.description().contains("Cargo.lock"),
            "affected lockfile carried on the ticket"
        );
        // CXA-B116: a filing pass brings its umbrella with it — the epic the
        // remediation links to must be a real ticket, not a dangling id.
        assert!(
            s.tickets
                .iter()
                .any(|t| t.id().as_str() == crate::deps_scan::MASTER_EPIC_ID),
            "filing pass must create the master epic in state"
        );
    }

    #[tokio::test]
    async fn repeat_scan_suppresses_instead_of_duplicating() {
        let store = Arc::new(MemStore::default());
        let uc = ScanDependenciesUseCase::new(
            Arc::clone(&store) as Arc<dyn StateStorePort>,
            Arc::new(discovery_with("Cargo.lock", cargo_lock(&[("alpha", "1.2.3")]))),
            PathBuf::from("/work"),
        );
        let input = ScanDepsInput {
            registry_latest: registry(&[("alpha", "2.0.0")]),
            cve_severity: BTreeMap::new(),
        };

        let first = uc.execute(input.clone()).await.expect("first pass");
        assert_eq!(first.filed.len(), 1);

        let second = uc.execute(input).await.expect("second pass");
        assert!(
            second.filed.is_empty(),
            "re-scan must not duplicate: {:?}",
            second.filed
        );
        assert_eq!(second.suppressed, 1);
        assert_eq!(second.findings.len(), 1, "still reported, just suppressed");
    }

    #[tokio::test]
    async fn an_empty_registry_still_reports_what_was_scanned() {
        let discovery = FakeDiscovery {
            locks: vec![
                Lockfile {
                    path: "Cargo.lock".to_owned(),
                    body: cargo_lock(&[("alpha", "1.2.3")]),
                },
                Lockfile {
                    path: "e2e/package-lock.json".to_owned(),
                    body: r#"{"packages":{"node_modules/lodash":{"version":"4.17.21"}}}"#
                        .to_owned(),
                },
            ],
            fails: false,
        };
        let store = Arc::new(MemStore::default());
        let uc = ScanDependenciesUseCase::new(
            Arc::clone(&store) as Arc<dyn StateStorePort>,
            Arc::new(discovery),
            PathBuf::from("/work"),
        );

        let out = uc.execute(ScanDepsInput::default()).await.expect("scan");

        assert_eq!(
            out.scanned_files,
            vec![
                "Cargo.lock".to_owned(),
                "e2e/package-lock.json".to_owned()
            ]
        );
        assert!(out.findings.is_empty(), "nothing flagged without a registry");
        assert!(out.filed.is_empty());

        // CXA-B116: `filed:[]` must mean nothing was written — the zero-finding
        // pass used to sneak the DEP-AUDIT-001 master epic into persisted state.
        let s = store.load().await.expect("load");
        assert!(
            !s.tickets
                .iter()
                .any(|t| t.id().as_str() == crate::deps_scan::MASTER_EPIC_ID),
            "empty scan must not file the master epic"
        );
    }

    #[tokio::test]
    async fn an_urgent_cve_files_a_high_priority_bug() {
        let store = Arc::new(MemStore::default());
        let uc = ScanDependenciesUseCase::new(
            Arc::clone(&store) as Arc<dyn StateStorePort>,
            Arc::new(discovery_with("Cargo.lock", cargo_lock(&[("alpha", "1.2.3")]))),
            PathBuf::from("/work"),
        );
        let mut cves = BTreeMap::new();
        cves.insert("alpha".to_owned(), "critical".to_owned());

        let out = uc
            .execute(ScanDepsInput {
                // Current version — no upgrade gap; only the CVE flags it.
                registry_latest: registry(&[("alpha", "1.2.3")]),
                cve_severity: cves,
            })
            .await
            .expect("scan");

        assert_eq!(out.filed.len(), 1);
        assert_eq!(
            out.findings[0].urgent_cve_severity.as_deref(),
            Some("critical")
        );
        let s = store.load().await.expect("load");
        let id = coxagent_domain::TicketId::new(out.filed[0].clone()).expect("id");
        let t = s.ticket(&id).expect("filed bug");
        assert_eq!(t.ticket_type(), coxagent_domain::TicketType::Bug);
        assert_eq!(t.priority(), coxagent_domain::Priority::High);
    }

    #[tokio::test]
    async fn a_discovery_failure_surfaces_as_an_error() {
        let store = Arc::new(MemStore::default());
        let uc = ScanDependenciesUseCase::new(
            Arc::clone(&store) as Arc<dyn StateStorePort>,
            Arc::new(FakeDiscovery {
                locks: Vec::new(),
                fails: true,
            }),
            PathBuf::from("/work"),
        );

        let err = uc
            .execute(ScanDepsInput::default())
            .await
            .expect_err("discovery failure propagates");

        assert!(err.to_string().contains("unreadable lockfile"), "{err}");
        // Nothing was persisted on the failed pass.
        assert!(store.load().await.expect("load").tickets.is_empty());
    }
}
