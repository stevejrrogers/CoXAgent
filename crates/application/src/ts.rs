//! Tree-sitter parsing for the code graph — accurate, scope-aware symbol and
//! reference extraction that the line/regex heuristic can't match. Supports
//! Rust, Python, JavaScript, TypeScript and Go; other languages fall back to the
//! heuristic. Never executes code — it only parses.

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
        "go" => tree_sitter_go::language(),
        _ => return None,
    })
}

/// Whether this language is parsed by tree-sitter (vs the heuristic fallback).
#[must_use]
pub fn supported(lang: &str) -> bool {
    matches!(lang, "rust" | "python" | "javascript" | "typescript" | "go")
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
            "javascript" | "typescript" => match kind {
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
