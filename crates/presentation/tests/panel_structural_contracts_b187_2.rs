//! CXA-B187.2 — structural contracts for the second batch of remaining
//! above-the-fold Overview panels, over the shared CXA-B186a harness.
//!
//! Batch 1 (CXA-B186a, `guardrail_scaffold_b201.rs`) took the pilot KPI tile
//! strip. This batch guards the NEXT five panels from the CXA-B204 inventory
//! — alerts strip, drain banner, activity feed, working-now strip, health
//! grid — each as a declarative contract over that panel's REAL markup: the
//! why/why-not copy, the class names, the attribution hooks and the feed
//! wiring a regression would silently drop. The decisions stay pure over the
//! text an adapter hands them (`ReadRepoText`); the filesystem appears only
//! in the `Workspace` adapter, per the hexagonal rule the scaffold set.
//!
//! The panels still waiting (Diagnostics disclosure, `#ov-charts`,
//! `#ov-attention`) are CXA-B187.3's batch and are NOT touched here.
//!
//! Ledger: `docs/CXA-B187-structural-contract-ledger.md`.

// Test-binary lints: unwrap-family is the point of a failing gate assertion.
#![allow(clippy::unwrap_in_result, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Ports — the only IO a structural check may perform (CXA-B186a pattern)
// ---------------------------------------------------------------------------

/// Outbound port: read a UTF-8 file relative to the repo root.
trait ReadRepoText {
    fn read_utf8(&self, rel: &str) -> Option<String>;
}

/// Outbound port: list the files a guardrail scans (repo-relative).
trait ListRepoFiles {
    fn list_scanned(&self, sub: &str) -> Vec<String>;
}

// ---------------------------------------------------------------------------
// The shared contract rule — one panel = one named set of required anchors
// ---------------------------------------------------------------------------

/// The structural contract of one panel. Every entry in `requires` is a real
/// substring of the panel's checked-in source today, so the contract can only
/// pass while that structure survives; losing it is a red build, not a
/// note in a doc nobody re-reads.
struct PanelContract {
    /// Stable name — what CI prints; the ledger links to it.
    name: &'static str,
    /// The panel this contract speaks for (shown on failure).
    panel: &'static str,
    /// Repo-relative file whose source must carry the structure.
    file: &'static str,
    /// Anchors the panel's real markup must keep.
    requires: &'static [&'static str],
    /// A structural rule beyond fixed needles (e.g. a card count).
    /// `None` = obeys. Most contracts need none.
    extra: fn(&str) -> Option<String>,
}

/// The no-extra-rule default.
fn no_extra(_: &str) -> Option<String> {
    None
}

impl PanelContract {
    /// What this contract holds against `src`. Empty = obeys.
    fn violations(&self, src: &str) -> Vec<String> {
        let mut v: Vec<String> = self
            .requires
            .iter()
            .filter(|n| !src.contains(**n))
            .map(|n| format!("markup lost: `{n}`"))
            .collect();
        if let Some(why) = (self.extra)(src) {
            v.push(why);
        }
        v
    }

    /// The pure decision for one panel over one file read.
    fn check(&self, read: &dyn ReadRepoText) -> Result<(), String> {
        let Some(src) = read.read_utf8(self.file) else {
            return Err(format!(
                "[{}] {} (`{}`) is not readable — refusing to guess (fail closed)",
                self.name, self.panel, self.file
            ));
        };
        let v = self.violations(&src);
        if v.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "[{}] {} (`{}`) drifted from its structural contract:\n{}",
                self.name,
                self.panel,
                self.file,
                v.iter()
                    .map(|s| format!("  - {s}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ))
        }
    }
}

/// The batch decision: every panel contract passes, or one failure naming
/// each drifted panel. An empty batch is a failure, not a green light —
/// a refactor that silently drops the registration must not pass CI.
fn check_batch(contracts: &[PanelContract], read: &dyn ReadRepoText) -> Result<(), String> {
    if contracts.is_empty() {
        return Err("the batch registered no structural contracts — nothing is guarded".to_owned());
    }
    let failed: Vec<String> = contracts
        .iter()
        .filter_map(|c| c.check(read).err())
        .collect();
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed.join("\n---\n"))
    }
}

/// Slice the body of one top-level `function NAME(` to the next top-level
/// `function` (or EOF). The web js keeps one statement per line, so this
/// scopes a needle to the function that owns it.
fn fn_body(js: &str, name: &str) -> Option<String> {
    let anchor = format!("function {name}(");
    let start = js.find(&anchor)?;
    let from = start + anchor.len();
    let end = js[from..]
        .find("\nfunction ")
        .map_or(js.len(), |p| from + p);
    Some(js[start..end].to_owned())
}

// ---------------------------------------------------------------------------
// The batch-2 contracts — one per remaining above-the-fold panel
// ---------------------------------------------------------------------------

/// `core.js` carries three of the five panels; `chat.js` and `shell.js` one
/// each. Kept next to the contracts that cite them.
const CORE_JS: &str = "crates/presentation/src/web/js/core.js";
const CHAT_JS: &str = "crates/presentation/src/web/js/chat.js";
const SHELL_JS: &str = "crates/presentation/src/web/js/shell.js";

/// Alerts strip (`#ov-alerts`, `alertsHtml` in `core.js`): a chip must name
/// WHAT broke and WHY it matters, carry an icon on a priority-coloured
/// `.panel` surface — and the why-not branch (no alerts = quiet) must stay.
fn alerts_contract() -> PanelContract {
    PanelContract {
        name: "overview/alerts-strip-says-what-and-why",
        panel: "Alerts strip (#ov-alerts)",
        file: CORE_JS,
        requires: &[
            "function alertsHtml(",
            "Last deployment failed",
            "Budget cap reached",
            "— loop paused",
            "DEV-BUG is prioritising fixes over features",
            "Reverted work",
            "decide in the Inbox",
            "planning weights them down next cycle",
            "class=\"panel\"",
            "<i class=\"ti ti-",
            "!al.length",
        ],
        extra: no_extra,
    }
}

/// Drain banner (`#ov-drain`, `drainBanner` in `shell.js`): a hold notice must
/// state the hold, the reason (all agents paused for a clean merge), the way
/// out (Review) and the token-free sweep affordance — not a bare pause.
fn drain_contract() -> PanelContract {
    PanelContract {
        name: "overview/drain-banner-names-the-hold-and-the-way-out",
        panel: "Drain banner (#ov-drain)",
        file: SHELL_JS,
        requires: &[
            "async function drainBanner(elId)",
            "class=\"drainbar\"",
            "CLEAN-BASE DRAIN",
            "All agents for all users PAUSE new work",
            "Open Review to merge green PRs",
            "SA merge sweep",
        ],
        extra: no_extra,
    }
}

/// Activity feed (`#ov-activity`, `actItem` in `core.js`): every row carries
/// WHO did it, WHAT they did and WHEN (title-tooltip timestamp), capped at a
/// page of 7, with the empty state naming what fills it.
fn activity_contract() -> PanelContract {
    PanelContract {
        name: "overview/activity-rows-carry-attribution",
        panel: "Activity feed (#ov-activity)",
        file: CORE_JS,
        requires: &[
            "function actItem(",
            ".map(actItem)",
            "class=\"tlrow\"",
            "class=\"tl-who\"",
            "class=\"tl-act\"",
            "class=\"tl-t\"",
            "slice(0,7)",
            "'<div class=\"empty\">no activity yet</div>'",
        ],
        extra: no_extra,
    }
}

/// Working-now strip (`#ov-working`, `renderOvWorking` in `core.js`): liveness
/// is fused from BOTH feeds (claim registry + fresh live-log ground truth),
/// each chip is a click-through naming the agent and its ticket, and the
/// true-zero case stays silent (a banner with nothing to say is absent).
fn working_contract() -> PanelContract {
    PanelContract {
        name: "overview/working-now-strip-lives-on-two-feeds",
        panel: "Working-now strip (#ov-working)",
        file: CORE_JS,
        requires: &[
            "function renderOvWorking()",
            "getElementById(\"ov-working\")",
            "fetch(api(\"/workers\"))",
            "fetch(api(\"/agent-liveness\"))",
            "const chips=[]",
            "if(!chips.length){setHTML(el,\"\");return;}",
            "class=\"ov-work-chip\"",
            "class=\"ov-work-dot\"",
            "working now",
            "openAgent('",
            ">120",
        ],
        extra: no_extra,
    }
}

/// The health grid renders its six metrics through ONE card builder inside
/// `renderHealth` — a metric dropped from the grid is a silent hole in the
/// front door, so the count is part of the contract.
const HEALTH_CARDS: usize = 6;

fn health_card_count(js: &str) -> Option<String> {
    let body = fn_body(js, "renderHealth")?;
    let n = body.matches("card(").count();
    if n == HEALTH_CARDS {
        None
    } else {
        Some(format!(
            "health grid renders {n} `card(` metric(s), not the {HEALTH_CARDS} \
             Team health ships — a metric was added or dropped without updating \
             the guard"
        ))
    }
}

/// Health grid (`#ov-health`, `renderHealth` in `chat.js`): the `.hgrid`
/// must ship its labelled, sub-captioned, click-through metric cards with
/// their thresholds (a number with its meaning, not a bare figure).
fn health_contract() -> PanelContract {
    PanelContract {
        name: "overview/health-grid-ships-six-attributed-metrics",
        panel: "Changelog + deploy health row (#ov-health)",
        file: CHAT_JS,
        requires: &[
            "function renderHealth(s)",
            "getElementById(\"ov-health\")",
            "Team health",
            "class=\"hgrid\"",
            "class=\"hval\"",
            "class=\"hlbl\"",
            "class=\"hsub\"",
            "Sprint velocity",
            "openBugs>4",
        ],
        extra: health_card_count,
    }
}

/// The batch-2 registry: the five panels this ticket guards, in inventory
/// order. CXA-B187.3 adds the rest (Diagnostics disclosure, `#ov-charts`,
/// `#ov-attention`) — do not grow this list here.
fn structural_contracts() -> Vec<PanelContract> {
    vec![
        alerts_contract(),
        drain_contract(),
        activity_contract(),
        working_contract(),
        health_contract(),
    ]
}

/// The files this batch guards. Kept as data so the CI wiring needs no
/// per-file list (one `--test panel_structural_contracts_b187_2` run covers
/// all five) and so a moved/renamed panel file is caught, not silently
/// unreadable. This set grows only when a new batch takes a panel over.
fn batch_guarded_files() -> Vec<&'static str> {
    let mut files: Vec<&'static str> = structural_contracts().iter().map(|c| c.file).collect();
    files.sort_unstable();
    files.dedup();
    files
}

/// The batch's guarded-file rule, the mirror of the scaffold's
/// allowlist-shrink rule (CXA-B186a): the guarded set may only GROW. A file
/// leaving the registry means a contract was deleted to make a red build
/// green — the regression this rule exists to catch — so it fails.
fn guarded_set_never_shrinks(before: &[&str], after: &[&str]) -> Result<(), String> {
    let now: std::collections::BTreeSet<&str> = after.iter().copied().collect();
    let dropped: Vec<&str> = before
        .iter()
        .copied()
        .filter(|f| !now.contains(f))
        .collect();
    if dropped.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "guarded file(s) {} left the batch registry — deleting a contract \
             to make a red build green is the regression this rule exists to \
             catch; fix the panel instead",
            dropped
                .iter()
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

// ---------------------------------------------------------------------------
// Adapter — the only place this file touches the real filesystem
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../")
}

/// The real repo, behind the ports. Reads relative to the repo root.
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
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(self.root.join(sub)) else {
            return out;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if std::path::Path::new(&name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("js"))
            {
                out.push(format!("{sub}/{name}"));
            }
        }
        out.sort();
        out
    }
}

/// The never-readable repo — what an empty tree hands back, so "unreadable"
/// and "missing" are indistinguishable to a check (fail closed either way).
struct NullRepo;

impl ReadRepoText for NullRepo {
    fn read_utf8(&self, _rel: &str) -> Option<String> {
        None
    }
}

/// The live reader with chosen files deliberately malformed — the negative
/// tests mutate a panel's structure the way a careless refactor would,
/// WITHOUT touching the working tree, and prove the batch goes red while
/// the untouched panels stay green. `over` maps a repo-relative path to
/// replacement source; anything else falls through to the real repo.
struct MalformedRepo {
    over: BTreeMap<String, String>,
    inner: Workspace,
}

impl ReadRepoText for MalformedRepo {
    fn read_utf8(&self, rel: &str) -> Option<String> {
        if let Some(text) = self.over.get(rel) {
            return Some(text.clone());
        }
        self.inner.read_utf8(rel)
    }
}

/// Build a `MalformedRepo` whose named files have been mutated in memory:
/// every `(from, to)` pair is a `str::replace` over that file's checked-in
/// source — exactly the edit a careless refactor would make.
fn malformed(file_edits: &[(&str, &[(&str, &str)])]) -> MalformedRepo {
    let inner = Workspace { root: repo_root() };
    let mut over = BTreeMap::new();
    for (rel, edits) in file_edits {
        let src = inner
            .read_utf8(rel)
            .unwrap_or_else(|| panic!("{rel} must be readable to mutate it in memory"));
        let mut mutated = src;
        for (from, to) in *edits {
            mutated = mutated.replace(from, to);
        }
        over.insert((*rel).to_owned(), mutated);
    }
    MalformedRepo { over, inner }
}

// ---------------------------------------------------------------------------
// Self-tests — the batch proves its own rules before pointing at the repo
// ---------------------------------------------------------------------------

const ALERTS_FIXTURE: &str = r#"
function alertsHtml(s,m,spend){
  const al=[];
  if(s.deploy&&!s.deploy.ok)al.push(["red","cloud-x","Last deployment failed",esc(s.deploy.summary)]);
  if(bud&&spend.total_cost_usd>=bud)al.push(["red","alert-triangle","Budget cap reached","$3 of $3 — loop paused"]);
  if(m.openBugs>=5)al.push(["amber","bug",m.openBugs+" open bugs","DEV-BUG is prioritising fixes over features"]);
  const rv=[{decision:"pending"}];
  if(rv.length){
    const pend=rv.filter(e=>e.decision==="pending").length,okd=rv.length-pend;
    if(pend)al.push(["amber","arrow-back-up","Reverted work",pend+" detected revert needs your review — decide in the Inbox"]);
    else if(okd)al.push(["red","arrow-back-up","Reverted work",okd+" confirmed reverts — planning weights them down next cycle"]);
  }
  if(!al.length)return "";
  return al.map(([c,ic,t,d])=>`<div class="panel" style="border-color:var(--${c})">
    <i class="ti ti-${ic}"></i><div>${t}</div></div>`).join("");
}
function nextFn(){}
"#;

#[test]
fn a_compliant_panel_source_passes_its_contract() {
    let c = alerts_contract();
    let v = c.violations(ALERTS_FIXTURE);
    assert!(v.is_empty(), "fixture must obey: {v:?}");
}

#[test]
fn a_panel_that_lost_its_why_copy_is_caught() {
    let c = alerts_contract();
    let broken = ALERTS_FIXTURE.replace("DEV-BUG is prioritising fixes over features", "");
    let v = c.violations(&broken);
    assert!(
        v.iter().any(|s| s.contains("DEV-BUG is prioritising")),
        "the lost why-copy must be named: {v:?}"
    );
}

#[test]
fn a_panel_that_lost_its_attribution_is_caught() {
    let c = activity_contract();
    let broken = "function actItem(a){return '<div class=\"tlrow\">x</div>';}
function render(){items.map(actItem)}";
    let v = c.violations(broken);
    assert!(
        v.iter().any(|s| s.contains("tl-who")),
        "a row without WHO must be caught: {v:?}"
    );
    assert!(
        v.iter().any(|s| s.contains("tl-t")),
        "a row without WHEN must be caught: {v:?}"
    );
}

#[test]
fn a_panel_that_lost_its_second_feed_is_caught() {
    let c = working_contract();
    let broken = "function renderOvWorking(){const el=document.getElementById(\"ov-working\");
fetch(api(\"/workers\")).then(r=>r.json());const chips=[];
if(!chips.length){setHTML(el,\"\");return;}
setHTML(el,`<div class=\"ov-work-chip\" onclick=\"openAgent('DEV')\"><span class=\"ov-work-dot\"></span>working now</div>`);}";
    let v = c.violations(broken);
    assert!(
        v.iter().any(|s| s.contains("/agent-liveness")),
        "losing the liveness ground-truth feed must be caught: {v:?}"
    );
}

#[test]
fn a_health_grid_that_lost_a_metric_card_is_caught() {
    let mut five_cards = String::from("function renderHealth(s){\n");
    for _ in 0..HEALTH_CARDS - 1 {
        five_cards.push_str("  card(\"lbl\",\"1\",\"sub\",\"col\",\"go\")\n");
    }
    five_cards.push_str("}\nfunction render(s){}\n");
    let why = (health_contract().extra)(five_cards.as_str()).expect("5 of 6 cards must be caught");
    assert!(
        why.contains("5 `card(` metric(s), not the 6"),
        "unhelpful: {why}"
    );
}

#[test]
fn the_health_card_count_passes_a_full_grid() {
    let mut six_cards = String::from("function renderHealth(s){\n");
    for _ in 0..HEALTH_CARDS {
        six_cards.push_str("  card(\"lbl\",\"1\",\"sub\",\"col\",\"go\")\n");
    }
    six_cards.push_str("}\nfunction render(s){}\n");
    assert_eq!((health_contract().extra)(six_cards.as_str()), None);
}

#[test]
fn fn_body_scopes_the_slice_to_one_function() {
    let js = "function a(){card(1);card(2);}\nfunction b(){card(3);}\n";
    let body = fn_body(js, "a").expect("a must be found");
    assert!(body.contains("card(1)"));
    assert!(!body.contains("card(3)"), "b's cards must not leak into a");
    assert_eq!(fn_body(js, "missing"), None);
}

#[test]
fn a_missing_panel_file_fails_closed() {
    let why = alerts_contract()
        .check(&NullRepo)
        .expect_err("the gate must fail, not pass");
    assert!(
        why.contains("not readable"),
        "an unreadable panel must fail closed: {why}"
    );
}

#[test]
fn an_empty_batch_is_a_failure_not_a_green_light() {
    let why = check_batch(&[], &NullRepo).expect_err("nothing guarded must fail");
    assert!(why.contains("no structural contracts"), "unhelpful: {why}");
}

#[test]
fn the_batch_two_allowlist_only_shrinks() {
    // The scaffold's allowlist-shrink rule (CXA-B186a), restated for the
    // batch registry: adding a file to the guarded set is the only legal
    // direction; a file must never silently LEAVE the guarded set — a
    // contract deleted to make a red build green is the regression this
    // rule exists to catch. Before = batch 2's five panels; after = batch 2
    // plus a hypothetical CXA-B187.3 migration.
    let batch_two = [
        "crates/presentation/src/web/js/chat.js",
        "crates/presentation/src/web/js/core.js",
        "crates/presentation/src/web/js/shell.js",
    ];
    let with_batch_three = [
        "crates/presentation/src/web/js/chat.js",
        "crates/presentation/src/web/js/core.js",
        "crates/presentation/src/web/js/kpis.js",
        "crates/presentation/src/web/js/shell.js",
    ];
    guarded_set_never_shrinks(&batch_two, &with_batch_three)
        .expect("adding newly-guarded files is the legal direction");
    let why = guarded_set_never_shrinks(&with_batch_three, &batch_two)
        .expect_err("a file leaving the guarded set must fail");
    assert!(why.contains("fix the panel instead"), "unhelpful: {why}");
}

// ---------------------------------------------------------------------------
// Live gates — the checked-in panels, through the real adapter
// ---------------------------------------------------------------------------

#[test]
fn the_batch_declares_exactly_the_five_remaining_panels() {
    let contracts = structural_contracts();
    assert_eq!(
        contracts.len(),
        5,
        "batch 2 guards five panels; growing it belongs to CXA-B187.3's ledger row"
    );
    let files = batch_guarded_files();
    assert_eq!(files.len(), 3, "core.js carries three of the five panels");
    for f in files {
        assert!(
            f.starts_with("crates/presentation/src/web/js/"),
            "a contract must cite a real web js file: {f}"
        );
    }
}

#[test]
fn the_scan_set_still_contains_every_batch_two_panel_file() {
    // A moved/renamed panel file must move its contract with it — a silently
    // unreadable file would green nothing (the missing-file test above
    // proves the check itself still fails closed).
    let ws = Workspace { root: repo_root() };
    let files = ws.list_scanned("crates/presentation/src/web/js");
    for f in batch_guarded_files() {
        assert!(
            files.iter().any(|scan| scan == f),
            "`{f}` vanished from the scan set — update the contract"
        );
    }
}

#[test]
fn the_checked_in_batch_two_panels_obey_their_structural_contracts() {
    let contracts = structural_contracts();
    let ws = Workspace { root: repo_root() };
    check_batch(&contracts, &ws)
        .unwrap_or_else(|why| panic!("an above-the-fold Overview panel regressed:\n{why}"));
}

// ---------------------------------------------------------------------------
// Negative tests — the batch must fail a deliberately malformed structure
// ---------------------------------------------------------------------------

#[test]
fn panel_contracts_fail_a_deliberately_malformed_alerts_strip() {
    // Mutate core.js the way a careless refactor would: rename the alerts
    // entrypoint, drop the why-copy and the why-not branch — while leaving
    // the activity and working-now panels (same file) intact.
    let repo = malformed(&[(
        CORE_JS,
        &[
            ("function alertsHtml(", "function renderAlerts("),
            ("DEV-BUG is prioritising fixes over features", ""),
            ("if(!al.length)return \"\";", ""),
        ],
    )]);
    let why = check_batch(&structural_contracts(), &repo)
        .expect_err("a malformed alerts strip must fail the batch");
    assert!(
        why.contains("overview/alerts-strip-says-what-and-why"),
        "the failure must name the drifted contract: {why}"
    );
    assert!(
        why.contains("function alertsHtml(") && why.contains("DEV-BUG is prioritising"),
        "each lost anchor must be listed: {why}"
    );
    // Attribution: exactly one panel is red, so the fixer knows where to look.
    for untouched in [
        "overview/drain-banner-names-the-hold-and-the-way-out",
        "overview/activity-rows-carry-attribution",
        "overview/working-now-strip-lives-on-two-feeds",
        "overview/health-grid-ships-six-attributed-metrics",
    ] {
        assert!(
            !why.contains(untouched),
            "{untouched} is intact and must not be blamed: {why}"
        );
    }
}

#[test]
fn panel_contracts_fail_a_deliberately_malformed_health_grid() {
    // A metric silently dropped from the Team health grid: the count rule
    // must catch what a needle-per-card list would miss.
    let repo = malformed(&[(
        CHAT_JS,
        &[("card(\"Refactor debt\"", "cardX(\"Refactor debt\"")],
    )]);
    let why = check_batch(&structural_contracts(), &repo)
        .expect_err("a five-card health grid must fail the batch");
    assert!(
        why.contains("overview/health-grid-ships-six-attributed-metrics"),
        "the failure must name the health contract: {why}"
    );
    assert!(
        why.contains("not the 6"),
        "the count delta must be spelled out: {why}"
    );
    assert!(
        !why.contains("overview/activity-rows-carry-attribution"),
        "core.js panels are untouched and must not be blamed: {why}"
    );
}
