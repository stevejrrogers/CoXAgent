//! CXA-F420b — deterministic repo scanner behind the Add-repo wizard.
//!
//! The adapter takes ONE snapshot of a candidate repo (relative file paths
//! plus the contents of a few known manifests); the proposal is a pure
//! function of that snapshot, testable with a struct literal (the same
//! snapshot-then-decide shape as `GitPort::working_tree` → `run_dev/gates`).
//! AI never types here: the scanner proposes, the human edits and approves in
//! the wizard, and the probe (build+test on a clean tree) is what earns
//! `active` — never this guess.

use serde::{Deserialize, Serialize};

/// Everything the scanner is allowed to know about a candidate repo: the
/// snapshot the adapter took. Paths are relative to the repo root, `/`-
/// separated, directories excluded.
#[derive(Debug, Default, Clone)]
pub struct RepoSnapshot {
    /// Directory basename of the clone — the default service name seed.
    pub root_name: String,
    /// Relative file paths (capped by the adapter; a partial walk still
    /// yields a useful proposal).
    pub files: Vec<String>,
    /// Contents of root-level manifests the adapter read (small, capped):
    /// keyed by the exact relative path, e.g. `Cargo.toml`, `go.work`,
    /// `pnpm-workspace.yaml`.
    pub manifests: Vec<(String, String)>,
}

impl RepoSnapshot {
    fn has(&self, path: &str) -> bool {
        self.files.iter().any(|f| f == path)
    }
    fn manifest(&self, path: &str) -> Option<&str> {
        self.manifests
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, c)| c.as_str())
    }
}

/// What the wizard renders as one editable "Proposed service" card.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProposedService {
    pub name: String,
    pub build_cmd: String,
    pub test_cmd: String,
    /// Monorepo folders (workspace members) — `scopes` on the service (R4).
    pub scopes: Vec<String>,
    /// Detected toolchains, most confident first — context for the card.
    pub languages: Vec<String>,
    /// A root Dockerfile exists — the card offers a deploy gate.
    pub has_dockerfile: bool,
    /// Human-readable caveats ("no manifest recognized", …).
    pub notes: Vec<String>,
}

/// Propose service settings from one snapshot. Deterministic-first (spec §3.2):
/// recognized manifests beat guesses, several toolchains all get named, and an
/// unrecognized tree yields empty commands plus a note — never an invented one.
#[must_use]
pub fn propose(snap: &RepoSnapshot) -> ProposedService {
    let mut p = ProposedService {
        name: sanitize_name(&snap.root_name),
        has_dockerfile: snap.has("Dockerfile"),
        ..ProposedService::default()
    };
    if snap.has("Cargo.toml") {
        p.languages.push("rust".to_owned());
        "cargo build --workspace".clone_into(&mut p.build_cmd);
        "cargo test --workspace".clone_into(&mut p.test_cmd);
        p.scopes = cargo_workspace_members(snap.manifest("Cargo.toml").unwrap_or_default());
    }
    if snap.has("go.work") || snap.has("go.mod") {
        p.languages.push("go".to_owned());
        if p.build_cmd.is_empty() {
            "go build ./...".clone_into(&mut p.build_cmd);
            "go test ./...".clone_into(&mut p.test_cmd);
            p.scopes = go_work_members(snap.manifest("go.work").unwrap_or_default());
        }
    }
    if snap.has("package.json") {
        p.languages.push("node".to_owned());
        if p.build_cmd.is_empty() {
            // Timid defaults (spec §3.5): `npm test` exists on every package,
            // while a build script may not — the probe tells the truth.
            "npm ci".clone_into(&mut p.build_cmd);
            "npm test".clone_into(&mut p.test_cmd);
            p.scopes =
                pnpm_workspace_members(snap.manifest("pnpm-workspace.yaml").unwrap_or_default());
        }
    }
    if snap.has("pyproject.toml") || snap.has("setup.py") {
        p.languages.push("python".to_owned());
        if p.test_cmd.is_empty() {
            "pytest".clone_into(&mut p.test_cmd);
        }
    }
    if p.languages.is_empty() {
        p.notes.push(
            "no recognized toolchain manifest at the repo root — fill the commands in".to_owned(),
        );
    }
    if p.languages.len() > 1 {
        p.notes.push(format!(
            "several toolchains detected ({}) — commands follow the first; adjust if another leads",
            p.languages.join(", ")
        ));
    }
    p
}

/// Service names live inside ticket `service_tag`s and prompt text: keep them
/// short, lowercase, and free of path/separator noise.
fn sanitize_name(raw: &str) -> String {
    let cleaned: String = raw
        .trim()
        .trim_end_matches(".git")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('-');
    let mut out = String::new();
    let mut prev_dash = false;
    for c in cleaned.chars() {
        if c == '-' {
            if !prev_dash {
                out.push(c);
            }
            prev_dash = true;
        } else {
            out.push(c);
            prev_dash = false;
        }
    }
    if out.is_empty() {
        "service".to_owned()
    } else {
        out.chars().take(40).collect()
    }
}

/// `[workspace] members = ["crates/*", "tools/x"]` → the literal entries.
/// A glob stays as its parent folder (`crates/*` → `crates/`): scopes name
/// folders the gate narrows to, not exact crate lists.
fn cargo_workspace_members(toml: &str) -> Vec<String> {
    let Some(ws) = toml.split("[workspace]").nth(1) else {
        return Vec::new();
    };
    // The members array ends at `]`; a later section would start a new `[`.
    let Some(start) = ws.find("members") else {
        return Vec::new();
    };
    let tail = &ws[start..];
    let Some(open) = tail.find('[') else {
        return Vec::new();
    };
    let Some(close) = tail[open..].find(']') else {
        return Vec::new();
    };
    tail[open + 1..open + close]
        .split(',')
        .filter_map(|e| {
            let e = e.trim().trim_matches('"').trim();
            if e.is_empty() {
                return None;
            }
            Some(match e.split_once('*') {
                Some((prefix, _)) => prefix.trim_end_matches('/').to_owned() + "/",
                None => e.to_owned(),
            })
        })
        .collect()
}

/// `use ( ./a ./b )` or repeated `use ./x` lines → member folders.
fn go_work_members(work: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_block = false;
    for line in work.lines() {
        let l = line.trim();
        if l.starts_with("use") {
            if l.contains('(') {
                in_block = true;
                continue;
            }
            if let Some(p) = l.strip_prefix("use ") {
                out.push(clean_member(p));
            }
        } else if in_block {
            if l.starts_with(')') {
                in_block = false;
            } else if !l.is_empty() {
                out.push(clean_member(l));
            }
        }
    }
    out.retain(|m| !m.is_empty());
    out
}

/// `packages:` entries of pnpm-workspace.yaml → member folders.
fn pnpm_workspace_members(yaml: &str) -> Vec<String> {
    yaml.lines()
        .filter_map(|l| l.trim().strip_prefix("- "))
        .map(|e| {
            let e = e.trim_matches(|c| c == '"' || c == '\'');
            match e.split_once('*') {
                Some((prefix, _)) => prefix.trim_end_matches('/').to_owned() + "/",
                None => e.to_owned(),
            }
        })
        .filter(|m| !m.is_empty())
        .collect()
}

fn clean_member(raw: &str) -> String {
    raw.trim()
        .trim_start_matches("./")
        .trim_matches('"')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(root: &str, files: &[&str], manifests: &[(&str, &str)]) -> RepoSnapshot {
        RepoSnapshot {
            root_name: root.to_owned(),
            files: files.iter().map(|s| (*s).to_owned()).collect(),
            manifests: manifests
                .iter()
                .map(|(p, c)| ((*p).to_owned(), (*c).to_owned()))
                .collect(),
        }
    }

    #[test]
    fn rust_workspace_proposes_cargo_and_member_scopes() {
        let p = propose(&snap(
            "Harxes.git",
            &["Cargo.toml", "Dockerfile", "src/main.rs"],
            &[(
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/*\", \"tools/bench\"]\n",
            )],
        ));
        assert_eq!(p.name, "harxes");
        assert_eq!(p.build_cmd, "cargo build --workspace");
        assert_eq!(p.test_cmd, "cargo test --workspace");
        assert_eq!(p.scopes, vec!["crates/", "tools/bench"]);
        assert!(p.has_dockerfile);
        assert_eq!(p.languages, vec!["rust"]);
    }

    #[test]
    fn unrecognized_tree_proposes_nothing_but_says_so() {
        let p = propose(&snap("mystery", &["README.md"], &[]));
        assert!(p.build_cmd.is_empty() && p.test_cmd.is_empty());
        assert!(p.notes.iter().any(|n| n.contains("no recognized")));
    }

    #[test]
    fn node_repo_gets_timid_npm_defaults_and_pnpm_scopes() {
        let p = propose(&snap(
            "web-app",
            &["package.json", "pnpm-workspace.yaml"],
            &[(
                "pnpm-workspace.yaml",
                "packages:\n  - 'apps/*'\n  - libs/core\n",
            )],
        ));
        assert_eq!(p.build_cmd, "npm ci");
        assert_eq!(p.test_cmd, "npm test");
        assert_eq!(p.scopes, vec!["apps/", "libs/core"]);
    }

    #[test]
    fn go_work_members_parse_both_forms() {
        assert_eq!(
            go_work_members("go 1.22\nuse ./svc\nuse (\n\t./a\n\t./b\n)\n"),
            vec!["svc", "a", "b"]
        );
    }

    #[test]
    fn multi_toolchain_keeps_first_and_notes_the_rest() {
        let p = propose(&snap(
            "mixed",
            &["Cargo.toml", "package.json", "pyproject.toml"],
            &[],
        ));
        assert_eq!(p.build_cmd, "cargo build --workspace");
        assert_eq!(p.languages, vec!["rust", "node", "python"]);
        assert!(p.notes.iter().any(|n| n.contains("several toolchains")));
    }

    #[test]
    fn names_are_sanitized_for_service_tags() {
        assert_eq!(sanitize_name("My Repo!!.git"), "my-repo");
        assert_eq!(sanitize_name("---"), "service");
    }
}
