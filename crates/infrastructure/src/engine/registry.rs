//! Engine discovery — scan `PATH` for known agent CLIs so the app can report a
//! machine's capabilities and the settings UI can map roles to installed engines.

use coxagent_application::config::EngineKind;
use std::path::{Path, PathBuf};

/// A discovered engine binary on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedEngine {
    pub kind: EngineKind,
    pub path: PathBuf,
}

/// Discover known engines on the current process `PATH`, plus the well-known
/// install homes `PATH` tends to omit.
///
/// The extra dirs are the SAME list `resolve_engine_binary` falls back to when
/// it spawns an engine — and they must stay the same list. When only the
/// spawn side had them, an app-launched hub (GUI `PATH`, no `~/.opencode/bin`)
/// could RUN opencode fine while the settings page said it was not installed.
#[must_use]
pub fn discover() -> Vec<DetectedEngine> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    dirs.extend(fallback_dirs());
    discover_in(&dirs)
}

/// Resolve one agent CLI to an absolute path, searching `PATH` and then the
/// fallback install homes. The ONE resolver: discovery, the models probe and
/// spawn-time resolution must all agree on where a binary is, or the UI and
/// the runner tell the user different stories about the same machine.
#[must_use]
pub fn resolve_binary(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    dirs.extend(fallback_dirs());
    find_binary(&dirs, name)
}

/// Install locations agent CLIs use that a GUI-inherited `PATH` omits.
fn fallback_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    vec![
        home.join(".local/bin"),
        home.join(".claude/bin"),
        home.join(".opencode/bin"),
        home.join(".hermes/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ]
}

/// The `provider/model` pairs this machine's `opencode` can reach, from
/// `opencode models`. Empty when the CLI is absent or errors.
///
/// Only the CLI knows these: a user's custom providers live in their own
/// opencode config, so the built-in provider list in the settings UI can never
/// include them, and a hub in a container has no opencode to ask. Reported
/// through the worker registry with everything else this machine can do.
#[must_use]
pub fn discover_opencode_models() -> Vec<String> {
    let Some(bin) = resolve_binary("opencode") else {
        return Vec::new();
    };
    let Ok(out) = std::process::Command::new(bin).arg("models").output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        // Every real entry is `provider/model`; anything else is chatter
        // ("No models configured", an error banner) and must not become a
        // provider in someone's dropdown.
        .filter(|l| l.contains('/') && !l.contains(' '))
        .map(ToOwned::to_owned)
        .collect()
}

/// Discover known engines by scanning the given directories. Pure over its
/// inputs, so tests can point it at a temp dir with fake executables.
#[must_use]
pub fn discover_in(dirs: &[PathBuf]) -> Vec<DetectedEngine> {
    let mut found = Vec::new();
    for &kind in EngineKind::all() {
        if let Some(path) = find_binary(dirs, kind.as_binary()) {
            found.push(DetectedEngine { kind, path });
        }
    }
    found
}

/// A developer tool the git/deploy flow may need, and whether it is installed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DetectedTool {
    /// Binary name (`git`, `gh`, `glab`, `docker`).
    pub name: &'static str,
    /// What CoXAgent uses it for.
    pub purpose: &'static str,
    /// Whether it was found on `PATH`.
    pub present: bool,
    /// Full path when present.
    pub path: String,
    /// Shell command / link to install it, chosen for this OS and whether
    /// Homebrew is available.
    pub install: String,
    /// Whether it needs an interactive auth step only the user can do.
    pub needs_auth: bool,
}

/// The developer-tooling picture: the host OS, whether Homebrew is present, and
/// each tool's status. `os` + `has_brew` let the dashboard offer a package
/// manager that actually exists on the machine (and prompt to install Homebrew
/// when a Mac lacks it).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Tooling {
    /// `"macos"`, `"linux"`, `"windows"`, …
    pub os: &'static str,
    pub has_brew: bool,
    /// Command to install Homebrew (only meaningful on macOS without it).
    pub brew_install: &'static str,
    pub tools: Vec<DetectedTool>,
}

const BREW_INSTALL: &str =
    "/bin/bash -c \"$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\"";

/// The tools the git flow and deploy step rely on: `(name, purpose, needs_auth)`.
const TOOLING: &[(&str, &str, bool)] = &[
    ("git", "version control (branch/commit)", false),
    ("gh", "GitHub PRs & auth", true),
    ("glab", "GitLab MRs & auth", true),
    ("docker", "deploy (docker compose)", false),
];

/// The best install command for `tool` given the OS and whether brew exists —
/// never suggests a package manager the machine doesn't have.
fn install_for(tool: &str, os: &str, brew: bool) -> String {
    let mac_brew = brew && os == "macos";
    match tool {
        "git" => match os {
            "macos" => "xcode-select --install",
            "linux" => "sudo apt-get install -y git",
            _ => "https://git-scm.com/downloads",
        }
        .to_owned(),
        "gh" => {
            if mac_brew {
                "brew install gh"
            } else if os == "linux" {
                "sudo apt install -y gh   # or see cli.github.com/manual/installation"
            } else {
                "download from https://cli.github.com"
            }
        }
        .to_owned(),
        "glab" => {
            if mac_brew {
                "brew install glab"
            } else if os == "linux" {
                "curl -sL https://gitlab.com/gitlab-org/cli/-/raw/main/scripts/install.sh | sudo sh"
            } else {
                "download from https://gitlab.com/gitlab-org/cli/-/releases"
            }
        }
        .to_owned(),
        "docker" => match os {
            "linux" => "curl -fsSL https://get.docker.com | sh",
            "macos" => "Docker Desktop: https://docs.docker.com/desktop/setup/install/mac-install/",
            _ => "https://docs.docker.com/get-docker/",
        }
        .to_owned(),
        _ => String::new(),
    }
}

/// Detect the developer tooling on `PATH`, so the dashboard can show what's
/// missing and how to install it on this machine.
#[must_use]
pub fn discover_tooling() -> Tooling {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    // Same GUI-PATH blindness as engines: an app-spawned hub missed gh/docker
    // in /opt/homebrew/bin and told a Mac user to install what they had.
    dirs.extend(fallback_dirs());
    let os = std::env::consts::OS;
    let has_brew = find_binary(&dirs, "brew").is_some();
    let tools = TOOLING
        .iter()
        .map(|&(name, purpose, needs_auth)| {
            let found = find_binary(&dirs, name);
            DetectedTool {
                name,
                purpose,
                present: found.is_some(),
                path: found.map(|p| p.display().to_string()).unwrap_or_default(),
                install: install_for(name, os, has_brew),
                needs_auth,
            }
        })
        .collect();
    let mut tools: Vec<DetectedTool> = tools;
    if os == "linux" {
        let found = find_binary(&dirs, "bwrap");
        tools.push(DetectedTool {
            name: "bwrap",
            purpose: "confines sandboxed agent writes to the workspace (workflow.sandbox)",
            present: found.is_some(),
            path: found.map(|p| p.display().to_string()).unwrap_or_default(),
            install:
                "sudo apt-get install -y bubblewrap   # or see github.com/containers/bubblewrap"
                    .to_owned(),
            needs_auth: false,
        });
    }
    Tooling {
        os,
        has_brew,
        brew_install: BREW_INSTALL,
        tools,
    }
}

/// Return the first directory containing an executable named `name`.
fn find_binary(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    dirs.iter().find_map(|dir| {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            Some(candidate)
        } else {
            None
        }
    })
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC-adjacent: `workflow.sandbox` on Linux needs `bwrap` — the dashboard
    /// should be able to tell the operator whether it's installed, same as it
    /// already does for git/gh/glab/docker.
    #[test]
    fn discover_tooling_lists_bwrap_on_linux() {
        let t = discover_tooling();
        let has_bwrap_entry = t.tools.iter().any(|d| d.name == "bwrap");
        assert_eq!(
            has_bwrap_entry,
            t.os == "linux",
            "bwrap should be listed on Linux only (os={})",
            t.os
        );
    }
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}
