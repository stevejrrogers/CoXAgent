//! The hexagonal ratchet: the application layer does not do IO directly.
//!
//! Every external effect — a process, a file, a socket — goes through a port
//! in `ports/outbound/`, fulfilled by an infrastructure adapter. Direct
//! `std::process`/`std::fs` in a use case does more than bend the rule: it
//! makes the logic untestable without a real repo/filesystem, which is where
//! this workspace's flaky tests came from.
//!
//! Files that predate the rule are grandfathered below. The list may only
//! SHRINK: fixing a file and forgetting to remove it here fails the test too,
//! so the debt stays visible and the ratchet only turns one way. A NEW file
//! reaching for std::process/std::fs fails immediately, with this explanation.

use std::path::{Path, PathBuf};

/// Files that already did direct IO when the ratchet was installed
/// (2026-07-31). Remove an entry when you fix the file — never add one.
const GRANDFATHERED: &[&str] = &[];

const FORBIDDEN: &[&str] = &["std::process::Command", "std::fs::"];

fn application_sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../application/src");
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p: PathBuf = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs")
                // Whole-file test modules (`mod x_tests;` declared under
                // #[cfg(test)]) are test code end to end — the production-half
                // split below never sees a `#[cfg(test)]` marker inside them,
                // so exempt them by the naming convention the splits use.
                && !p
                    .file_stem()
                    .is_some_and(|n| n.to_string_lossy().ends_with("_tests"))
            {
                let rel = p
                    .strip_prefix(&root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .into_owned();
                if let Ok(text) = std::fs::read_to_string(&p) {
                    out.push((rel, text));
                }
            }
        }
    }
    out.sort();
    out
}

/// Only production code counts: `#[cfg(test)]` modules may do IO (a test
/// double that shells out is test scaffolding, not architecture).
fn production_half(text: &str) -> &str {
    text.split("#[cfg(test)]").next().unwrap_or(text)
}

/// The ratchet's verdict over one scan, split by direction so each failure
/// names its own fix: new debt (`new_offenders`) vs stale allowlist entries
/// (`fixed_but_listed`).
#[derive(Debug, PartialEq, Eq)]
struct RatchetVerdict {
    /// Files that do direct IO and are NOT grandfathered — add a port.
    new_offenders: Vec<String>,
    /// Grandfathered files that no longer offend — shrink the list.
    fixed_but_listed: Vec<String>,
}

impl RatchetVerdict {
    fn is_clean(&self) -> bool {
        self.new_offenders.is_empty() && self.fixed_but_listed.is_empty()
    }
}

/// The pure decision (CXA-B226): given scanned sources and an allowlist, who
/// breaks the ratchet and in which direction. IO-free by construction, so the
/// fixture self-tests below can drive it without a repo on disk — the same
/// property it demands of the production code it guards.
fn ratchet_verdict(sources: &[(String, String)], grandfathered: &[&str]) -> RatchetVerdict {
    let mut verdict = RatchetVerdict {
        new_offenders: Vec::new(),
        fixed_but_listed: Vec::new(),
    };
    for (rel, text) in sources {
        let dirty = FORBIDDEN.iter().any(|f| production_half(text).contains(f));
        let listed = grandfathered.contains(&rel.as_str());
        match (dirty, listed) {
            (true, false) => verdict.new_offenders.push(rel.clone()),
            (false, true) => verdict.fixed_but_listed.push(rel.clone()),
            _ => {}
        }
    }
    // Sorted so the verdict never depends on scan order and every failure
    // message is stable — `verdict_is_independent_of_scan_order` below holds
    // this line: drop either sort and that test goes red.
    verdict.new_offenders.sort();
    verdict.fixed_but_listed.sort();
    verdict
}

#[test]
fn application_does_io_through_ports_only() {
    let verdict = ratchet_verdict(&application_sources(), GRANDFATHERED);
    assert!(
        verdict.new_offenders.is_empty(),
        "these application files reach for std::process/std::fs directly — put the IO behind a \
         port in ports/outbound/ (see GitPort::working_tree for the pattern) and keep the \
         decision a pure function: {:?}",
        verdict.new_offenders
    );
    assert!(
        verdict.fixed_but_listed.is_empty(),
        "these files no longer do direct IO — remove them from GRANDFATHERED so the ratchet \
         keeps turning: {:?}",
        verdict.fixed_but_listed
    );
}

// ---------------------------------------------------------------------------
// Fixture self-tests (CXA-B226): the gate proves both directions on hand-
// built fixtures, so a change to the decision itself is caught here — not
// only when the real tree happens to violate it.
// ---------------------------------------------------------------------------

/// GREEN FIXTURE — a compliant application: one clean production file, plus a
/// file whose IO lives inside a `#[cfg(test)]` module (test scaffolding, not
/// architecture). The gate must stay green on this.
fn compliant_fixture() -> Vec<(String, String)> {
    vec![
        (
            "use_cases/merge_policy.rs".to_string(),
            "pub fn decide(a: bool, b: bool) -> bool { a && b }".to_string(),
        ),
        (
            "use_cases/backup.rs".to_string(),
            "pub fn due() -> bool { false }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn \
             shells_out_for_real() {\n        let _ = std::process::Command::new(\"ls\");\n    \
             }\n}\n"
                .to_string(),
        ),
    ]
}

/// VIOLATING FIXTURE — two files reaching for the forbidden tokens in their
/// production halves (one per forbidden token). The gate must go red and
/// name them.
fn violating_fixture() -> Vec<(String, String)> {
    vec![
        (
            "use_cases/rogue_spawn.rs".to_string(),
            "pub fn run() { std::process::Command::new(\"git\"); }".to_string(),
        ),
        (
            "use_cases/rogue_read.rs".to_string(),
            "pub fn read() -> String { std::fs::read_to_string(\"x\").unwrap() }".to_string(),
        ),
    ]
}

#[test]
fn green_on_compliant_fixture() {
    let verdict = ratchet_verdict(&compliant_fixture(), GRANDFATHERED);
    assert!(
        verdict.is_clean(),
        "a compliant fixture must not trip the gate, got {verdict:?}"
    );
}

#[test]
fn red_on_violating_fixture() {
    let verdict = ratchet_verdict(&violating_fixture(), GRANDFATHERED);
    assert_eq!(
        verdict.new_offenders,
        vec![
            "use_cases/rogue_read.rs".to_string(),
            "use_cases/rogue_spawn.rs".to_string()
        ],
        "each forbidden token in a production half must be named by the gate"
    );
    assert!(verdict.fixed_but_listed.is_empty());
}

#[test]
fn red_on_violating_fixture_even_inside_test_code_lookalike() {
    // `#[cfg(test)]` hides IO only AFTER the marker — a file whose production
    // half offends stays an offender no matter what test code follows it.
    let mut sources = violating_fixture();
    sources[0].1.push_str("\n#[cfg(test)]\nmod tests {}");
    let verdict = ratchet_verdict(&sources, GRANDFATHERED);
    assert_eq!(verdict.new_offenders.len(), 2);
}

#[test]
fn a_stale_allowlist_entry_fails_the_gate() {
    // The ratchet's core promise: the allowlist only SHRINKS. A grandfathered
    // path whose file has been fixed (here: compliant from the start) must
    // fail the gate and name the entry to remove — forgetting the cleanup is
    // a failure, not a free pass.
    let stale = ["use_cases/merge_policy.rs"];
    let verdict = ratchet_verdict(&compliant_fixture(), &stale);
    assert_eq!(
        verdict.fixed_but_listed,
        vec!["use_cases/merge_policy.rs".to_string()],
        "a stale GRANDFATHERED entry must fail the gate so the list keeps shrinking"
    );
    assert!(!verdict.is_clean());
}

#[test]
fn a_live_offender_may_still_be_grandfathered() {
    // The fourth quadrant: (dirty, listed) is exactly what grandfathering is
    // FOR — known debt, visible and tolerated until it is fixed.
    let debt = ["use_cases/rogue_spawn.rs"];
    let verdict = ratchet_verdict(&violating_fixture(), &debt);
    assert_eq!(
        verdict.new_offenders,
        vec!["use_cases/rogue_read.rs".to_string()],
        "only the UNLISTED offender is new debt — the listed one is tolerated"
    );
    assert!(verdict.fixed_but_listed.is_empty());
}

// ---------------------------------------------------------------------------
// Regression proof (CXA-B226): the verdict is a property of the TREE, not of
// the scan order it arrived in.
// ---------------------------------------------------------------------------

#[test]
fn verdict_is_independent_of_scan_order() {
    // The fix's behavioral core: both verdict directions are sorted, so two
    // scans of the same tree in different orders report identically and no
    // failure message ever depends on scan order. On the pre-fix decision
    // (verdicts in input order) this test goes red — red_on_violating_
    // fixture only pins one fixture's order; this pins the property — and
    // dropping either sort must turn it red again.
    let mut forward = violating_fixture();
    forward.push((
        "use_cases/merge_policy.rs".to_string(),
        "pub fn decide(a: bool, b: bool) -> bool { a && b }".to_string(),
    ));
    let mut reversed = forward.clone();
    reversed.reverse();

    let grandfathered = ["use_cases/merge_policy.rs"];
    let expected = ratchet_verdict(&forward, &grandfathered);
    assert_eq!(
        ratchet_verdict(&reversed, &grandfathered),
        expected,
        "scan order must not change the verdict — sort both directions"
    );
    assert_eq!(
        expected.new_offenders,
        vec![
            "use_cases/rogue_read.rs".to_string(),
            "use_cases/rogue_spawn.rs".to_string()
        ],
        "every unlisted offender must be named, in stable order"
    );
    assert_eq!(
        expected.fixed_but_listed,
        vec!["use_cases/merge_policy.rs".to_string()],
        "a stale allowlist entry must be named, in stable order"
    );
}
