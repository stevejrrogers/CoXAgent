//! Repo-wide guard that the Seatbelt retry is actually WIRED — the COX-B022
//! regression test.
//!
//! The bug COX-B022 reports is not a coding error, it's a delivery one: the
//! COX-B013 fix (retry `sandbox-exec` when its own `sandbox_apply()` fails, so
//! a legitimate in-workspace write is not silently denied) existed only on an
//! unmerged branch. `crates/infrastructure/src/proc.rs` on the shipping branch
//! had none of it, every test still passed, and the release shipped the defect.
//!
//! Two failure modes, both invisible to the unit tests in `proc.rs`:
//!   1. The retry helpers themselves vanish in a merge/forward-port.
//!   2. The helpers survive, but a call site goes back to spawning the confined
//!      command raw (`cmd.spawn()` / `cmd.output()`), or a NEW engine is added
//!      that never routes through them. `proc.rs`'s own tests keep passing —
//!      they exercise the helpers, not the callers.
//!
//! So this asserts on the production source: the policy exists, and every
//! `agent_command` caller hands its command to `spawn_confined`/
//! `output_confined` (or passes the `SandboxStatus` on to something that does).
//! Source-level like `platform_gates.rs`, and for the same reason: the defect
//! is a shape the compiler and the runtime tests are both blind to.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

/// The mechanism's own definitions — they implement the raw spawn the rest of
/// the codebase is forbidden from doing, so they are not call sites.
const MECHANISM: [&str; 4] = [
    "agent_command",
    "spawn_confined",
    "spawn_confined_within",
    "output_confined",
];

/// A top-level or `impl`-level function: its name and its full text (signature
/// through closing brace).
struct Func {
    name: String,
    line: usize,
    body: String,
}

/// Production source only, with any trailing `#[cfg(test)]` module cut off:
/// unit tests legitimately spawn commands raw, and fixtures inside them are
/// prose, not call sites.
fn production_source(src: &str) -> String {
    match src.lines().position(|l| l.starts_with("#[cfg(test)]")) {
        Some(at) => src.lines().take(at).collect::<Vec<_>>().join("\n"),
        None => src.to_owned(),
    }
}

/// Every function in `src`, brace-matched so multi-line signatures and nested
/// blocks stay with their owner.
fn functions(src: &str) -> Vec<Func> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        let is_fn = trimmed.starts_with("fn ")
            || trimmed.starts_with("async fn ")
            || trimmed.starts_with("pub fn ")
            || trimmed.starts_with("pub async fn ")
            || trimmed.starts_with("pub(crate) fn ")
            || trimmed.starts_with("pub(crate) async fn ");
        if !is_fn {
            i += 1;
            continue;
        }
        let name = trimmed
            .split("fn ")
            .nth(1)
            .and_then(|rest| rest.split(['(', '<']).next())
            .unwrap_or_default()
            .to_owned();
        let (mut depth, mut opened, mut end) = (0i32, false, i);
        for (j, line) in lines.iter().enumerate().skip(i) {
            depth += i32::try_from(line.matches('{').count()).unwrap();
            depth -= i32::try_from(line.matches('}').count()).unwrap();
            if depth > 0 {
                opened = true;
            }
            end = j;
            if opened && depth <= 0 {
                break;
            }
        }
        out.push(Func {
            name,
            line: i + 1,
            body: lines[i..=end].join("\n"),
        });
        i = end + 1;
    }
    out
}

/// Whether `body` starts a child process at all.
fn spawns_a_process(body: &str) -> bool {
    body.contains(".spawn()") || body.contains(".output()") || body.contains("_confined(")
}

/// Whether `body` routes through the COX-B013 retry wrappers.
fn is_confined(body: &str) -> bool {
    body.contains("spawn_confined(") || body.contains("output_confined(")
}

/// Whether `body` hands its `SandboxStatus` to another call — the legitimate
/// alternative to spawning here (e.g. `run` building the command and passing
/// `sandbox` down to a shared `exec` that does the confined spawn).
fn delegates_sandbox(body: &str) -> bool {
    body.lines().any(|l| {
        let t = l.trim();
        // The binding line is where the status comes FROM, and a signature is
        // not a call — neither counts as passing it on. Match `sandbox` as a
        // whole token so `_sandbox` (deliberately discarded) does not.
        t.contains('(')
            && !t.contains("agent_command(")
            && !t.contains(": SandboxStatus")
            && t.split(|c: char| !c.is_alphanumeric() && c != '_')
                .any(|tok| tok == "sandbox")
    })
}

/// Violations in one production file, plus the `agent_command` call sites seen
/// (so the scan can prove it is not silently matching nothing).
fn scan(src: &str) -> (Vec<String>, Vec<String>) {
    let (mut sites, mut violations) = (Vec::new(), Vec::new());
    for f in functions(&production_source(src)) {
        if MECHANISM.contains(&f.name.as_str()) {
            continue;
        }
        let builds = f.body.contains("agent_command(");
        let handles = f.body.contains("sandbox: SandboxStatus");
        if !builds && !handles {
            continue;
        }
        if builds {
            sites.push(format!("{}:{}", f.name, f.line));
        }
        if spawns_a_process(&f.body) {
            if !is_confined(&f.body) {
                violations.push(format!(
                    "`fn {}` (line {}) spawns a write-confined command directly \
                     instead of via proc::spawn_confined/output_confined: macOS \
                     Seatbelt's sandbox_apply() fails transiently (COX-B013), so \
                     without the retry an in-workspace write is silently denied",
                    f.name, f.line
                ));
            }
        } else if builds && !delegates_sandbox(&f.body) {
            violations.push(format!(
                "`fn {}` (line {}) builds a command via proc::agent_command but \
                 neither spawns it confined nor passes the SandboxStatus on — the \
                 COX-B013 retry cannot apply to it",
                f.name, f.line
            ));
        }
    }
    (sites, violations)
}

/// Every production (`src/`) Rust file under the workspace's `crates/`.
fn production_sources() -> Vec<PathBuf> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut out = Vec::new();
    let mut stack = vec![crates];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs")
                && path.components().any(|c| c.as_os_str() == "src")
            {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// The process module and its `confined` submodule as one text: the retry
/// policy lives in `proc/confined.rs`, but which file holds it is an internal
/// detail — this guard is about whether the policy is IN THE BUILD at all, so
/// it must not fail (or pass) merely because the code moved.
fn proc_source() -> String {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../infrastructure/src");
    ["proc.rs", "proc/confined.rs"]
        .iter()
        .map(|rel| {
            let path = dir.join(rel);
            std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// COX-B022 failure mode 1: the fix is missing from the branch being built.
#[test]
fn the_seatbelt_retry_policy_is_present_in_this_build() {
    let src = proc_source();
    for needle in [
        "fn is_transient_seatbelt_apply_failure",
        "pub async fn output_confined",
        "pub async fn spawn_confined",
        "SEATBELT_APPLY_RETRIES",
    ] {
        assert!(
            src.contains(needle),
            "crates/infrastructure/src/proc.rs is missing `{needle}` — the COX-B013 \
             fix is not in this build (it was lost in a merge/forward-port, exactly \
             what COX-B022 reports). Any release cut from here denies in-workspace \
             writes on ~1 in 3 macOS runs."
        );
    }
    // The signature the retry keys on: sandbox-exec's own EX_OSERR, not the
    // target program's status. Widening it would retry real failures.
    assert!(
        src.contains("code == Some(71)"),
        "the retry must key on sandbox-exec's own EX_OSERR (71)"
    );
}

/// COX-B022 failure mode 2: the helpers survive but a caller bypasses them.
#[test]
fn every_confined_command_is_spawned_through_the_retry() {
    let (mut sites, mut violations) = (Vec::new(), Vec::new());
    for path in production_sources() {
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let (file_sites, file_violations) = scan(&src);
        sites.extend(
            file_sites
                .into_iter()
                .map(|s| format!("{}:{s}", path.display())),
        );
        violations.extend(
            file_violations
                .into_iter()
                .map(|v| format!("{}: {v}", path.display())),
        );
    }
    assert!(violations.is_empty(), "{}", violations.join("\n"));

    // The real call sites are claude (run + resume_run), opencode (run +
    // resume_run) and hermes (run). Fewer means the scan drifted into a no-op.
    assert!(
        sites.len() >= 5,
        "confinement scan found only {} agent_command call sites ({sites:?}) — \
         the guard itself is broken",
        sites.len()
    );
}

#[test]
fn scan_flags_a_caller_that_bypasses_the_retry() {
    let raw = "\
    async fn run(&self, request: AgentRequest) -> Result<Outcome, PortError> {
        let (mut cmd, sandbox) = crate::proc::agent_command(&self.binary, &request.work_dir, true);
        let out = cmd.output().await?;
        Ok(parse(out))
    }
";
    let (sites, violations) = scan(raw);
    assert_eq!(sites.len(), 1, "{sites:?}");
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(violations[0].contains("spawns a write-confined command directly"));

    let wired = raw.replace(
        "cmd.output().await?",
        "crate::proc::output_confined(&mut cmd, sandbox).await?",
    );
    assert!(
        scan(&wired).1.is_empty(),
        "routing through the retry wrapper is the fix, and must pass"
    );
}

#[test]
fn scan_flags_a_caller_that_drops_the_sandbox_status() {
    let dropped = "\
    async fn run(&self, request: AgentRequest) -> Result<Outcome, PortError> {
        let (cmd, _sandbox) = crate::proc::agent_command(&self.binary, &request.work_dir, true);
        self.exec(cmd, request.timeout).await
    }
";
    let violations = scan(dropped).1;
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(violations[0].contains("passes the SandboxStatus on"));

    let passed = dropped.replace("self.exec(cmd, request.timeout)", "self.exec(cmd, sandbox)");
    assert!(
        scan(&passed).1.is_empty(),
        "delegating the status to a helper that spawns confined is legitimate"
    );
}
