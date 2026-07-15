//! Tree-sitter parsing for the code graph — accurate, scope-aware symbol and
//! reference extraction that the line/regex heuristic can't match. Supports
//! Rust, Python, JavaScript, TypeScript/TSX, Go, Java, C#, Swift, C, C++, Ruby
//! and PHP; other languages fall back to the heuristic. Never executes code — it
//! only parses.

use tree_sitter::{Language, Node, Parser};

/// A definition found by the parser.
pub struct TsSymbol {
    pub name: String,
    pub kind: &'static str,
    /// Containing type/class, e.g. `AppState` for `AppState::build`.
    pub scope: Option<String>,
    pub line: usize,
}

fn language_for(lang: &str) -> Option<Language> {
    Some(match lang {
        "rust" => tree_sitter_rust::language(),
        "python" => tree_sitter_python::language(),
        "javascript" => tree_sitter_javascript::language(),
        "typescript" => tree_sitter_typescript::language_typescript(),
        "tsx" => tree_sitter_typescript::language_tsx(),
        "go" => tree_sitter_go::language(),
        "java" => tree_sitter_java::language(),
        "csharp" => tree_sitter_c_sharp::language(),
        "swift" => tree_sitter_swift::language(),
        "c" => tree_sitter_c::language(),
        "cpp" => tree_sitter_cpp::language(),
        "ruby" => tree_sitter_ruby::language(),
        "php" => tree_sitter_php::language_php(),
        _ => return None,
    })
}

/// Whether this language is parsed by tree-sitter (vs the heuristic fallback).
#[must_use]
pub fn supported(lang: &str) -> bool {
    matches!(
        lang,
        "rust"
            | "python"
            | "javascript"
            | "typescript"
            | "tsx"
            | "go"
            | "java"
            | "csharp"
            | "swift"
            | "c"
            | "cpp"
            | "ruby"
            | "php"
    )
}

fn parse(lang: &str, source: &str) -> Option<tree_sitter::Tree> {
    let language = language_for(lang)?;
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    parser.parse(source, None)
}

fn text<'a>(node: Node, src: &'a str) -> &'a str {
    node.utf8_text(src.as_bytes()).unwrap_or("")
}

fn name_field(node: Node, src: &str) -> Option<String> {
    node.child_by_field_name("name")
        .map(|n| text(n, src).to_owned())
        .filter(|s| !s.is_empty())
}

/// First identifier-ish descendant's text — for grammars (C/C++) that bury the
/// name inside a declarator rather than a `name` field.
fn descend_name(node: Node, src: &str) -> Option<String> {
    if matches!(
        node.kind(),
        "identifier" | "field_identifier" | "type_identifier"
    ) {
        return Some(text(node, src).to_owned());
    }
    let mut cursor = node.walk();
    for c in node.named_children(&mut cursor) {
        if let Some(n) = descend_name(c, src) {
            return Some(n);
        }
    }
    None
}

/// The function name from a C/C++ `function_definition` (via its declarator).
fn c_fn_name(node: Node, src: &str) -> Option<String> {
    node.child_by_field_name("declarator")
        .and_then(|d| descend_name(d, src))
}

/// Extract definitions with scope. Returns `None` for unsupported languages or a
/// parse failure, so the caller can fall back to the heuristic.
#[must_use]
pub fn symbols(lang: &str, source: &str) -> Option<Vec<TsSymbol>> {
    let tree = parse(lang, source)?;
    let mut out = Vec::new();
    collect_symbols(lang, tree.root_node(), source, None, &mut out);
    Some(out)
}

#[allow(clippy::too_many_lines)]
fn collect_symbols(
    lang: &str,
    node: Node,
    src: &str,
    scope: Option<&str>,
    out: &mut Vec<TsSymbol>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let kind = child.kind();
        // (definition?, name, scope introduced for this subtree)
        let mut new_scope: Option<String> = scope.map(str::to_owned);
        let mut def: Option<(&'static str, String, Option<String>)> = None;
        match lang {
            "rust" => match kind {
                "function_item" => {
                    if let Some(n) = name_field(child, src) {
                        def = Some(("fn", n, scope.map(str::to_owned)));
                    }
                }
                "struct_item" => def = name_field(child, src).map(|n| ("struct", n, None)),
                "enum_item" => def = name_field(child, src).map(|n| ("enum", n, None)),
                "trait_item" => def = name_field(child, src).map(|n| ("trait", n, None)),
                "type_item" => def = name_field(child, src).map(|n| ("type", n, None)),
                "const_item" | "static_item" => {
                    def = name_field(child, src).map(|n| ("const", n, None));
                }
                "macro_definition" => def = name_field(child, src).map(|n| ("macro", n, None)),
                "impl_item" => {
                    new_scope = child
                        .child_by_field_name("type")
                        .map(|t| text(t, src).to_owned());
                }
                _ => {}
            },
            "python" => match kind {
                "function_definition" => {
                    if let Some(n) = name_field(child, src) {
                        def = Some(("fn", n, scope.map(str::to_owned)));
                    }
                }
                "class_definition" => {
                    if let Some(n) = name_field(child, src) {
                        new_scope = Some(n.clone());
                        def = Some(("class", n, None));
                    }
                }
                _ => {}
            },
            "javascript" | "typescript" | "tsx" => match kind {
                "function_declaration" | "generator_function_declaration" => {
                    if let Some(n) = name_field(child, src) {
                        def = Some(("fn", n, scope.map(str::to_owned)));
                    }
                }
                "method_definition" => {
                    def = name_field(child, src).map(|n| ("fn", n, scope.map(str::to_owned)));
                }
                "class_declaration" => {
                    if let Some(n) = name_field(child, src) {
                        new_scope = Some(n.clone());
                        def = Some(("class", n, None));
                    }
                }
                "interface_declaration" => {
                    def = name_field(child, src).map(|n| ("interface", n, None));
                }
                "type_alias_declaration" => def = name_field(child, src).map(|n| ("type", n, None)),
                "enum_declaration" => def = name_field(child, src).map(|n| ("enum", n, None)),
                "lexical_declaration" | "variable_declaration" => {
                    // `const foo = (…) => …` / `const foo = function …`
                    let mut c2 = child.walk();
                    for d in child.named_children(&mut c2) {
                        if d.kind() == "variable_declarator" {
                            let is_fn = d.child_by_field_name("value").is_some_and(|v| {
                                matches!(
                                    v.kind(),
                                    "arrow_function" | "function" | "function_expression"
                                )
                            });
                            if is_fn {
                                if let Some(n) = name_field(d, src) {
                                    out.push(TsSymbol {
                                        name: n,
                                        kind: "fn",
                                        scope: scope.map(str::to_owned),
                                        line: d.start_position().row + 1,
                                    });
                                }
                            }
                        }
                    }
                }
                _ => {}
            },
            "go" => match kind {
                "function_declaration" => {
                    if let Some(n) = name_field(child, src) {
                        def = Some(("fn", n, None));
                    }
                }
                "method_declaration" => {
                    // Receiver type is the scope: `func (r *T) M()` → scope T.
                    let recv = child.child_by_field_name("receiver").map(|r| {
                        text(r, src)
                            .trim_matches(['(', ')', ' '])
                            .rsplit([' ', '*'])
                            .next()
                            .unwrap_or("")
                            .to_owned()
                    });
                    def = name_field(child, src).map(|n| ("fn", n, recv));
                }
                "type_declaration" => {
                    let mut c2 = child.walk();
                    for spec in child.named_children(&mut c2) {
                        if spec.kind() == "type_spec" {
                            let n = name_field(spec, src)
                                .or_else(|| spec.named_child(0).map(|x| text(x, src).to_owned()));
                            let k = spec.child_by_field_name("type").map_or("type", |t| {
                                match t.kind() {
                                    "struct_type" => "struct",
                                    "interface_type" => "interface",
                                    _ => "type",
                                }
                            });
                            if let Some(n) = n {
                                out.push(TsSymbol {
                                    name: n,
                                    kind: k,
                                    scope: None,
                                    line: spec.start_position().row + 1,
                                });
                            }
                        }
                    }
                }
                _ => {}
            },
            "java" | "csharp" => match kind {
                "method_declaration" | "constructor_declaration" => {
                    def = name_field(child, src).map(|n| ("fn", n, scope.map(str::to_owned)));
                }
                "class_declaration" | "record_declaration" => {
                    if let Some(n) = name_field(child, src) {
                        new_scope = Some(n.clone());
                        def = Some(("class", n, None));
                    }
                }
                "struct_declaration" => {
                    if let Some(n) = name_field(child, src) {
                        new_scope = Some(n.clone());
                        def = Some(("struct", n, None));
                    }
                }
                "interface_declaration" => {
                    def = name_field(child, src).map(|n| ("interface", n, None));
                }
                "enum_declaration" => def = name_field(child, src).map(|n| ("enum", n, None)),
                _ => {}
            },
            "swift" => match kind {
                "function_declaration" => {
                    def = name_field(child, src).map(|n| ("fn", n, scope.map(str::to_owned)));
                }
                "protocol_declaration" => {
                    def = name_field(child, src).map(|n| ("interface", n, None));
                }
                // Swift uses `class_declaration` for class/struct/enum/actor —
                // read the leading keyword for an accurate label + set scope.
                "class_declaration" => {
                    if let Some(n) = name_field(child, src) {
                        let kw = child
                            .child(0)
                            .map(|c| text(c, src))
                            .and_then(|k| match k {
                                "struct" => Some("struct"),
                                "enum" => Some("enum"),
                                "actor" => Some("class"),
                                _ => None,
                            })
                            .unwrap_or("class");
                        new_scope = Some(n.clone());
                        def = Some((kw, n, None));
                    }
                }
                _ => {}
            },
            "c" | "cpp" => match kind {
                "function_definition" => {
                    def = c_fn_name(child, src).map(|n| ("fn", n, scope.map(str::to_owned)));
                }
                "class_specifier" => {
                    if let Some(n) = name_field(child, src) {
                        new_scope = Some(n.clone());
                        def = Some(("class", n, None));
                    }
                }
                "struct_specifier" => def = name_field(child, src).map(|n| ("struct", n, None)),
                "enum_specifier" => def = name_field(child, src).map(|n| ("enum", n, None)),
                _ => {}
            },
            "ruby" => match kind {
                "method" | "singleton_method" => {
                    def = name_field(child, src).map(|n| ("fn", n, scope.map(str::to_owned)));
                }
                "class" | "module" => {
                    if let Some(n) = name_field(child, src) {
                        new_scope = Some(n.clone());
                        def = Some((if kind == "module" { "module" } else { "class" }, n, None));
                    }
                }
                _ => {}
            },
            "php" => match kind {
                "method_declaration" | "function_definition" => {
                    def = name_field(child, src).map(|n| ("fn", n, scope.map(str::to_owned)));
                }
                "class_declaration" => {
                    if let Some(n) = name_field(child, src) {
                        new_scope = Some(n.clone());
                        def = Some(("class", n, None));
                    }
                }
                "interface_declaration" => {
                    def = name_field(child, src).map(|n| ("interface", n, None));
                }
                "trait_declaration" => def = name_field(child, src).map(|n| ("trait", n, None)),
                "enum_declaration" => def = name_field(child, src).map(|n| ("enum", n, None)),
                _ => {}
            },
            _ => {}
        }
        if let Some((k, n, sc)) = def {
            out.push(TsSymbol {
                name: n,
                kind: k,
                scope: sc,
                line: child.start_position().row + 1,
            });
        }
        collect_symbols(lang, child, src, new_scope.as_deref(), out);
    }
}

/// One call site: `caller` (with its `caller_scope`) invokes `callee`.
pub struct TsCall {
    pub caller: String,
    pub caller_scope: Option<String>,
    pub callee: String,
    pub line: usize,
}

/// Extract the call graph (function → function it calls). `None` for
/// unsupported languages / parse failure.
#[must_use]
pub fn calls(lang: &str, source: &str) -> Option<Vec<TsCall>> {
    let tree = parse(lang, source)?;
    let mut out = Vec::new();
    collect_calls(lang, tree.root_node(), source, None, None, &mut out);
    Some(out)
}

/// The trailing identifier of a callee expression: `self.build` → `build`,
/// `Mod::run` → `run`, `pkg.Fn` → `Fn`, `foo` → `foo`.
fn last_ident(s: &str) -> String {
    let rev: String = s
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    rev.chars().rev().collect()
}

/// The type/class a node introduces as a scope (rust impl, py/js/ts class).
#[allow(clippy::unnested_or_patterns)] // per-language (lang, kind) pairs read clearer flat
fn type_scope_intro(lang: &str, node: Node, src: &str) -> Option<String> {
    match (lang, node.kind()) {
        ("rust", "impl_item") => node
            .child_by_field_name("type")
            .map(|t| text(t, src).to_owned()),
        ("python", "class_definition")
        | ("javascript" | "typescript" | "tsx" | "swift", "class_declaration")
        | ("php", "class_declaration")
        | ("cpp", "class_specifier")
        | ("ruby", "class" | "module")
        | ("java" | "csharp", "class_declaration" | "struct_declaration" | "record_declaration") => {
            name_field(node, src)
        }
        _ => None,
    }
}

/// If `node` is a function/method definition, its `(name, scope)`.
#[allow(clippy::unnested_or_patterns)] // per-language (lang, kind) pairs read clearer flat
fn fn_identity(
    lang: &str,
    node: Node,
    src: &str,
    type_scope: Option<&str>,
) -> Option<(String, Option<String>)> {
    let scoped = |n: String| (n, type_scope.map(str::to_owned));
    match (lang, node.kind()) {
        ("rust", "function_item")
        | ("python", "function_definition")
        | ("swift", "function_declaration")
        | ("ruby", "method" | "singleton_method")
        | ("php", "method_declaration" | "function_definition")
        | ("javascript" | "typescript" | "tsx", "function_declaration" | "method_definition")
        | ("java" | "csharp", "method_declaration" | "constructor_declaration") => {
            name_field(node, src).map(scoped)
        }
        ("c" | "cpp", "function_definition") => c_fn_name(node, src).map(scoped),
        ("go", "function_declaration") => name_field(node, src).map(|n| (n, None)),
        ("go", "method_declaration") => {
            let recv = node.child_by_field_name("receiver").map(|r| {
                text(r, src)
                    .trim_matches(['(', ')', ' '])
                    .rsplit([' ', '*'])
                    .next()
                    .unwrap_or("")
                    .to_owned()
            });
            name_field(node, src).map(|n| (n, recv))
        }
        _ => None,
    }
}

/// If `node` is a call/macro invocation, its `(callee, line)`.
fn call_target(lang: &str, node: Node, src: &str) -> Option<(String, usize)> {
    let line = node.start_position().row + 1;
    let callee = match node.kind() {
        "call_expression"
            if matches!(lang, "rust" | "javascript" | "typescript" | "tsx" | "go") =>
        {
            node.child_by_field_name("function").map(|f| text(f, src))
        }
        "call" if lang == "python" => node.child_by_field_name("function").map(|f| text(f, src)),
        "macro_invocation" if lang == "rust" => {
            node.child_by_field_name("macro").map(|m| text(m, src))
        }
        "method_invocation" if lang == "java" => {
            node.child_by_field_name("name").map(|n| text(n, src))
        }
        "invocation_expression" if lang == "csharp" => {
            node.child_by_field_name("function").map(|f| text(f, src))
        }
        // Swift: `(call_expression (simple_identifier) (call_suffix …))`.
        "call_expression" if lang == "swift" => node.named_child(0).map(|f| text(f, src)),
        "call_expression" if matches!(lang, "c" | "cpp") => {
            node.child_by_field_name("function").map(|f| text(f, src))
        }
        "call" if lang == "ruby" => node.child_by_field_name("method").map(|m| text(m, src)),
        "function_call_expression" | "member_call_expression" | "scoped_call_expression"
            if lang == "php" =>
        {
            node.child_by_field_name("function")
                .or_else(|| node.child_by_field_name("name"))
                .map(|f| text(f, src))
        }
        _ => None,
    }?;
    let name = last_ident(callee);
    (!name.is_empty()).then_some((name, line))
}

fn collect_calls(
    lang: &str,
    node: Node,
    src: &str,
    type_scope: Option<&str>,
    cur: Option<&(String, Option<String>)>,
    out: &mut Vec<TsCall>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some((callee, line)) = call_target(lang, child, src) {
            if let Some((cn, cs)) = cur {
                out.push(TsCall {
                    caller: cn.clone(),
                    caller_scope: cs.clone(),
                    callee,
                    line,
                });
            }
        }
        let child_scope = type_scope_intro(lang, child, src);
        let next_scope = child_scope.as_deref().or(type_scope);
        let this_fn = fn_identity(lang, child, src, type_scope);
        let next_cur = this_fn.as_ref().or(cur);
        collect_calls(lang, child, src, next_scope, next_cur, out);
    }
}

/// Lines where `name` occurs as an identifier (not in a comment or string).
/// Returns `None` for unsupported languages / parse failure.
#[must_use]
pub fn reference_lines(lang: &str, source: &str, name: &str) -> Option<Vec<usize>> {
    let tree = parse(lang, source)?;
    let mut lines = Vec::new();
    collect_refs(tree.root_node(), source, name, &mut lines);
    lines.sort_unstable();
    lines.dedup();
    Some(lines)
}

const IDENT_KINDS: &[&str] = &[
    "identifier",
    "type_identifier",
    "field_identifier",
    "property_identifier",
    "shorthand_property_identifier",
    "package_identifier",
    "simple_identifier", // Swift
    "constant",          // Ruby
    "name",              // PHP
];

fn collect_refs(node: Node, src: &str, name: &str, out: &mut Vec<usize>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if IDENT_KINDS.contains(&child.kind()) && text(child, src) == name {
            out.push(child.start_position().row + 1);
        }
        collect_refs(child, src, name, out);
    }
}
