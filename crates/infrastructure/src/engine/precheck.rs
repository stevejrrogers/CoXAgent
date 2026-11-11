//! Dry-run/precheck for the agent-CLI engines (`opencode`, `claude`, ...) and
//! one shared mapper that turns a failed engine spawn into a diagnostic a human
//! can act on.
//!
//! Ticket SEC-12CF60: three identical sweep runs died on
//! `backend failure: spawn opencode: No such file or directory (os error 2)`
//! because the scanning CLI was not installed where the hub ran. A bare io
//! error names neither the missing executable nor the fix — this module does
//! both, once, so every adapter produces the same actionable message instead of
//! its own copy-paste retry loop over an unexplained crash.

use crate::engine::registry::resolve_binary;

/// Result of checking one agent-CLI binary before a sweep/run starts.
#[must_use]
pub struct BinaryCheck {
    /// Executable name we looked for (the engine's binary, e.g. `opencode`).
    pub binary: &'static str,
    /// Whether an executable file with that name is reachable on `PATH`.
    pub present: bool,
    /// Absolute path found (empty when absent) — proves WHICH binary would run.
    pub path: String,
    /// Exact command that installs it, tailored to this OS/package manager.
    pub install_hint: String,
}

impl std::fmt::Display for BinaryCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.present {
            write!(f, "{}: PASS ({})", self.binary, self.path)
        } else {
            write!(f, "{}: MISSING — fix: {}", self.binary, self.install_hint)
        }
    }
}

/// Verify the scanner binary exists before anything spawns. Call this as the
/// sweep's precheck: it reports PASS or the exact fix, never a guess.
pub fn check_binary(name: &'static str) -> BinaryCheck {
    match resolve_binary(name) {
        Some(path) => BinaryCheck {
            binary: name,
            present: true,
            path: path.display().to_string(),
            install_hint: install_hint_for(name),
        },
        None => BinaryCheck {
            binary: name,
            present: false,
            path: String::new(),
            install_hint: install_hint_for(name),
        },
    }
}

/// The best-effort install command for a known agent CLI on this host.
fn install_hint_for(name: &str) -> String {
    let mac = cfg!(target_os = "macos");
    match name {
        "opencode" => {
            if mac {
                "brew install sst/tap/opencode".to_owned()
            } else {
                "curl -fsSL https://opencode.ai/install | bash   # or npm i -g opencode-ai"
                    .to_owned()
            }
        }
        "claude" => "npm i -g @anthropic-ai/claude-code".to_owned(),
        "copilot" => "npm i -g @github/copilot".to_owned(),
        "hermes" => "pip install hermes-cli   # or see the hermes docs".to_owned(),
        _ => format!("install `{name}` and make sure it is on the hub's PATH"),
    }
}

/// Map a failed spawn to a `PortError` payload that NAMES the problem.
///
/// On a genuine ENOENT ("No such file or directory", os error 2) the message
/// names the missing executable and how to install it — instead of a bare
/// `No such file or directory (os error 2)` that three retries cannot fix.
/// Every other IO condition keeps the OS detail untouched. Retries still work;
/// they just carry the cause along.
/// Marker wrapper so the engine adapters pass a compile-time-checked tool name.
#[derive(Clone, Copy)]
pub struct SpawnTool(pub &'static str);

#[must_use]
pub fn spawn_error(tool: SpawnTool, err: &std::io::Error) -> String {
    let name = tool.0;
    if err.kind() == std::io::ErrorKind::NotFound {
        format!(
            "spawn {name}: executable `{name}` not found on PATH — {fix}",
            name = name,
            fix = install_hint_for(name)
        )
    } else {
        format!("spawn {name}: {err}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_spawn_names_the_executable_and_the_fix() {
        // The exact failure SEC-12CF60 shipped with: os error 2 on spawn.
        let enoent = std::io::Error::from(std::io::ErrorKind::NotFound);
        let msg = spawn_error(SpawnTool("opencode"), &enoent);
        assert!(
            msg.contains("`opencode` not found on PATH"),
            "must name the missing executable: {msg}"
        );
        assert!(
            msg.contains("install") || msg.contains("Install") || msg.contains("curl"),
            "must say how to install it: {msg}"
        );
    }

    #[test]
    fn non_enoent_spawns_keep_the_os_detail() {
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let msg = spawn_error(SpawnTool("claude"), &denied);
        assert!(msg.starts_with("spawn claude:"), "{msg}");
        assert!(
            !msg.contains("not found on PATH"),
            "permission errors are real OS facts, not a missing install: {msg}"
        );
    }

    #[test]
    fn precheck_reports_pass_or_exact_fix_never_a_guess() {
        let c = check_binary("no-such-cox-agent-tool-xyz");
        assert!(!c.present);
        assert!(c.path.is_empty());
        let report = c.to_string();
        assert!(
            report.starts_with("no-such-cox-agent-tool-xyz: MISSING — fix: "),
            "{report}"
        );

        // A binary guaranteed to exist on any dev machine the tests run on.
        let probe = if cfg!(windows) { "cmd" } else { "sh" };
        let c = check_binary(probe);
        assert!(c.present, "`{probe}` should be on PATH everywhere");
        assert!(c.report_starts_with_pass());
    }

    impl BinaryCheck {
        fn report_starts_with_pass(&self) -> bool {
            self.to_string()
                .starts_with(&format!("{}: PASS (", self.binary))
        }
    }
}
