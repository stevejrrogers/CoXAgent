//! CXA-B186a — the reusable guardrail test scaffold, self-tested.
//!
//! CXA-B181.1/.2 and CXA-B191 ship guardrails whose whole job is to fail
//! when someone regresses a shipped invariant (a KPI tile loses its
//! zero-state hint, the 14-day window drops off a tile, the tile count
//! silently changes). This file holds ONLY the machinery those gates
//! share, so each gate stays a pure decision over the data an adapter
//! hands it:
//!
//! - ports ([`ReadRepoText`], [`ListRepoFiles`]) — the only way a check
//!   touches the filesystem, so tests can drive every gate from a
//!   hand-built [`Ctx`] with no repo on disk;
//! - the allowlist-shrink rule ([`shrink_check`]) — a grandfathered file
//!   may leave the list, never enter it, so migrations can only move
//!   forward;
//! - one exemplar gate (the overview KPI panel's depth contract, the
//!   invariants tabled in `docs/CXA-B204-overview-kpi-panel-inventory.md`)
//!   with its own pure self-tests proving both directions, plus a live
//!   check that the checked-in `kpis.js` obeys it.
//!
//! CI enforcement lives in `.github/workflows/ci.yml` (`guard-tests`,
//! step `cargo test -p coxagent-presentation --test guardrail_scaffold_b201`)
//! and is itself guarded by `crates/app/tests/ci_availability_gate.rs`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Ports — the only IO a guardrail check may perform
// ---------------------------------------------------------------------------

/// Outbound port: read a UTF-8 file relative to the repo root.
trait ReadRepoText {
    fn read_utf8(&self, rel: &str) -> Option<String>;
}

/// Outbound port: list the files a guardrail scans (repo-relative).
trait ListRepoFiles {
    fn list_scanned(&self, sub: &str) -> Vec<String>;
}

/// Everything a check may see. A guardrail is a pure function
/// `(&Ctx, &Invariant) -> Result<(), String>` — the adapter fills the ctx,
/// the decision stays testable without a filesystem.
#[allow(dead_code)]
struct Ctx {
    files: Vec<String>,
    reader: Box<dyn ReadRepoText>,
}

impl Ctx {
    /// An empty context — the nothing-to-protect case every check must
    /// treat as a failure, not a silent pass.
    #[allow(dead_code)]
    fn empty() -> Self {
        Self {
            files: Vec::new(),
            reader: Box::new(NullRepo),
        }
    }

}

/// The never-readable repo — what an empty ctx hands back, so `read_utf8`
/// on a missing file and "no file at all" are indistinguishable to a check.
struct NullRepo;

impl ReadRepoText for NullRepo {
    fn read_utf8(&self, _rel: &str) -> Option<String> {
        None
    }
}

// ---------------------------------------------------------------------------
// The allowlist-shrink rule (CXA-B186a's core deliverable)
// ---------------------------------------------------------------------------

/// One named guardrail: what it protects, the files still allowed to
/// violate it, and the pure decision.
struct Invariant {
    /// Stable name — what CI prints and the invariant registry links to.
    name: &'static str,
    /// One line saying why the invariant exists (shown on failure).
    #[allow(dead_code)]
    why: &'static str,
    /// Files grandfathered as allowed to violate it. May only shrink.
    allowlist: &'static [&'static str],
    /// The pure decision over one file's contents: `None` = obeys.
    check: fn(&str) -> Option<String>,
}

impl Invariant {
    fn scan(&self, files: &[String], read: &dyn ReadRepoText) -> Result<(), String> {
        let mut offenders = Vec::new();
        for file in files {
            if self.allowlist.contains(&file.as_str()) {
                continue;
            }
            let Some(text) = read.read_utf8(file) else {
                return Err(format!(
                    "[{}] `{file}` is in the scanned tree but not readable — \
                     refusing to guess (fail closed)",
                    self.name
                ));
            };
            if let Some(why) = (self.check)(&text) {
                offenders.push(format!("  {file}: {why}"));
            }
        }
        if offenders.is_empty() {
            Ok(())
        } else {
            Err(format!("[{}] violated:\n{}", self.name, offenders.join("\n")))
        }
    }
}

/// The shrink rule: an allowlist may lose entries, never gain them.
/// A migration removes its entry and fixes the file — the only legal
/// transition. A grow re-opens a hole someone closed, so it fails.
fn shrink_check(before: &[&str], after: &[&str]) -> Result<(), String> {
    let was: BTreeSet<&str> = before.iter().copied().collect();
    let grew: Vec<&str> = after.iter().copied().filter(|f| !was.contains(f)).collect();
    if grew.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "allowlist grew — re-granted exemption(s) {} must not come back; \
             fix the file instead",
            grew.iter().map(|f| format!("`{f}`")).collect::<Vec<_>>().join(", ")
        ))
    }
}

// ---------------------------------------------------------------------------
// Exemplar gate: the overview KPI panel's depth contract
// ---------------------------------------------------------------------------

/// The file whose rendered output is the pilot panel this scaffold guards.
const KPI_PANEL_FILE: &str = "crates/presentation/src/web/js/kpis.js";

/// A tile literal must carry each of these to honour the depth contract:
/// a zero-state hint (what fills the tile when it reads 0), a per-day
/// series (the 14-day sparkline window) and a click-through target.
const TILE_REQUIRED_KEYS: [&str; 3] = ["hint:", "series:", "go:"];

/// Split a `overviewKpiTile({ ... })` literal body out of the source.
/// The tiles are the object literals passed to the renderer; naive brace
/// matching is enough because the file keeps each literal on few lines
/// and never nests an object inside a tile.
fn tile_bodies(js: &str) -> Vec<String> {
    let mut bodies = Vec::new();
    let mut rest = js;
    while let Some(pos) = rest.find("overviewKpiTile({") {
        let after = &rest[pos + "overviewKpiTile({".len()..];
        let mut depth = 1usize;
        let mut end = None;
        for (i, c) in after.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else { break };
        bodies.push(after[..end].to_owned());
        rest = &after[end..];
    }
    bodies
}

/// The pure decision behind the exemplar: does `js` render the five
/// overview tiles, each carrying the full depth contract? `None` = obeys.
fn why_kpi_panel_shallow(js: &str) -> Option<String> {
    let bodies = tile_bodies(js);
    if bodies.len() != 5 {
        return Some(format!(
            "renders {} tile(s), not the 5 the Overview panel ships — a tile \
             was added or dropped without updating the guard",
            bodies.len()
        ));
    }
    for (i, body) in bodies.iter().enumerate() {
        for key in TILE_REQUIRED_KEYS {
            if !body.contains(key) {
                return Some(format!(
                    "tile #{i} lost `{key}` — depth regressed (label: {})",
                    body.split(',').next().unwrap_or("?").trim()
                ));
            }
        }
    }
    None
}

fn kpi_depth_invariant() -> Invariant {
    Invariant {
        name: "overview/kpi-tiles-carry-depth",
        why: "a KPI number ships with its window and zero-state hint",
        allowlist: &[],
        check: why_kpi_panel_shallow,
    }
}

// ---------------------------------------------------------------------------
// Adapter — the only place this file touches the real filesystem
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../")
}

/// The real repo, behind the two ports. Reads relative to the repo root.
struct Workspace {
    root: PathBuf,
}

impl ReadRepoText for Workspace {
    fn read_utf8(&self, rel: &str) -> Option<String> {
        fs::read_to_string(self.root.join(rel)).ok()
    }
}

impl ListRepoFiles for Workspace {
    fn list_scanned(&self, sub: &str) -> Vec<String> {
        let dir = self.root.join(sub);
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".js") {
                out.push(format!("{sub}/{name}"));
            }
        }
        out.sort();
        out
    }
}

/// The scan set of a live run: the web/js tree, of which the guarded
/// panel files are the part the depth contract speaks about.
fn workspace_ctx() -> (Vec<String>, Workspace) {
    let ws = Workspace { root: repo_root() };
    let files = ws.list_scanned("crates/presentation/src/web/js");
    (files, ws)
}

// ---------------------------------------------------------------------------
// Self-tests — the scaffold proves its own rules before the gates use it
// ---------------------------------------------------------------------------

#[test]
fn an_allowlist_that_grows_is_rejected() {
    let why = shrink_check(&["a.js"], &["a.js", "b.js"]).unwrap_err();
    assert!(why.contains("`b.js`"), "unhelpful: {why}");
    assert!(why.contains("fix the file instead"), "unhelpful: {why}");
}

#[test]
fn an_allowlist_that_shrinks_is_accepted() {
    shrink_check(&["a.js", "b.js"], &["a.js"])
        .expect("shrinking is the only legal direction");
}

#[test]
fn an_unchanged_allowlist_still_passes() {
    shrink_check(&["a.js"], &["a.js"]).expect("no change is not a grow");
}

const TILE: &str = r#"overviewKpiTile({label:"Shipped",num:m.shipped,text:String(m.shipped),
      series:bucketDaily(shipDays(r=>isF(byId[r.ticket])),days),
      hint:"ships land here when a ticket reaches documented",go:"nav('board')"})"#;

#[test]
fn the_exemplar_passes_a_compliant_panel() {
    let five = [TILE; 5].join(";\n");
    assert_eq!(why_kpi_panel_shallow(&five), None);
}

#[test]
fn the_exemplar_catches_a_tile_that_lost_its_zero_state_hint() {
    let no_hint = TILE.replace("hint:\"ships land here when a ticket reaches documented\",", "");
    let five = [no_hint.as_str(), TILE, TILE, TILE, TILE].join(";\n");
    let why = why_kpi_panel_shallow(&five).expect("the violation must be caught");
    assert!(why.contains("hint:"), "unhelpful: {why}");
    assert!(why.contains("depth regressed"), "unhelpful: {why}");
}

#[test]
fn the_exemplar_catches_a_tile_that_lost_its_window() {
    let no_series = TILE.replace("series:bucketDaily(shipDays(r=>isF(byId[r.ticket])),days),", "");
    let five = [TILE, &no_series, TILE, TILE, TILE].join(";\n");
    let why = why_kpi_panel_shallow(&five).expect("the violation must be caught");
    assert!(why.contains("series:"), "unhelpful: {why}");
}

#[test]
fn the_exemplar_catches_a_dropped_tile() {
    let why = why_kpi_panel_shallow(TILE).expect("4 tiles must be caught");
    assert!(why.contains("renders 1 tile(s), not the 5"), "unhelpful: {why}");
}

#[test]
fn a_missing_file_fails_closed() {
    // A scan over a tree whose files went missing must not report green.
    let inv = kpi_depth_invariant();
    let why = inv
        .scan(&["crates/presentation/src/web/js/kpis.js".to_owned()], &NullRepo)
        .unwrap_err();
    assert!(
        why.contains("not readable"),
        "an unreadable tree must fail closed: {why}"
    );
}

#[test]
fn the_checked_in_kpi_panel_obeys_the_depth_contract() {
    // Live gate: the real kpis.js, through the real adapter.
    let (_, ws) = workspace_ctx();
    kpi_depth_invariant()
        .scan(&[KPI_PANEL_FILE.to_owned()], &ws)
        .expect("web/js/kpis.js regressed: a tile lost its depth");
}

#[test]
fn the_scan_set_contains_the_guarded_panel() {
    // The adapter must actually see the panel file — a moved/renamed
    // kpis.js must move the guard with it, not silently green the scan.
    let (files, _) = workspace_ctx();
    assert!(
        files.iter().any(|f| f == KPI_PANEL_FILE),
        "`{KPI_PANEL_FILE}` vanished from the scan set — update the guard"
    );
}
