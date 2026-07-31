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
    /// `fn`/`struct`/`enum`/`trait`/`class`/`interface`/`type`/`const`/`macro`.
    pub kind: String,
    /// Containing type/class when known (e.g. `AppState` for `AppState::build`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
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

/// One function-call edge in the call graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Call {
    /// File the call site is in.
    pub file: String,
    /// Calling function's name.
    pub caller: String,
    /// Calling function's scope (type/class), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_scope: Option<String>,
    /// Name of the function being called.
    pub callee: String,
    pub line: usize,
}

/// The whole graph, persisted to `.coxagent/codegraph.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodeGraph {
    pub built_at: String,
    pub files: Vec<FileNode>,
    pub symbols: Vec<Symbol>,
    /// `(from_file, imported)` import edges.
    pub edges: Vec<(String, String)>,
    /// Function-call edges (caller → callee).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<Call>,
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
        // Imports drive the file→file dep graph — the line heuristic is fine.
        let mut imports = Vec::new();
        for raw in text.lines() {
            if let Some(imp) = extract_import(lang, raw.trim()) {
                if !imp.is_empty() {
                    imports.push(imp.clone());
                    self.edges.push((rel.to_owned(), imp));
                }
            }
        }
        imports.sort();
        imports.dedup();

        // Symbols: accurate tree-sitter parse when supported, else the heuristic.
        // An empty parse (e.g. a grammar edge case) also falls back, so we never
        // lose symbols a simple scan would have found.
        let mut sym_count = 0usize;
        if let Some(syms) = crate::ts::symbols(lang, text).filter(|v| !v.is_empty()) {
            for s in syms {
                sym_count += 1;
                self.symbols.push(Symbol {
                    name: s.name,
                    kind: s.kind.to_owned(),
                    scope: s.scope,
                    file: rel.to_owned(),
                    line: s.line,
                    lang: lang.to_owned(),
                });
            }
        } else {
            for (i, raw) in text.lines().enumerate() {
                if let Some((kind, name)) = extract_symbol(lang, raw.trim()) {
                    sym_count += 1;
                    self.symbols.push(Symbol {
                        name,
                        kind: kind.to_owned(),
                        scope: None,
                        file: rel.to_owned(),
                        line: i + 1,
                        lang: lang.to_owned(),
                    });
                }
            }
        }
        // Call graph (function → function), where the grammar supports it.
        if let Some(calls) = crate::ts::calls(lang, text) {
            for c in calls {
                self.calls.push(Call {
                    file: rel.to_owned(),
                    caller: c.caller,
                    caller_scope: c.caller_scope,
                    callee: c.callee,
                    line: c.line,
                });
            }
        }
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

    /// Relevance search: rank symbols by how many query terms match their name,
    /// scope, and file path (splitting camelCase/snake_case). Finds things a
    /// plain substring misses — e.g. "auth user" ranks `authenticate_user`,
    /// `User::login` in `auth.rs` — without needing an embedding model.
    #[must_use]
    pub fn relevance_search(&self, query: &str, limit: usize) -> Vec<&Symbol> {
        let terms: Vec<String> = tokenize(query);
        if terms.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(u32, &Symbol)> = self
            .symbols
            .iter()
            .filter_map(|s| {
                let name_toks = tokenize(&s.name);
                let scope_toks = s.scope.as_deref().map(tokenize).unwrap_or_default();
                let path_toks = tokenize(&s.file);
                let mut score = 0u32;
                for t in &terms {
                    // Name matches weigh most, then scope, then path.
                    if name_toks.iter().any(|w| w == t) {
                        score += 6;
                    } else if s.name.to_lowercase().contains(t.as_str()) {
                        score += 3;
                    }
                    if scope_toks.iter().any(|w| w == t) {
                        score += 2;
                    }
                    if path_toks.iter().any(|w| w == t) {
                        score += 1;
                    }
                }
                (score > 0).then_some((score, s))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.len().cmp(&b.1.name.len())));
        scored.into_iter().take(limit).map(|(_, s)| s).collect()
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

    /// Resolve import edges to `(from_file, to_file)` where the import points at
    /// another indexed file (matched by file stem). Deduped. This is the
    /// internal dependency graph, ready to visualise.
    #[must_use]
    pub fn resolved_edges(&self) -> Vec<(String, String)> {
        // stem -> file path (first wins; ambiguous stems are best-effort).
        let mut by_stem: BTreeMap<String, String> = BTreeMap::new();
        for f in &self.files {
            if let Some(stem) = Path::new(&f.path).file_stem() {
                by_stem
                    .entry(stem.to_string_lossy().to_string())
                    .or_insert_with(|| f.path.clone());
            }
        }
        let mut out: Vec<(String, String)> = Vec::new();
        for (from, imp) in &self.edges {
            for seg in imp.split(['/', '.', ':', '\\']).rev() {
                if let Some(target) = by_stem.get(seg) {
                    if target != from {
                        out.push((from.clone(), target.clone()));
                    }
                    break;
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// Functions that call `name` — "if I change `name`, these break". Each entry
    /// is `(caller_label, file, line)`, deduped, sorted.
    #[must_use]
    pub fn callers(&self, name: &str) -> Vec<(String, String, usize)> {
        let mut out: Vec<(String, String, usize)> = self
            .calls
            .iter()
            .filter(|c| c.callee == name)
            .map(|c| {
                (
                    label(&c.caller, c.caller_scope.as_deref()),
                    c.file.clone(),
                    c.line,
                )
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Functions that `name` (a function) calls. `(callee, file, line)`.
    #[must_use]
    pub fn callees(&self, name: &str) -> Vec<(String, String, usize)> {
        let mut out: Vec<(String, String, usize)> = self
            .calls
            .iter()
            .filter(|c| c.caller == name)
            .map(|c| (c.callee.clone(), c.file.clone(), c.line))
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
            "# Repo map — {} files, {} symbols\n\
             Query it (if `coxagent` is on PATH): `coxagent codegraph search|impact|callers <name>`.",
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
                .map(|sy| match &sy.scope {
                    Some(sc) => format!("{} {sc}::{}", sy.kind, sy.name),
                    None => format!("{} {}", sy.kind, sy.name),
                })
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
        std::fs::write(dir.join("codegraph.json"), json)?;
        // The human-readable map ships with the graph: one producer, one save,
        // no caller left to remember the second artifact.
        std::fs::write(dir.join("REPO_MAP.md"), self.repo_map(40_000))
    }

    /// Load a previously built graph, if present.
    #[must_use]
    pub fn load(root: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(root.join(".coxagent").join("codegraph.json")).ok()?;
        serde_json::from_str(&text).ok()
    }
}

/// One usage of a symbol found by an on-demand text scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reference {
    pub file: String,
    pub line: usize,
    pub text: String,
    /// True when this line is the symbol's own definition.
    pub is_def: bool,
}

/// Whether `name` occurs in `line` as a whole identifier (word boundaries).
fn word_matches(line: &str, name: &str) -> bool {
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(pos) = line[from..].find(name) {
        let i = from + pos;
        let before_ok = i == 0 || !is_ident(bytes[i - 1]);
        let after = i + name.len();
        let after_ok = after >= bytes.len() || !is_ident(bytes[after]);
        if before_ok && after_ok {
            return true;
        }
        from = i + name.len();
    }
    false
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Impact analysis: scan the tree for whole-word usages of `name`. On-demand
/// (re-reads source), so it reflects the current tree without a rebuild.
#[must_use]
pub fn references(root: &Path, name: &str, limit: usize) -> Vec<Reference> {
    let name = name.trim();
    if name.is_empty() || name.len() < 2 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let fname = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if !is_skipped_dir(&fname) {
                    stack.push(path);
                }
            } else if let Some(lang) = lang_of(&fname) {
                if entry.metadata().map_or(0, |m| m.len()) > 1_500_000 {
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
                let lines: Vec<&str> = text.lines().collect();
                let push_ref = |out: &mut Vec<Reference>, ln: usize, raw: &str| {
                    let is_def = extract_symbol(lang, raw.trim()).is_some_and(|(_, n)| n == name);
                    out.push(Reference {
                        file: rel.clone(),
                        line: ln,
                        text: raw.trim().chars().take(200).collect(),
                        is_def,
                    });
                };
                // Tree-sitter counts only real identifier tokens — usages inside
                // comments and string literals are correctly ignored. Heuristic
                // word-scan for languages without a grammar.
                if let Some(ref_lines) = crate::ts::reference_lines(lang, &text, name) {
                    for ln in ref_lines {
                        let raw = lines.get(ln.saturating_sub(1)).copied().unwrap_or("");
                        push_ref(&mut out, ln, raw);
                        if out.len() >= limit {
                            return sort_refs(out);
                        }
                    }
                } else {
                    for (i, raw) in lines.iter().enumerate() {
                        if word_matches(raw, name) {
                            push_ref(&mut out, i + 1, raw);
                            if out.len() >= limit {
                                return sort_refs(out);
                            }
                        }
                    }
                }
            }
        }
    }
    sort_refs(out)
}

/// Lowercase word tokens from an identifier or path, splitting on non-alnum,
/// camelCase, and snake_case boundaries (`authenticateUser` → `authenticate`,
/// `user`). Drops 1-char tokens.
pub(crate) fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            // camelCase boundary: lower→Upper starts a new token.
            if ch.is_uppercase() && prev_lower && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cur.push(ch.to_ascii_lowercase());
            prev_lower = ch.is_lowercase();
        } else {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.retain(|t| t.len() > 1);
    out
}

/// `scope::name` when a scope is known, else `name`.
fn label(name: &str, scope: Option<&str>) -> String {
    match scope {
        Some(s) if !s.is_empty() => format!("{s}::{name}"),
        _ => name.to_owned(),
    }
}

/// Definitions first, then by file/line.
fn sort_refs(mut refs: Vec<Reference>) -> Vec<Reference> {
    refs.sort_by(|a, b| {
        b.is_def
            .cmp(&a.is_def)
            .then(a.file.cmp(&b.file))
            .then(a.line.cmp(&b.line))
    });
    refs
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
        "ts" => "typescript",
        "tsx" => "tsx",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "go" => "go",
        "java" => "java",
        "swift" => "swift",
        "cs" => "csharp",
        "rb" => "ruby",
        "php" => "php",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => "cpp",
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
    fn word_matches_respects_boundaries() {
        assert!(word_matches("let x = build();", "build"));
        assert!(word_matches("build(a, b)", "build"));
        assert!(!word_matches("let x = rebuild();", "build"));
        assert!(!word_matches("building = 1", "build"));
    }

    #[test]
    fn treesitter_excludes_comments_and_strings() {
        let dir = std::env::temp_dir().join(format!("cgts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("mk");
        // `widget` appears as: a real def, a real call, a comment, and a string.
        std::fs::write(
            dir.join("src/a.rs"),
            "pub fn widget() {}\nfn caller() { widget(); }\n// call widget here\nlet s = \"widget\";\n",
        )
        .expect("w");
        let refs = references(&dir, "widget", 50);
        // Heuristic would return 4; tree-sitter returns only the 2 real ones.
        assert_eq!(refs.len(), 2, "comment + string usages must be excluded");
        assert!(refs.iter().any(|r| r.is_def && r.line == 1));
        assert!(refs.iter().any(|r| !r.is_def && r.line == 2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tokenize_splits_camel_and_snake() {
        assert_eq!(tokenize("authenticateUser"), vec!["authenticate", "user"]);
        assert_eq!(tokenize("get_current_user"), vec!["get", "current", "user"]);
        assert_eq!(
            tokenize("src/auth/login.rs"),
            vec!["src", "auth", "login", "rs"]
        );
    }

    #[test]
    fn relevance_ranks_multi_term_matches_higher() {
        let dir = std::env::temp_dir().join(format!("cgrel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("auth")).expect("mk");
        std::fs::write(
            dir.join("auth/login.rs"),
            "pub fn authenticate_user() {}\npub fn parse_json() {}\n",
        )
        .expect("w");
        let g = CodeGraph::index(&dir);
        let hits = g.relevance_search("auth user", 10);
        assert_eq!(
            hits.first().map(|s| s.name.as_str()),
            Some("authenticate_user"),
            "the fn matching both terms (name + auth/ path) ranks first"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn c_cpp_ruby_php_symbols_and_calls() {
        let dir = std::env::temp_dir().join(format!("cgccrp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("mk");
        std::fs::write(
            dir.join("src/a.c"),
            "int help(){return 0;}\nint run(){return help();}",
        )
        .expect("c");
        std::fs::write(
            dir.join("src/b.cpp"),
            "class Foo { public: void bar(){ baz(); } };\nvoid baz(){}",
        )
        .expect("cpp");
        std::fs::write(
            dir.join("src/c.rb"),
            "class Foo\n  def bar\n    helper\n  end\n  def helper\n  end\nend",
        )
        .expect("rb");
        std::fs::write(
            dir.join("src/d.php"),
            "<?php\nclass Foo { function bar(){ baz(); } }\nfunction baz(){}",
        )
        .expect("php");
        let g = CodeGraph::index(&dir);
        // C free function + call.
        assert!(g.symbols.iter().any(|s| s.name == "help" && s.kind == "fn"));
        assert!(g.callers("help").iter().any(|(w, _, _)| w == "run"));
        // C++ method scope + call.
        assert!(g
            .symbols
            .iter()
            .any(|s| s.name == "bar" && s.scope.as_deref() == Some("Foo")));
        assert!(g.callers("baz").iter().any(|(w, _, _)| w == "Foo::bar"));
        // Ruby method scope + call.
        assert!(g
            .symbols
            .iter()
            .any(|s| s.name == "helper" && s.scope.as_deref() == Some("Foo")));
        // PHP method scope + call.
        assert!(g
            .symbols
            .iter()
            .any(|s| s.name == "bar" && s.lang == "php" && s.scope.as_deref() == Some("Foo")));
        assert!(g.callers("baz").iter().any(|(w, _, _)| w == "Foo::bar"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn java_csharp_swift_symbols_and_calls() {
        let dir = std::env::temp_dir().join(format!("cgjcs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("mk");
        std::fs::write(
            dir.join("src/Foo.java"),
            "class Foo { void bar() { helper(); } void helper() {} }",
        )
        .expect("j");
        std::fs::write(
            dir.join("src/Prog.cs"),
            "class Prog { void Run() { Work(); } void Work() {} }",
        )
        .expect("c");
        std::fs::write(
            dir.join("src/App.swift"),
            "class App { func build() { validate() }\nfunc validate() {} }",
        )
        .expect("s");
        let g = CodeGraph::index(&dir);
        // Scope-aware methods across all three languages.
        assert!(g
            .symbols
            .iter()
            .any(|s| s.name == "bar" && s.scope.as_deref() == Some("Foo")));
        assert!(g
            .symbols
            .iter()
            .any(|s| s.name == "Run" && s.scope.as_deref() == Some("Prog")));
        assert!(g
            .symbols
            .iter()
            .any(|s| s.name == "build" && s.scope.as_deref() == Some("App")));
        // Call graph resolves in each.
        assert!(g.callers("helper").iter().any(|(w, _, _)| w == "Foo::bar"));
        assert!(g.callers("Work").iter().any(|(w, _, _)| w == "Prog::Run"));
        assert!(g
            .callers("validate")
            .iter()
            .any(|(w, _, _)| w == "App::build"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn call_graph_links_callers_and_callees() {
        let dir = std::env::temp_dir().join(format!("cgcall-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("mk");
        std::fs::write(
            dir.join("src/a.rs"),
            "fn helper() {}\nfn run() { helper(); helper(); }\nfn main() { run(); }\n",
        )
        .expect("w");
        let g = CodeGraph::index(&dir);
        let callers = g.callers("helper");
        assert!(
            callers.iter().any(|(who, _, _)| who == "run"),
            "run calls helper"
        );
        let run_calls: Vec<String> = g.callees("run").into_iter().map(|(c, _, _)| c).collect();
        assert!(run_calls.contains(&"helper".to_owned()));
        assert!(g.callers("run").iter().any(|(who, _, _)| who == "main"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn treesitter_captures_scope_and_methods() {
        let syms = crate::ts::symbols(
            "rust",
            "struct App;\nimpl App {\n  pub fn build(&self) {}\n}\n",
        )
        .expect("rust supported");
        assert!(syms.iter().any(|s| s.name == "App" && s.kind == "struct"));
        let m = syms
            .iter()
            .find(|s| s.name == "build")
            .expect("method found");
        assert_eq!(
            m.scope.as_deref(),
            Some("App"),
            "method scope is its impl type"
        );
    }

    #[test]
    fn resolved_edges_link_files() {
        let dir = std::env::temp_dir().join(format!("cgdep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("mk");
        std::fs::write(dir.join("src/tokens.rs"), "pub fn compress() {}\n").expect("w");
        std::fs::write(dir.join("src/user.rs"), "use crate::tokens::compress;\n").expect("w2");
        let g = CodeGraph::index(&dir);
        let e = g.resolved_edges();
        assert!(e
            .iter()
            .any(|(f, t)| f == "src/user.rs" && t == "src/tokens.rs"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn references_finds_usages_and_marks_def() {
        let dir = std::env::temp_dir().join(format!("cgref-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("mk");
        std::fs::write(dir.join("src/a.rs"), "pub fn widget() {}\n").expect("w");
        std::fs::write(
            dir.join("src/b.rs"),
            "fn main() { widget(); let w = widget(); }\n",
        )
        .expect("w2");
        let refs = references(&dir, "widget", 50);
        assert!(refs.iter().any(|r| r.is_def && r.file == "src/a.rs"));
        assert!(refs.iter().filter(|r| !r.is_def).count() >= 1);
        assert!(refs[0].is_def, "definition should sort first");
        let _ = std::fs::remove_dir_all(&dir);
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
