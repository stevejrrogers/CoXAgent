//! Architecture conformance — deterministic tech-stack enforcement.
//!
//! The real CoXChat run showed an agent building the server in TypeScript when
//! the context said Rust. Prompts alone don't hold the line, so the declared
//! stack is checked in code: each area may require marker files and forbid file
//! extensions; violations become tracked bugs. This is the enforce-by-code
//! answer to architecture drift.

use serde::{Deserialize, Serialize};

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

use std::collections::BTreeMap;

/// Directories never scanned for conformance.
const SKIP_DIRS: &[&str] = &["node_modules", ".git", "target", "dist", "build", ".vite"];

/// Check `rules` against a per-area file listing (relative or absolute paths —
/// matching is by suffix). Areas absent from the map, or listed empty, are
/// skipped: nothing built there yet. Pure function — the caller gathers the
/// listing through the workspace files port.
#[must_use]
pub fn check(files_by_area: &BTreeMap<String, Vec<String>>, rules: &[StackRule]) -> Vec<Violation> {
    let mut out = Vec::new();
    for rule in rules {
        let Some(files) = files_by_area.get(&rule.area).filter(|f| !f.is_empty()) else {
            continue;
        };
        let files: Vec<&String> = files
            .iter()
            .filter(|f| !f.split(['/', '\\']).any(|seg| SKIP_DIRS.contains(&seg)))
            .collect();

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

#[cfg(test)]
mod tests {
    use super::*;

    fn area(name: &str, files: &[&str]) -> BTreeMap<String, Vec<String>> {
        let mut m = BTreeMap::new();
        m.insert(
            name.to_owned(),
            files.iter().map(|f| (*f).to_owned()).collect(),
        );
        m
    }

    fn rule(require: &[&str], forbid: &[&str]) -> StackRule {
        StackRule {
            area: "server".to_owned(),
            language: "Rust".to_owned(),
            require_any: require.iter().map(|s| (*s).to_owned()).collect(),
            forbid_ext: forbid.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn missing_marker_and_forbidden_files_both_flag() {
        let files = area("server", &["server/index.ts", "server/package.json"]);
        let v = check(&files, &[rule(&["Cargo.toml"], &[".ts", ".tsx"])]);
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(v.iter().any(|x| x.message.contains("Cargo.toml")));
        assert!(v.iter().any(|x| x.message.contains("forbidden")));
    }

    #[test]
    fn conformant_rust_server_passes() {
        let files = area("server", &["server/Cargo.toml", "server/src/main.rs"]);
        assert!(check(&files, &[rule(&["Cargo.toml"], &[".ts"])]).is_empty());
    }

    #[test]
    fn missing_or_empty_area_is_skipped() {
        assert!(check(&BTreeMap::new(), &[rule(&["Cargo.toml"], &[])]).is_empty());
        assert!(check(&area("server", &[]), &[rule(&["Cargo.toml"], &[])]).is_empty());
    }

    #[test]
    fn vendor_directories_do_not_count_as_evidence() {
        // A Cargo.toml buried in node_modules must not satisfy the marker rule.
        let files = area("server", &["server/node_modules/x/Cargo.toml"]);
        let v = check(&files, &[rule(&["Cargo.toml"], &[])]);
        assert_eq!(v.len(), 1, "{v:?}");
    }
}
