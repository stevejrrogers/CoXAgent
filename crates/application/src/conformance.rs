//! Architecture conformance — deterministic tech-stack enforcement.
//!
//! The real CoXChat run showed an agent building the server in TypeScript when
//! the context said Rust. Prompts alone don't hold the line, so the declared
//! stack is checked in code: each area may require marker files and forbid file
//! extensions; violations become tracked bugs. This is the enforce-by-code
//! answer to architecture drift.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// A rule for one area (subdirectory) of the codebase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackRule {
    /// Subdirectory relative to the codebase root (e.g. `server`).
    pub area: String,
    /// Human label of the required language/stack (e.g. `Rust`).
    pub language: String,
    /// At least one of these files must exist in the area (e.g. `Cargo.toml`).
    #[serde(default)]
    pub require_any: Vec<String>,
    /// File extensions that must not appear in the area (e.g. `.ts`, `.tsx`).
    #[serde(default)]
    pub forbid_ext: Vec<String>,
}

/// A detected conformance violation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub area: String,
    pub message: String,
}

impl Violation {
    /// A stable, dedupe-friendly bug title for this violation.
    #[must_use]
    pub fn bug_title(&self) -> String {
        format!("Architecture drift in {}", self.area)
    }
}

/// Directories never scanned for conformance.
const SKIP_DIRS: &[&str] = &["node_modules", ".git", "target", "dist", "build", ".vite"];

/// Check the codebase at `root` against `rules`. Areas that don't exist yet are
/// skipped (nothing built there). Returns one violation per broken rule.
#[must_use]
pub fn check(root: &Path, rules: &[StackRule]) -> Vec<Violation> {
    let mut out = Vec::new();
    for rule in rules {
        let dir = root.join(&rule.area);
        if !dir.is_dir() {
            continue;
        }
        let files = list_files(&dir);

        if !rule.require_any.is_empty()
            && !rule
                .require_any
                .iter()
                .any(|marker| files.iter().any(|f| f.ends_with(marker)))
        {
            out.push(Violation {
                area: rule.area.clone(),
                message: format!(
                    "{} expected but no marker file ({}) found in `{}`",
                    rule.language,
                    rule.require_any.join(", "),
                    rule.area
                ),
            });
        }

        let bad: Vec<&str> = rule
            .forbid_ext
            .iter()
            .filter(|ext| files.iter().any(|f| f.ends_with(ext.as_str())))
            .map(String::as_str)
            .collect();
        if !bad.is_empty() {
            out.push(Violation {
                area: rule.area.clone(),
                message: format!(
                    "`{}` must be {} but contains forbidden {} files",
                    rule.area,
                    rule.language,
                    bad.join(", ")
                ),
            });
        }
    }
    out
}

/// Recursively list file paths (as strings) under `dir`, skipping vendor dirs.
fn list_files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    walk(dir, &mut out);
    out
}

fn walk(dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if !SKIP_DIRS.contains(&name.as_ref()) {
                walk(&path, out);
            }
        } else {
            out.push(path.to_string_lossy().into_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        std::fs::write(p, "x").expect("write");
    }

    #[test]
    fn flags_forbidden_extension_and_missing_marker() {
        let dir = tempfile::tempdir().expect("tmp");
        let root = dir.path();
        // server built in TypeScript when it should be Rust.
        write(root, "server/src/index.ts");
        write(root, "server/package.json");

        let rules = vec![StackRule {
            area: "server".to_owned(),
            language: "Rust".to_owned(),
            require_any: vec!["Cargo.toml".to_owned()],
            forbid_ext: vec![".ts".to_owned(), ".tsx".to_owned()],
        }];
        let v = check(root, &rules);
        // Two violations: missing Cargo.toml AND forbidden .ts files.
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(v.iter().any(|x| x.message.contains("Cargo.toml")));
        assert!(v.iter().any(|x| x.message.contains("forbidden")));
    }

    #[test]
    fn conformant_rust_server_passes() {
        let dir = tempfile::tempdir().expect("tmp");
        let root = dir.path();
        write(root, "server/Cargo.toml");
        write(root, "server/src/main.rs");
        let rules = vec![StackRule {
            area: "server".to_owned(),
            language: "Rust".to_owned(),
            require_any: vec!["Cargo.toml".to_owned()],
            forbid_ext: vec![".ts".to_owned()],
        }];
        assert!(check(root, &rules).is_empty());
    }

    #[test]
    fn missing_area_is_skipped() {
        let dir = tempfile::tempdir().expect("tmp");
        let rules = vec![StackRule {
            area: "server".to_owned(),
            language: "Rust".to_owned(),
            require_any: vec!["Cargo.toml".to_owned()],
            forbid_ext: vec![],
        }];
        assert!(check(dir.path(), &rules).is_empty());
    }
}
