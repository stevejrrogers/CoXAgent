// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
//! Dependency-health scan surface (CXA-B111): the HTTP door to the shipped
//! scanner. Until this route existed, `deps_scan` (and the CXA-B099 dedupe
//! fix inside it) had no production caller — no use case, endpoint or job
//! ever fed it, so nothing in the deployed app could exercise it.
//!
//! The request body carries the registry/CVE snapshots the pure core was
//! designed for: the scanner performs no network IO, so the caller supplies
//! what the registry currently reports. An empty body is still a full pass —
//! it discovers and parses every lockfile and reports the inventory, flagging
//! nothing.

use super::*;
use std::collections::BTreeMap;

/// Optional body of `POST /api/projects/:pid/deps/scan`. Both maps default to
/// empty, so a body-less POST stays a valid inventory-only scan. A malformed
/// body degrades the same way (`Option<Json<_>>` swallows the rejection, the
/// house pattern here) — never silently: the response always reports exactly
/// what was scanned and filed, so a typo'd snapshot shows up as empty findings.
#[derive(serde::Deserialize, Default)]
pub(super) struct ScanDepsReq {
    /// package -> newest version per the registry snapshot.
    #[serde(default)]
    pub(super) registry: BTreeMap<String, String>,
    /// package -> highest known CVE severity (e.g. `"high"`).
    #[serde(default)]
    pub(super) cves: BTreeMap<String, String>,
}

/// POST `/api/projects/:pid/deps/scan` — run one dependency-health pass over
/// the project's workspace: discover lockfiles, cross-reference the supplied
/// registry/CVE snapshots, and file remediation tickets (idempotent — the
/// scanner's dedupe suppresses already-remediated packages, reported as
/// `suppressed` rather than silently dropped).
pub(super) async fn scan_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    body: Option<Json<ScanDepsReq>>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(discovery) = p.deps_discovery.clone() else {
        return internal_error("no dependency discovery adapter configured");
    };
    let req = body.map(|Json(r)| r).unwrap_or_default();
    let uc = coxagent_application::use_cases::ScanDependenciesUseCase::new(
        Arc::clone(&p.store),
        discovery,
        p.work_dir.clone(),
    );
    match uc
        .execute(coxagent_application::use_cases::ScanDepsInput {
            registry_latest: req.registry,
            cve_severity: req.cves,
        })
        .await
    {
        Ok(outcome) => Json(outcome).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}
