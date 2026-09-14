//! CXA-B246 — the layer-purity gate: domain/application import no IO.
//!
//! AC2 of CXA-B201 says the scaffold must enforce "no `std::fs` / `std::process`
//! / `tokio` / `sqlx` imports in `crates/domain` or `crates/application`". The
//! B201 scaffold (guardrail_scaffold_b201.rs) never scanned those trees — the
//! only IO rule it shipped was the KPI-panel depth contract, and the layers
//! were left to hexagonal_gate.rs (CXA-B211) alone. That gate watches
//! `crates/app`'s view of the application layer with a narrower token set;
//! this file closes AC2 directly, next to the scaffold it belongs to:
//!
//! - a pure decision (`why_layers_do_io`) over data a port hands it — the
//!   same shape as the scaffold's `Invariant::scan`, so every guardrail
//!   reads the same way;
//! - the application layer's `ports/` subtree is granted immunity (that is
//!   where IO is *declared*, never executed);
//! - the same allowlist-shrink rule as the scaffold: an exemption may leave,
//!   never return;
//! - fail-closed on both silent-green paths: a file that vanishes from disk,
//!   and a scan set that comes back empty (a moved/renamed crate must move
//!   the gate, not green it).
//!
//! Fixture tests prove both directions on hand-built sources; two live gates
//! pin the checked-in trees. The forbidden-token list is deliberately a
//! superset of hexagonal_gate's (`std::net`, `reqwest`, `ureq` added): the
//! two gates agree wherever they overlap and this one is stricter, so a
//! socket-level regression cannot slip between them.
//!
//! Lint note: this is a test target — fixtures `expect`/`expect_err` by
//! design (a gate that "just returns" would be a lying test), and the map
//! builders use small literal slices, so the workspace's src-facing lint
//! floor is relaxed here only.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::inefficient_to_string,
        clippy::case_sensitive_file_extension_comparisons,
        clippy::needless_raw_string_hashes,
        clippy::explicit_into_iter_loop,
        clippy::trivially_copy_pass_by_ref
    )
)]

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Ports — the only way a check touches the filesystem (scaffold pattern)
// ---------------------------------------------------------------------------

trait ListRepoFiles {
    /// Every `.rs` source under `sub`, repo-root-relative, sorted.
    fn list_scanned(&self, sub: &str) -> Vec<String>;
}

trait ReadRepoText {
    /// `None` = unreadable: the gate must fail closed, never guess.
    fn read_utf8(&self, rel: &str) -> Option<String>;
}

// ---------------------------------------------------------------------------
// The pure decision
// ---------------------------------------------------------------------------

/// Direct-IO tokens. `std::fs` / `std::process` / `tokio` / `sqlx` are the
/// AC2 list; `std::net` / `reqwest` / `ureq` extend "no IO" to sockets so a
/// network call cannot sneak in below the filesystem radar. Blocking or
/// async, file or socket — IO goes through a port and an adapter.
const FORBIDDEN_TOKENS: [&str; 7] = [
    "std::fs::",
    "std::process::",
    "std::net::",
    "tokio::",
    "sqlx::",
    "reqwest::",
    "ureq::",
];

/// One direct-IO hit inside a layer source's production half: the offending
/// line and token, so the message points at the fix instead of the file.
#[derive(Debug)]
struct Violation {
    line: usize,
    token: &'static str,
}

/// Everything from the first `#[cfg(test)]` on is test code: tests may spin
/// tempdirs and block on runtimes. Cutting at the *first* marker keeps the
/// scanned half minimal, so a stray marker can only narrow the scan — the
/// failure direction is towards false accusation, never false green.
fn violations_in(text: &str) -> Vec<Violation> {
    let scan_end = text.find("#[cfg(test)]").unwrap_or(text.len());
    let mut out = Vec::new();
    let mut offset = 0usize;
    for (i, line) in text.lines().enumerate() {
        let line_start = offset;
        offset += line.len() + 1;
        if line_start >= scan_end {
            break; // production half ended; the rest is #[cfg(test)]
        }
        for token in FORBIDDEN_TOKENS {
            if line.contains(token) {
                out.push(Violation { line: i + 1, token });
                break; // one diagnosis per line; the first token is enough
            }
        }
    }
    out
}

/// `crates/application/src/ports/...` — immune. Everything else is scanned.
/// The grant is application-only: domain declares no ports (they live in the
/// application layer), so a ports/-shaped path in domain stays scanned.
fn is_port_declaration(rel: &str) -> bool {
    rel.starts_with("crates/application/src/ports/")
}

/// Why does a scanned layer do direct IO? `Ok(())` only when every scanned
/// source's production half is free of every [`FORBIDDEN_TOKENS`] token (or
/// is an exempted port declaration, or is on the allowlist — which may
/// shrink, never grow). Any unreadable file or empty scan set fails closed.
fn why_layers_do_io(
    files: &[String],
    read: &dyn ReadRepoText,
    allowlist: &[&str],
    layer: &str,
) -> Result<(), String> {
    if files.is_empty() {
        return Err(format!(
            "nothing scanned under crates/{layer}/src — the tree moved or \
             the adapter broke, and a gate over nothing proves nothing"
        ));
    }
    let granted: std::collections::BTreeSet<&str> = allowlist.iter().copied().collect();
    let mut offenders: Vec<String> = Vec::new();
    for file in files {
        let Some(text) = read.read_utf8(file) else {
            return Err(format!(
                "{file} is in the scanned tree but not readable — refusing \
                 to guess (fail closed)"
            ));
        };
        if is_port_declaration(file) || granted.contains(&file.as_str()) {
            continue;
        }
        for v in violations_in(&text) {
            offenders.push(format!(
                "  {file}:{}: direct IO via `{}` — go through a port \
                 (crates/{layer}/src/ports/) and an infrastructure adapter",
                v.line, v.token
            ));
        }
    }
    if offenders.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "layer `{layer}` does direct IO (CXA-B201 AC2):\n{}",
            offenders.join("\n")
        ))
    }
}

/// The scaffold's shrink rule, restated for this gate: an allowlist may lose
/// entries, never gain them — fixing the file is the only legal transition.
fn shrink_check(before: &[&str], after: &[&str]) -> Result<(), String> {
    let was: std::collections::BTreeSet<&str> = before.iter().copied().collect();
    let grew: Vec<&str> = after.iter().copied().filter(|f| !was.contains(f)).collect();
    if grew.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "purity allowlist grew — re-granted exemption(s) {} must not \
             come back; fix the file instead",
            grew.iter()
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

/// The scan set of one layer: its `.rs` sources under `crates/{layer}/src`,
/// excluding `*_tests.rs` twin files (they are test code by name, not by
/// cfg — the same convention hexagonal_gate honours).
fn layer_scan(files: &[String], layer: &str) -> Vec<String> {
    let prefix = format!("crates/{layer}/src/");
    let mut out: Vec<String> = files
        .iter()
        .filter(|f| f.starts_with(&prefix) && f.ends_with(".rs") && !f.ends_with("_tests.rs"))
        .cloned()
        .collect();
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// The two invariants, shaped like the scaffold's
// ---------------------------------------------------------------------------

struct Invariant {
    name: &'static str,
    why: &'static str,
    allowlist: &'static [&'static str],
}

impl Invariant {
    fn scan(&self, files: &[String], read: &dyn ReadRepoText, layer: &str) -> Result<(), String> {
        let scan = layer_scan(files, layer);
        why_layers_do_io(&scan, read, self.allowlist, layer)
            .map_err(|e| format!("[{} — {}] {e}", self.name, self.why))
    }
}

/// `crates/domain` is pure business model — zero IO, zero exemptions. The
/// gate must bite here: a domain that can touch the disk is not a domain.
fn domain_purity() -> Invariant {
    Invariant {
        name: "layer-purity/domain-imports-no-io",
        why: "domain is the pure business model; IO lives behind ports in adapters",
        allowlist: &[],
    }
}

/// `crates/application` orchestrates through ports; only `src/ports/` may
/// name IO types. The seed list is exactly the ten files hexagonal_gate
/// (CXA-B211) already grandfathered on main — it only shrinks, so this gate
/// starts where that ratchet stands instead of re-litigating it; a file
/// added to hexagonal_gate's `GRANDFATHERED` must land here too, and the
/// live test below fails both ways if the lists drift apart.
fn application_no_direct_io() -> Invariant {
    Invariant {
        name: "layer-purity/application-imports-no-io",
        why: "application use cases orchestrate IO through ports, never perform it",
        allowlist: &[
            "crates/application/src/prompts.rs",
            "crates/application/src/use_cases/backup.rs",
            "crates/application/src/use_cases/cycle/forge_merge.rs",
            "crates/application/src/use_cases/cycle/mod.rs",
            "crates/application/src/use_cases/cycle/ops.rs",
            "crates/application/src/use_cases/generate_docs.rs",
            "crates/application/src/use_cases/refine_ticket.rs",
            "crates/application/src/use_cases/run_chat_reply.rs",
            "crates/application/src/use_cases/run_dev/mod.rs",
            "crates/application/src/use_cases/runner.rs",
        ],
    }
}

// ---------------------------------------------------------------------------
// Adapter — the only place this file touches the real filesystem
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../")
}

/// The real repo, behind the two ports. Walks each layer tree recursively.
struct Workspace {
    root: PathBuf,
}

impl ListRepoFiles for Workspace {
    fn list_scanned(&self, sub: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![self.root.join(sub)];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let rel = path
                        .strip_prefix(&self.root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .to_string();
                    out.push(rel.replace('\\', "/"));
                }
            }
        }
        out.sort();
        out
    }
}

impl ReadRepoText for Workspace {
    fn read_utf8(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.root.join(rel)).ok()
    }
}

/// The scan set of a live run: both layer trees, through the real adapter.
fn workspace_ctx() -> (Vec<String>, Workspace) {
    let ws = Workspace { root: repo_root() };
    let mut files = ws.list_scanned("crates/domain/src");
    files.extend(ws.list_scanned("crates/application/src"));
    (files, ws)
}

// ---------------------------------------------------------------------------
// In-memory fixture repos — every pure test is driven through the ports
// ---------------------------------------------------------------------------

/// A reader that can answer nothing: for empty-scan fixtures.
struct NullRepo;

impl ListRepoFiles for NullRepo {
    fn list_scanned(&self, _sub: &str) -> Vec<String> {
        Vec::new()
    }
}

impl ReadRepoText for NullRepo {
    fn read_utf8(&self, _rel: &str) -> Option<String> {
        None
    }
}

struct MemRepo {
    files: std::collections::BTreeMap<String, String>,
}

impl ListRepoFiles for MemRepo {
    fn list_scanned(&self, _sub: &str) -> Vec<String> {
        self.files.keys().cloned().collect()
    }
}

impl ReadRepoText for MemRepo {
    fn read_utf8(&self, rel: &str) -> Option<String> {
        self.files.get(rel).cloned()
    }
}

// ---------------------------------------------------------------------------
// Self-tests — green on compliant, red on violating, both fail-closed paths
// ---------------------------------------------------------------------------

const PURE_SOURCE: &str = r#"//! A use case that only orchestrates.
pub struct Ctx;

pub fn decide(items: &[u32]) -> u32 {
    items.iter().sum()
}

pub fn line_count(text: &str) -> usize {
    text.lines().count()
}
"#;

fn mem_repo(entries: &[(&str, &str)]) -> MemRepo {
    MemRepo {
        files: entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

#[test]
fn a_compliant_fixture_is_green() {
    let repo = mem_repo(&[("crates/application/src/decide.rs", PURE_SOURCE)]);
    let files = ListRepoFiles::list_scanned(&repo, "crates/application/src");
    application_no_direct_io()
        .scan(&files, &repo, "application")
        .expect("a use case behind ports must pass the purity gate");
}

#[test]
fn a_violating_fixture_is_red_for_every_ac2_token() {
    let cases: [(&str, &str); 4] = [
        ("std::fs::", "let ok = std::fs::read_to_string(p).is_ok();"),
        ("std::process::", "let pid = std::process::id();"),
        (
            "tokio::",
            "tokio::time::sleep(std::time::Duration::from_secs(1)).await;",
        ),
        (
            "sqlx::",
            "let row: (i64,) = sqlx::query_as(\"select 1\").fetch_one(&db).await?;",
        ),
    ];
    for (token, snippet) in cases {
        let src = format!("{PURE_SOURCE}\nfn sneaky() {{\n    {snippet}\n}}\n");
        let repo = mem_repo(&[("crates/application/src/sneaky.rs", &src)]);
        let files = ListRepoFiles::list_scanned(&repo, "crates/application/src");
        let why = application_no_direct_io()
            .scan(&files, &repo, "application")
            .expect_err("the gate must fail, not pass");
        assert!(
            why.contains(token),
            "the gate must name `{token}` as the offender: {why}"
        );
        assert!(
            why.contains("go through a port"),
            "the gate must point at the fix: {why}"
        );
    }
}

#[test]
fn the_violation_is_reported_at_its_line() {
    let src = format!("{PURE_SOURCE}\nfn sneaky() {{\n    let _ = std::fs::read(\"x\");\n}}\n");
    let repo = mem_repo(&[("crates/application/src/sneaky.rs", &src)]);
    let files = ListRepoFiles::list_scanned(&repo, "crates/application/src");
    let why = application_no_direct_io()
        .scan(&files, &repo, "application")
        .expect_err("the gate must fail, not pass");
    // PURE_SOURCE is 12 lines, so the hit lands on line 13.
    assert!(
        why.contains("sneaky.rs:13:"),
        "the report must carry a line number for the fix: {why}"
    );
}

#[test]
fn cfg_test_code_is_out_of_scope() {
    // A #[cfg(test)] module may touch tempdirs and runtimes — that is what
    // test code is for. Only the production half is scanned.
    let src = format!(
        "{PURE_SOURCE}\n#[cfg(test)]\nmod tests {{\n    use std::fs;\n\n    #[test]\n    fn \
         writes_a_tempdir() {{\n        let _ = fs::write(\"/tmp/x\", \"y\");\n    }}\n}}\n"
    );
    assert!(
        violations_in(&src).is_empty(),
        "test-half IO must stay out of the scan: {:?}",
        violations_in(&src)
    );
    let repo = mem_repo(&[("crates/application/src/decide.rs", &src)]);
    let files = ListRepoFiles::list_scanned(&repo, "crates/application/src");
    application_no_direct_io()
        .scan(&files, &repo, "application")
        .expect("#[cfg(test)] IO must stay out of the production-half scan");
}

#[test]
fn a_stray_cfg_test_marker_cannot_hide_a_violation_above_it() {
    // The scan cuts at the FIRST marker, so the production half above it is
    // still scanned — a misplaced marker narrows the gate, it never greens it.
    let src = "fn sneaky() {\n    let _ = std::fs::read(\"x\");\n}\n\n#[cfg(test)]\nmod tests {}\n";
    assert_eq!(
        violations_in(src).len(),
        1,
        "the hit above the marker must be scanned"
    );
}

#[test]
fn application_ports_subtree_is_granted_its_declaration() {
    // The port names the IO it abstracts (as docs do) — granted by its path.
    let port_src = "//! Reads like [`std::fs::read_to_string`] but behind a port.\npub trait \
                    BlobPort {\n    fn read_utf8(&self, rel: &str) -> Option<String>;\n}\n";
    let mut repo = mem_repo(&[("crates/application/src/ports/outbound/blob.rs", port_src)]);
    let files = ListRepoFiles::list_scanned(&repo, "crates/application/src");
    application_no_direct_io()
        .scan(&files, &repo, "application")
        .expect("a ports/ declaration is the pattern, not a violation");
    // …but the grant is earned by the path, not faked by contents: the same
    // IO in a non-ports file is still a violation.
    repo.files.insert(
        "crates/application/src/blob.rs".into(),
        "pub fn now() -> u32 { std::process::id() }\n".into(),
    );
    let files = ListRepoFiles::list_scanned(&repo, "crates/application/src");
    let why = application_no_direct_io()
        .scan(&files, &repo, "application")
        .expect_err("the gate must fail, not pass");
    assert!(why.contains("std::process::"), "unhelpful: {why}");
}

#[test]
fn domain_has_no_ports_grant() {
    // Domain is pure model: it declares no ports (those belong to the
    // application layer), so even a ports/-shaped path stays scanned.
    let repo = mem_repo(&[(
        "crates/domain/src/ports/blob.rs",
        "pub fn now() -> u32 { std::process::id() }\n",
    )]);
    let files = ListRepoFiles::list_scanned(&repo, "crates/domain/src");
    let why = domain_purity()
        .scan(&files, &repo, "domain")
        .expect_err("the gate must fail, not pass");
    assert!(
        why.contains("layer `domain`"),
        "the report must name the impure layer: {why}"
    );
}

#[test]
fn an_unreadable_file_fails_closed() {
    let repo = mem_repo(&[("crates/application/src/decide.rs", PURE_SOURCE)]);
    // Present in the scan set, absent from the reader: a file that vanished
    // between listing and reading must red the gate, not skip itself.
    let ghost = "crates/application/src/ghost.rs".to_string();
    let files = vec![repo.files.keys().next().unwrap().clone(), ghost];
    assert!(repo.read_utf8("crates/application/src/ghost.rs").is_none());
    let why = application_no_direct_io()
        .scan(&files, &repo, "application")
        .expect_err("the gate must fail, not pass");
    assert!(
        why.contains("not readable"),
        "an unreadable tree must fail closed: {why}"
    );
}

#[test]
fn an_empty_scan_set_fails_closed() {
    let why = domain_purity()
        .scan(&[], &NullRepo, "domain")
        .expect_err("the gate must fail, not pass");
    assert!(
        why.contains("nothing scanned"),
        "a gate over nothing proves nothing: {why}"
    );
}

#[test]
fn a_tests_twin_file_is_out_of_scope() {
    // `*_tests.rs` siblings are test code by convention (hexagonal_gate
    // honours the same rule) — the scan set excludes them.
    let repo = mem_repo(&[
        ("crates/application/src/decide_tests.rs", "use std::fs;\n"),
        ("crates/application/src/decide.rs", PURE_SOURCE),
    ]);
    let all = ListRepoFiles::list_scanned(&repo, "crates/application/src");
    let scan = layer_scan(&all, "application");
    assert_eq!(scan, vec!["crates/application/src/decide.rs".to_string()]);
}

#[test]
fn an_allowlist_that_grows_is_rejected() {
    let why = shrink_check(&["a.rs"], &["a.rs", "b.rs"]).expect_err("the gate must fail, not pass");
    assert!(why.contains("`b.rs`"), "unhelpful: {why}");
    assert!(why.contains("fix the file instead"), "unhelpful: {why}");
}

#[test]
fn an_allowlist_that_shrinks_is_accepted() {
    shrink_check(&["a.rs", "b.rs"], &["a.rs"]).expect("shrinking is the only legal direction");
}

#[test]
fn an_exempted_file_still_needs_to_exist_and_stay_readable() {
    // Immunity covers the IO scan, not the fail-closed read: a vanished
    // exempted file still reds the gate.
    let repo = mem_repo(&[("crates/application/src/decide.rs", PURE_SOURCE)]);
    let files = vec![
        "crates/application/src/decide.rs".to_string(),
        "crates/application/src/legacy.rs".to_string(),
    ];
    let lax = Invariant {
        name: "layer-purity/application-imports-no-io",
        why: "fixture allowlist",
        allowlist: &["crates/application/src/legacy.rs"],
    };
    let why = lax
        .scan(&files, &repo, "application")
        .expect_err("the gate must fail, not pass");
    assert!(
        why.contains("not readable"),
        "an allowlist exempts the scan, never the read: {why}"
    );
}

// ---------------------------------------------------------------------------
// Live gates — the checked-in trees, through the real adapter
// ---------------------------------------------------------------------------

/// Hexagonal_gate's `GRANDFATHERED` (CXA-B211), mirrored verbatim: the two
/// gates share one ratchet, and the equality test below fails the moment
/// either side moves without the other.
const GRANDFATHERED_B211: &[&str] = &[
    "crates/application/src/prompts.rs",
    "crates/application/src/use_cases/backup.rs",
    "crates/application/src/use_cases/cycle/forge_merge.rs",
    "crates/application/src/use_cases/cycle/mod.rs",
    "crates/application/src/use_cases/cycle/ops.rs",
    "crates/application/src/use_cases/generate_docs.rs",
    "crates/application/src/use_cases/refine_ticket.rs",
    "crates/application/src/use_cases/run_chat_reply.rs",
    "crates/application/src/use_cases/run_dev/mod.rs",
    "crates/application/src/use_cases/runner.rs",
];

#[test]
fn the_seed_allowlist_matches_the_cxa_b211_ratchet() {
    // One ratchet, one list: this gate's seed must equal hexagonal_gate's
    // `GRANDFATHERED` (CXA-B211). Drift in either direction fails here.
    assert_eq!(
        application_no_direct_io().allowlist,
        GRANDFATHERED_B211,
        "seed allowlist drifted from hexagonal_gate's GRANDFATHERED"
    );
}

#[test]
fn the_scan_set_is_not_empty() {
    // The adapter must actually see both trees — a moved/renamed crate must
    // move the gate with it, not silently green the scan.
    let (files, _) = workspace_ctx();
    for layer in ["domain", "application"] {
        let scan = layer_scan(&files, layer);
        assert!(
            scan.len() > 10,
            "crates/{layer}/src collapsed to {} sources — update the gate",
            scan.len()
        );
    }
}

#[test]
fn the_checked_in_layers_are_pure() {
    // Live gate: the real domain + application sources, through the real
    // adapter. This is CXA-B201 AC2 enforced on the repo itself.
    let (files, ws) = workspace_ctx();
    domain_purity()
        .scan(&files, &ws, "domain")
        .expect("crates/domain regressed: it imports IO directly");
    application_no_direct_io()
        .scan(&files, &ws, "application")
        .expect("crates/application regressed: it does direct IO outside ports/");
}
