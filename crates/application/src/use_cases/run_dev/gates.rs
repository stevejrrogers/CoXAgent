// Part of the run_dev module split by concern — see run_dev/mod.rs.
#![allow(clippy::wildcard_imports)]
//! The mechanical Definition-of-Done gates: what counts as a test, whose
//! lint a regression is, and how a failed attempt is recorded.

use super::*;

impl<S: StateStorePort, E: AgentEnginePort> RunDevUseCase<S, E> {
    /// Whether any lint location sits in a file this working diff touches.
    /// Paths are compared by suffix so a repo-relative lint path still matches
    /// a git path listed from the same root.
    pub(super) fn lints_touch_changed_files(&self, lint_files: &[String]) -> bool {
        let changed = self.changed_files();
        if changed.is_empty() {
            // Nothing changed on disk — nothing here is attributable.
            return false;
        }
        lint_files.iter().any(|lint| {
            let lint = lint.trim();
            !lint.is_empty()
                && changed
                    .iter()
                    .any(|c| lint.ends_with(c.as_str()) || c.ends_with(lint))
        })
    }
    /// Paths in the working diff: tracked modifications plus untracked files.
    pub(super) fn changed_files(&self) -> Vec<String> {
        let run = |args: &[&str]| -> String {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&self.work_dir)
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default()
        };
        format!(
            "{}\n{}",
            run(&["diff", "HEAD", "--name-only"]),
            run(&["ls-files", "--others", "--exclude-standard"])
        )
        .lines()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect()
    }
    /// Whether the working diff is documentation/assets only — README fixes,
    /// docs, images, licences. Such a "bug fix" has no runtime surface, so
    /// demanding a regression test just parks the ticket.
    pub(super) fn diff_is_docs_only(&self) -> bool {
        let run = |args: &[&str]| -> String {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&self.work_dir)
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default()
        };
        let names = format!(
            "{}\n{}",
            run(&["diff", "HEAD", "--name-only"]),
            run(&["ls-files", "--others", "--exclude-standard"])
        );
        let mut any = false;
        for f in names.lines().map(str::trim).filter(|f| !f.is_empty()) {
            any = true;
            let lower = f.to_lowercase();
            let doc_ext = [
                ".md", ".txt", ".adoc", ".rst", ".png", ".jpg", ".jpeg", ".svg", ".gif",
            ]
            .iter()
            .any(|e| lower.ends_with(e));
            let doc_name = lower.ends_with("license") || lower.ends_with(".gitignore");
            let doc_dir = lower.starts_with("docs/") || lower.contains("/docs/");
            if !(doc_ext || doc_name || doc_dir) {
                return false;
            }
        }
        any
    }
    /// Whether the current working diff (staged/unstaged + untracked) touches
    /// tests: a test-ish path, or added lines containing test markers.
    pub(super) fn diff_touches_tests(&self) -> bool {
        let run = |args: &[&str]| -> String {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&self.work_dir)
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default()
        };
        let names = format!(
            "{}\n{}",
            run(&["diff", "HEAD", "--name-only"]),
            run(&["ls-files", "--others", "--exclude-standard"])
        );
        if names.lines().any(|f| {
            let f = f.trim().to_lowercase();
            !f.is_empty()
                && (f.contains("/tests/")
                    || f.starts_with("tests/")
                    || f.ends_with("_test.rs")
                    || f.ends_with("_test.go")
                    || f.ends_with(".test.ts")
                    || f.ends_with(".test.js")
                    || f.contains("test_"))
        }) {
            return true;
        }
        let diff = run(&["diff", "HEAD"]);
        if diff.lines().any(|l| {
            l.starts_with('+')
                && (l.contains("#[test]")
                    || l.contains("#[tokio::test]")
                    || l.contains("def test_")
                    || l.contains("it(")
                    || l.contains("func Test"))
        }) {
            return true;
        }
        // Rust keeps most tests in an inline `#[cfg(test)] mod tests` at the
        // foot of the file it tests. A fix that hardens or extends one of those
        // adds no `#[test]` line and lives in no test-shaped path, so the two
        // checks above miss the single most common way a Rust regression test
        // actually lands — and the ticket gets failed for shipping without one.
        self.diff_touches_inline_test_module(&run)
    }
    /// Whether any changed line falls inside a file's `#[cfg(test)]` module.
    /// Uses `-U0` so the reported line numbers are the changed lines themselves,
    /// not context that happens to sit near the boundary.
    pub(super) fn diff_touches_inline_test_module(&self, run: &dyn Fn(&[&str]) -> String) -> bool {
        let diff = run(&["diff", "HEAD", "-U0"]);
        let mut file: Option<String> = None;
        for line in diff.lines() {
            if let Some(rest) = line.strip_prefix("+++ b/") {
                file = Some(rest.trim().to_owned());
                continue;
            }
            let Some(hunk) = line.strip_prefix("@@ ") else {
                continue;
            };
            let Some(f) = file.as_deref() else { continue };
            // `@@ -a,b +c,d @@` — c is the first changed line on the new side.
            let Some(new_side) = hunk.split('+').nth(1) else {
                continue;
            };
            let Ok(start) = new_side
                .split([',', ' '])
                .next()
                .unwrap_or("")
                .parse::<usize>()
            else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(self.work_dir.join(f)) else {
                continue;
            };
            if let Some(test_mod_line) = text
                .lines()
                .position(|l| l.trim_start().starts_with("#[cfg(test)]"))
            {
                // Line numbers are 1-based in the diff, 0-based from position().
                if start > test_mod_line {
                    return true;
                }
            }
        }
        false
    }
    /// Count a failed attempt on `id`; at the 3rd, park it with a visible note
    /// so a human decides instead of the team burning tokens forever.
    pub(super) async fn record_failure(&self, id: &TicketId, why: &str) {
        self.record_failure_at(
            id,
            why,
            crate::state::FailureLayer::Design,
            "engine",
            Vec::new(),
        )
        .await;
    }
    /// As [`Self::record_failure`], but the caller names the layer and gate it
    /// rejected the work at, so the next agent reads data instead of guessing
    /// from a sentence.
    pub(super) async fn record_failure_at(
        &self,
        id: &TicketId,
        why: &str,
        layer: crate::state::FailureLayer,
        gate: &str,
        files: Vec<String>,
    ) {
        let key = id.to_string();
        let short: String = why.chars().take(300).collect();
        // Infrastructure faults are NOT the ticket's fault — shared predicate
        // with the runner's circuit breaker (see crate::faults).
        let infra = crate::faults::is_infra_fault(why);
        let _ = layer;
        if infra {
            // Raise it where people look. An outage that only exists as a log
            // line means the team looks broken while the real problem is an
            // expired login nobody was told about.
            let (engine, role, detail) = (
                self.engine.id().to_owned(),
                format!("{:?}", self.mode.role()),
                short.clone(),
            );
            let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                s.log_activity(
                    "SYSTEM",
                    "engine infrastructure fault — attempt not counted",
                    Some(key.clone()),
                );
                let first = !s.engine_incidents.iter().any(|i| i.engine == engine);
                s.open_engine_incident(&engine, &role, &detail);
                if first {
                    let msg = format!(
                        "🔌 {engine} is failing for every agent: {detail}. Work is paused on this \
                         engine until it answers again — fix the credentials or the model, and \
                         this clears itself."
                    );
                    s.post_chat_in("SYSTEM", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
                }
                Ok(())
            })
            .await;
            return;
        }
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            let n = {
                let c = s.ticket_fail_attempts.entry(key.clone()).or_insert(0);
                *c += 1;
                *c
            };
            // Brief the NEXT attempt on what this one hit, so a retry builds
            // on prior findings instead of rediscovering them.
            s.journal_note(&key, &format!("attempt {n} failed: {short}"));
            s.record_attempt_failure(
                &key,
                crate::state::AttemptFailure {
                    attempt: n,
                    layer,
                    gate: gate.to_owned(),
                    detail: short.clone(),
                    files: files.clone(),
                },
            );
            if n == 3 {
                s.post_comment(
                    "DEV-BUG",
                    &format!(
                        "⛔ {id} PARKED after 3 failed attempts (last: {short}) — needs a \
                         human decision; agents will skip it."
                    ),
                    Some(key.clone()),
                );
            }
            Ok(())
        })
        .await;
    }
}
