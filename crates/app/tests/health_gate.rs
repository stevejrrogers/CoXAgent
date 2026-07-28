//! Repo-wide guard that the post-deploy health gate cannot be SKIPPED — the
//! COX-B009 regression test.
//!
//! COX-B004 added the gate to the autonomous cycle only. Two other call sites —
//! chat's "deploy" command and the PR-preview endpoint — kept reporting success
//! from the compose exit code alone, so the bug class COX-B004 closed stayed
//! fully reproducible through them. Every one of those call sites now runs
//! through [`verify_deploy_health`], and each has its own unit test.
//!
//! Those unit tests cannot catch the failure that produced this ticket, though:
//! the defect was never a wrong call site, it was a MISSING one. A sixth
//! `deploy.deploy(...)` added tomorrow — a new endpoint, a new chat command, a
//! rescue path — that reports `r.success` straight to the human breaks nothing
//! any existing test asserts on, and the compiler has nothing to say about it
//! either. The team decision behind COX-B004 asked for a health check that
//! "can't be skipped"; a gate that only guards the call sites someone remembered
//! to wire is one that can.
//!
//! So this asserts on the production source, the same way `platform_gates.rs`
//! and `sandbox_confinement_gate.rs` do for their own invariants: the gate
//! exists in this build, and every `deploy.deploy(...)` call site reaches it —
//! directly, or through a helper that does.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The gate mechanism itself. `verify_deploy_health` is the shared entry point;
/// `wait_healthy` is the polling probe it is built on, which the cycle's richer
/// COX-F005 check drives directly to keep the probe detail it records. A call
/// site that reaches either one has been gated.
const GATE: [&str; 2] = ["verify_deploy_health", "wait_healthy"];

/// How a deploy is requested. Every call site in the workspace names the port
/// binding `deploy`, so this matches the call and not the `self.deploy` field
/// or the `deploy` module path.
const DEPLOY_CALL: &str = "deploy.deploy(";

/// A function chunk: its name and its source text.
///
/// Chunks are split on `fn` headers rather than brace-matched. Braces are not
/// reliable here — `server.rs` is full of `format!`/`json!` literals whose
/// braces do not nest with the code's — and an over-long chunk can only make
/// the scan more permissive at its tail, never invent a violation.
struct Func {
    name: String,
    line: usize,
    body: String,
}

/// Production source only, with every `#[cfg(test)]` item cut out. Test doubles
/// deploy without a gate on purpose — that is what they are for.
///
/// Each `#[cfg(test)]` block is cut to its closing brace at the attribute's own
/// indentation rather than by truncating the rest of the file: production code
/// does sometimes follow a test module, and silently dropping it would leave
/// call sites the guard never sees — the exact blind spot this file exists to
/// remove. Indentation, not brace counting, because the test modules are full
/// of JSON fixtures whose braces do not nest with the code's.
fn production_source(src: &str) -> String {
    let mut out = Vec::new();
    let mut skip_until: Option<String> = None;
    for line in src.lines() {
        if let Some(closer) = &skip_until {
            if line == closer {
                skip_until = None;
            }
            continue;
        }
        if line.trim_start().starts_with("#[cfg(test)]") {
            let indent = &line[..line.len() - line.trim_start().len()];
            skip_until = Some(format!("{indent}}}"));
            continue;
        }
        out.push(line);
    }
    out.join("\n")
}

/// Whether `line` opens a function definition.
fn is_fn_header(line: &str) -> bool {
    let t = line.trim_start();
    let t = t
        .strip_prefix("pub(crate) ")
        .or_else(|| t.strip_prefix("pub "))
        .unwrap_or(t);
    let t = t.strip_prefix("async ").unwrap_or(t);
    t.starts_with("fn ")
}

/// Doc comments and prose name `deploy.deploy(...)` and the gate freely — this
/// file's own header does. Only code counts.
fn code_only(body: &str) -> String {
    body.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every function in `src`, each running to the next function header.
fn functions(src: &str) -> Vec<Func> {
    let lines: Vec<&str> = src.lines().collect();
    let starts: Vec<usize> = (0..lines.len())
        .filter(|&i| is_fn_header(lines[i]))
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(n, &i)| {
            let end = starts.get(n + 1).copied().unwrap_or(lines.len());
            Func {
                name: lines[i]
                    .trim_start()
                    .split("fn ")
                    .nth(1)
                    .and_then(|rest| rest.split(['(', '<']).next())
                    .unwrap_or_default()
                    .to_owned(),
                line: i + 1,
                body: code_only(&lines[i..end].join("\n")),
            }
        })
        .collect()
}

/// Names that reach the gate: [`GATE`] plus, transitively, every function whose
/// body calls one of them.
///
/// The closure is what lets a call site delegate. The cycle probes through
/// `verify_health_after_deploy`/`run_health_check` and the preview through
/// `preview_deploy_failure`; all three are legitimate, and hard-coding their
/// names here would just be the same skip-list the ticket is about.
fn gated_names(funcs: &[&Func]) -> BTreeSet<String> {
    let mut gated: BTreeSet<String> = GATE.iter().map(|&s| s.to_owned()).collect();
    loop {
        let mut grew = false;
        for f in funcs {
            if gated.contains(&f.name) {
                continue;
            }
            if gated.iter().any(|g| f.body.contains(&format!("{g}("))) {
                gated.insert(f.name.clone());
                grew = true;
            }
        }
        if !grew {
            return gated;
        }
    }
}

/// The deploy call sites in `sources` and the ungated ones among them.
///
/// The whole workspace is scanned at once: the gate lives in `application` and
/// its call sites in `application` and `presentation`, so a per-file scan could
/// not resolve delegation across that boundary.
fn scan(sources: &[(String, String)]) -> (Vec<String>, Vec<String>) {
    let per_file: Vec<(&str, Vec<Func>)> = sources
        .iter()
        .map(|(path, src)| (path.as_str(), functions(&production_source(src))))
        .collect();
    let all: Vec<&Func> = per_file.iter().flat_map(|(_, fs)| fs.iter()).collect();
    let gated = gated_names(&all);

    let (mut sites, mut violations) = (Vec::new(), Vec::new());
    for (path, funcs) in &per_file {
        for f in funcs {
            let calls = f.body.matches(DEPLOY_CALL).count();
            if calls == 0 {
                continue;
            }
            // One entry per call, not per function: `pr_preview` deploys twice
            // (start and restore), and the count is what proves the scan is
            // still matching everything it used to.
            sites.extend((0..calls).map(|_| format!("{path}:{} ({})", f.line, f.name)));
            if !gated.iter().any(|g| f.body.contains(&format!("{g}("))) {
                violations.push(format!(
                    "{path}:{} `fn {}` deploys but never reaches the health gate — a \
                     `docker compose up` exit 0 only proves the containers started, not \
                     that the app inside bound its port (COX-B004/COX-B009). Report \
                     success only once `verify_deploy_health` has passed.",
                    f.line, f.name
                ));
            }
        }
    }
    (sites, violations)
}

/// Every production (`src/`) Rust file under the workspace's `crates/`.
fn production_sources() -> Vec<(String, String)> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut paths = Vec::new();
    let mut stack = vec![crates];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path: PathBuf = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs")
                && path.components().any(|c| c.as_os_str() == "src")
            {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths
        .into_iter()
        .filter_map(|p| {
            std::fs::read_to_string(&p)
                .ok()
                .map(|src| (p.display().to_string(), src))
        })
        .collect()
}

/// Failure mode 1: the shared gate is missing from the build (lost in a merge
/// or forward-port), so every call site's "gate" resolves to nothing.
#[test]
fn the_shared_deploy_health_gate_is_present_in_this_build() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../application/src/ports/outbound/deploy.rs");
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    assert!(
        src.contains("pub async fn verify_deploy_health"),
        "crates/application/src/ports/outbound/deploy.rs no longer defines \
         `verify_deploy_health` — the COX-B009 gate is not in this build, and every \
         deploy path is back to trusting the compose exit code."
    );
    assert!(
        src.contains("async fn wait_healthy"),
        "the gate's polling probe `wait_healthy` is gone — a single immediate probe \
         would false-fail healthy deploys (COX-B018)."
    );
}

/// Failure mode 2, the one this ticket is: a call site that never reaches it.
#[test]
fn every_deploy_call_site_runs_through_the_health_gate() {
    let (sites, violations) = scan(&production_sources());
    assert!(violations.is_empty(), "{}", violations.join("\n"));

    // The known call sites: the cycle's deploy and its rollback redeploy, chat's
    // "deploy" command, and the PR preview's start and restore. Fewer means the
    // scan stopped matching and is guarding nothing.
    assert!(
        sites.len() >= 5,
        "the deploy-gate scan found only {} call sites ({sites:?}) — the guard itself \
         is broken",
        sites.len()
    );
}

#[test]
fn scan_flags_a_call_site_that_trusts_the_compose_exit_code() {
    let raw = "\
    async fn deploy_now(&self) {
        let result = deploy.deploy(&self.work_dir).await;
        match result {
            Ok(r) if r.success => self.post(\"DEV\", \"Deploy OK\").await,
            _ => self.post(\"DEV\", \"Deploy failed\").await,
        }
    }
";
    let sources = vec![("chat.rs".to_owned(), raw.to_owned())];
    let (sites, violations) = scan(&sources);
    assert_eq!(sites.len(), 1, "{sites:?}");
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(violations[0].contains("never reaches the health gate"));

    let gated = raw.replace(
        "Ok(r) if r.success =>",
        "Ok(r) if r.success && verify_deploy_health(deploy, self.host_port).await =>",
    );
    assert!(
        scan(&[("chat.rs".to_owned(), gated)]).1.is_empty(),
        "probing the port before reporting success is the fix, and must pass"
    );
}

/// Delegation is legitimate — but only when the helper actually probes. A chain
/// that ends somewhere else is the same skip with an extra hop.
#[test]
fn scan_flags_a_delegate_that_never_probes() {
    let dropped = "\
    async fn pr_preview(deploy: &Arc<dyn DeployPort>) -> Response {
        match deploy.deploy(&prev_dir).await {
            Ok(r) => preview_failure(&r),
            Err(e) => internal_error(&e),
        }
    }
    fn preview_failure(report: &DeployReport) -> Option<String> {
        (!report.success).then(|| report.summary.clone())
    }
";
    let violations = scan(&[("server.rs".to_owned(), dropped.to_owned())]).1;
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(violations[0].contains("pr_preview"));

    let probing = dropped.replace(
        "(!report.success).then(|| report.summary.clone())",
        "(!report.success || !verify_deploy_health(deploy, port).await)\
         .then(|| report.summary.clone())",
    );
    assert!(
        scan(&[("server.rs".to_owned(), probing)]).1.is_empty(),
        "a helper that reaches the gate satisfies its callers"
    );
}

/// The scan reads production code, not prose: a doc comment that merely
/// mentions the gate must not launder an ungated call site — the failure mode
/// COX-B006 hit when a `cfg` gate was matched by a comment.
#[test]
fn a_comment_naming_the_gate_does_not_satisfy_it() {
    let commented = "\
    async fn deploy_now(&self) {
        // Mandatory health gate: verify_deploy_health(deploy, self.host_port).
        let r = deploy.deploy(&self.work_dir).await;
        self.post(\"DEV\", \"Deploy OK\").await;
    }
";
    let violations = scan(&[("chat.rs".to_owned(), commented.to_owned())]).1;
    assert_eq!(
        violations.len(),
        1,
        "a comment is not a probe: {violations:?}"
    );
}

/// Test doubles deploy without a gate by design; the scan must stay out of
/// `#[cfg(test)]` or it would flag every fixture in the workspace.
#[test]
fn deploys_inside_test_modules_are_not_call_sites() {
    let with_tests = "\
    async fn gated(&self) {
        let r = deploy.deploy(&self.work_dir).await;
        let _ = verify_deploy_health(deploy, self.host_port).await;
    }
    #[cfg(test)]
    mod tests {
        async fn fixture() {
            let r = deploy.deploy(&dir).await;
        }
    }
";
    let (sites, violations) = scan(&[("uc.rs".to_owned(), with_tests.to_owned())]);
    assert_eq!(
        sites.len(),
        1,
        "only the production call site counts: {sites:?}"
    );
    assert!(violations.is_empty(), "{violations:?}");
}

/// ...but skipping a test module must not mean skipping the REST of the file.
/// Cutting from the first `#[cfg(test)]` to end-of-file is the cheap way to do
/// it, and it makes the guard blind to anything declared afterwards — a hole
/// big enough to hide the very call site this file exists to catch.
#[test]
fn a_call_site_after_a_test_module_is_still_scanned() {
    let trailing = "\
    async fn gated(&self) {
        let r = deploy.deploy(&self.work_dir).await;
        let _ = verify_deploy_health(deploy, self.host_port).await;
    }
    #[cfg(test)]
    mod tests {
        async fn fixture() {
            let r = deploy.deploy(&dir).await;
        }
    }
    async fn added_later(&self) -> bool {
        deploy.deploy(&self.work_dir).await.map(|r| r.success).unwrap_or(false)
    }
";
    let (sites, violations) = scan(&[("uc.rs".to_owned(), trailing.to_owned())]);
    assert_eq!(sites.len(), 2, "{sites:?}");
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(violations[0].contains("added_later"));
}
