//! `CodeGraph` — a minimal, dependency-free code knowledge graph (the native
//! take on GitNexus). It walks a repository, extracts top-level symbols and
//! per-file imports with lightweight per-language heuristics, and builds an
//! import/dependency graph. The result powers a human "code map" view and a
//! compact repo map that agents can read instead of blindly exploring a large
//! tree — cutting tokens and grounding their work.
//!
//! Heuristic (regex/line-based), not a full parser: fast, portable, and good
//! enough for orientation. It never executes code.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// One extracted definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    /// `fn`/`struct`/`enum`/`trait`/`class`/`interface`/`type`/`const`.
    pub kind: String,
    /// Repo-relative path.
    pub file: String,
    pub line: usize,
    pub lang: String,
}

/// One source file in the graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileNode {
    pub path: String,
    pub lang: String,
    pub loc: usize,
    pub symbols: usize,
    /// Modules/paths this file imports (raw, as written).
    pub imports: Vec<String>,
}

/// The whole graph, persisted to `.coxagent/codegraph.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodeGraph {
    pub built_at: String,
    pub files: Vec<FileNode>,
    pub symbols: Vec<Symbol>,
    /// `(from_file, imported)` import edges.
    pub edges: Vec<(String, String)>,
    /// Language → file count.
    pub languages: BTreeMap<String, usize>,
}

impl CodeGraph {
    /// Build the graph by walking `root`. Skips vendored/build/VCS directories
    /// and binary/huge files. Best-effort — unreadable files are skipped.
    #[must_use]
    pub fn index(root: &Path) -> Self {
        let mut g = CodeGraph {
            built_at: now_rfc3339(),
            ..Default::default()
        };
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                if path.is_dir() {
                    if is_skipped_dir(&name) {
                        continue;
                    }
                    stack.push(path);
                } else if let Some(lang) = lang_of(&name) {
                    // Guard against pathological files.
                    let Ok(meta) = entry.metadata() else { continue };
                    if meta.len() > 1_500_000 {
                        continue;
                    }
                    let Ok(text) = std::fs::read_to_string(&path) else {
                        continue;
                    };
                    let rel = path
                        .strip_prefix(root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    g.ingest_file(&rel, lang, &text);
                }
            }
        }
        g.files.sort_by(|a, b| a.path.cmp(&b.path));
        g.symbols.sort_by_key(|s| s.name.to_lowercase());
        g
    }

    fn ingest_file(&mut self, rel: &str, lang: &'static str, text: &str) {
        let mut imports = Vec::new();
        let mut sym_count = 0usize;
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if let Some(imp) = extract_import(lang, line) {
                if !imp.is_empty() {
                    imports.push(imp.clone());
                    self.edges.push((rel.to_owned(), imp));
                }
            }
            if let Some((kind, name)) = extract_symbol(lang, line) {
                sym_count += 1;
                self.symbols.push(Symbol {
                    name,
                    kind: kind.to_owned(),
                    file: rel.to_owned(),
                    line: i + 1,
                    lang: lang.to_owned(),
                });
            }
        }
        imports.sort();
        imports.dedup();
        *self.languages.entry(lang.to_owned()).or_insert(0) += 1;
        self.files.push(FileNode {
            path: rel.to_owned(),
            lang: lang.to_owned(),
            loc: text.lines().count(),
            symbols: sym_count,
            imports,
        });
    }

    /// Case-insensitive symbol search by name substring, capped.
    #[must_use]
    pub fn search(&self, query: &str, limit: usize) -> Vec<&Symbol> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        self.symbols
            .iter()
            .filter(|s| s.name.to_lowercase().contains(&q))
            .take(limit)
            .collect()
    }

    /// Files that import `file`'s module (crude reverse-dependency by basename).
    #[must_use]
    pub fn dependents(&self, file: &str) -> Vec<String> {
        let stem = Path::new(file)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if stem.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<String> = self
            .edges
            .iter()
            .filter(|(_, imp)| imp.split(['/', '.', ':']).any(|seg| seg == stem))
            .map(|(from, _)| from.clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// A compact, token-bounded overview for agents: languages, then each file
    /// with its symbols. Truncated to `max_chars`.
    #[must_use]
    pub fn repo_map(&self, max_chars: usize) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "# Repo map — {} files, {} symbols",
            self.files.len(),
            self.symbols.len()
        );
        let langs: Vec<String> = self
            .languages
            .iter()
            .map(|(l, n)| format!("{l} {n}"))
            .collect();
        let _ = writeln!(s, "Languages: {}\n", langs.join(", "));
        for f in &self.files {
            let syms: Vec<String> = self
                .symbols
                .iter()
                .filter(|sy| sy.file == f.path)
                .take(12)
                .map(|sy| format!("{} {}", sy.kind, sy.name))
                .collect();
            let _ = writeln!(s, "## {} ({})", f.path, f.lang);
            if !syms.is_empty() {
                let _ = writeln!(s, "  {}", syms.join(", "));
            }
            if s.len() > max_chars {
                s.push_str("\n… [truncated] …\n");
                break;
            }
        }
        s
    }

    /// Persist to `<root>/.coxagent/codegraph.json`.
    ///
    /// # Errors
    /// IO/serialisation failures.
    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        let dir = root.join(".coxagent");
        std::fs::create_dir_all(&dir)?;
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(dir.join("codegraph.json"), json)
    }

    /// Load a previously built graph, if present.
    #[must_use]
    pub fn load(root: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(root.join(".coxagent").join("codegraph.json")).ok()?;
        serde_json::from_str(&text).ok()
    }
}

fn is_skipped_dir(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | "node_modules"
            | "target"
            | "dist"
            | "build"
            | ".next"
            | ".venv"
            | "venv"
            | "__pycache__"
            | ".coxagent"
            | "vendor"
            | ".idea"
            | ".vscode"
    ) || name.starts_with('.') && name.len() > 1 && name != ".github"
}

fn lang_of(name: &str) -> Option<&'static str> {
    let ext = name.rsplit_once('.').map(|(_, e)| e)?;
    Some(match ext {
        "rs" => "rust",
        "py" => "python",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "go" => "go",
        "java" => "java",
        "rb" => "ruby",
        _ => return None,
    })
}

/// Extract an import target from one line, per language (raw, best-effort).
fn extract_import(lang: &str, line: &str) -> Option<String> {
    match lang {
        "rust" => line
            .strip_prefix("use ")
            .map(|r| r.trim_end_matches(';').trim().to_owned()),
        "python" => {
            if let Some(r) = line.strip_prefix("from ") {
                r.split_whitespace().next().map(str::to_owned)
            } else {
                line.strip_prefix("import ")
                    .and_then(|r| r.split([' ', ',']).next())
                    .map(str::to_owned)
            }
        }
        "go" | "java" => line
            .strip_prefix("import ")
            .map(|r| r.trim().trim_matches(['"', '(', ' ']).to_owned()),
        "typescript" | "javascript" => {
            let i = line.find("from ")?;
            let rest = line[i + 5..].trim();
            let q = rest.trim_start_matches(['"', '\'', '`']);
            q.split(['"', '\'', '`']).next().map(str::to_owned)
        }
        "ruby" => line
            .strip_prefix("require ")
            .or_else(|| line.strip_prefix("require_relative "))
            .map(|r| r.trim().trim_matches(['"', '\'']).to_owned()),
        _ => None,
    }
}

/// Extract a top-level definition `(kind, name)` from one line, per language.
fn extract_symbol(lang: &str, line: &str) -> Option<(&'static str, String)> {
    // Strip common visibility/qualifier prefixes so the keyword is at the front.
    let l = line
        .trim_start_matches("pub ")
        .trim_start_matches("export ")
        .trim_start_matches("default ")
        .trim_start_matches("async ")
        .trim_start_matches("pub(crate) ")
        .trim_start();
    let word_after = |kw: &str| -> Option<String> {
        l.strip_prefix(kw).and_then(|r| {
            let n: String = r
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            (!n.is_empty()).then_some(n)
        })
    };
    match lang {
        "rust" => word_after("fn ")
            .map(|n| ("fn", n))
            .or_else(|| word_after("struct ").map(|n| ("struct", n)))
            .or_else(|| word_after("enum ").map(|n| ("enum", n)))
            .or_else(|| word_after("trait ").map(|n| ("trait", n)))
            .or_else(|| word_after("type ").map(|n| ("type", n)))
            .or_else(|| word_after("const ").map(|n| ("const", n)))
            .or_else(|| word_after("static ").map(|n| ("const", n))),
        "python" => word_after("def ")
            .map(|n| ("fn", n))
            .or_else(|| word_after("class ").map(|n| ("class", n))),
        "go" => word_after("func ")
            .map(|n| ("fn", n))
            .or_else(|| word_after("type ").map(|n| ("type", n))),
        "typescript" | "javascript" => word_after("function ")
            .map(|n| ("fn", n))
            .or_else(|| word_after("class ").map(|n| ("class", n)))
            .or_else(|| word_after("interface ").map(|n| ("interface", n)))
            .or_else(|| word_after("type ").map(|n| ("type", n)))
            .or_else(|| word_after("const ").map(|n| ("const", n))),
        "java" => word_after("class ")
            .map(|n| ("class", n))
            .or_else(|| word_after("interface ").map(|n| ("interface", n))),
        "ruby" => word_after("def ")
            .map(|n| ("fn", n))
            .or_else(|| word_after("class ").map(|n| ("class", n)))
            .or_else(|| word_after("module ").map(|n| ("class", n))),
        _ => None,
    }
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_symbols_and_imports() {
        assert_eq!(
            extract_symbol("rust", "pub fn build(x: u8) {"),
            Some(("fn", "build".to_owned()))
        );
        assert_eq!(
            extract_symbol("rust", "struct AppState {"),
            Some(("struct", "AppState".to_owned()))
        );
        assert_eq!(
            extract_import("rust", "use std::sync::Arc;"),
            Some("std::sync::Arc".to_owned())
        );
    }

    #[test]
    fn python_and_ts() {
        assert_eq!(
            extract_symbol("python", "def handler(req):"),
            Some(("fn", "handler".to_owned()))
        );
        assert_eq!(
            extract_symbol("python", "class Widget:"),
            Some(("class", "Widget".to_owned()))
        );
        assert_eq!(
            extract_import("python", "from app.core import db"),
            Some("app.core".to_owned())
        );
        assert_eq!(
            extract_symbol("typescript", "export function render() {"),
            Some(("fn", "render".to_owned()))
        );
        assert_eq!(
            extract_import("typescript", "import { x } from './foo'"),
            Some("./foo".to_owned())
        );
    }

    #[test]
    fn index_walks_and_maps() {
        let dir = std::env::temp_dir().join(format!("cgtest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("mk");
        std::fs::write(
            dir.join("src/main.rs"),
            "use std::io;\npub fn main() {}\nstruct S;\n",
        )
        .expect("w");
        std::fs::create_dir_all(dir.join("node_modules")).expect("mk2");
        std::fs::write(dir.join("node_modules/skip.js"), "function ignoreMe(){}").expect("w2");

        let g = CodeGraph::index(&dir);
        assert_eq!(g.files.len(), 1, "node_modules must be skipped");
        assert!(g.symbols.iter().any(|s| s.name == "main" && s.kind == "fn"));
        assert!(g
            .symbols
            .iter()
            .any(|s| s.name == "S" && s.kind == "struct"));
        assert!(g.search("mai", 10).iter().any(|s| s.name == "main"));
        assert!(g.repo_map(10_000).contains("src/main.rs"));

        g.save(&dir).expect("save");
        assert!(CodeGraph::load(&dir).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
