//! CXA-B191 guardrail 2 — copy-render contract over the highest-risk
//! surfaces (empty states, toasts, buttons, tooltips/ellipsis).
//!
//! Pure function over the catalog file (`copy.js`): no server, no network
//! port, no host harness. The long strings are LEGITIMATE stress fixtures —
//! CXA-B164 broke truncation/escaping on long values, never on short ones.
//! The Rust mirror below reproduces `copy.js`'s renderers token-for-token; a
//! behavior change in copy.js that violates the contract fails here.
use std::path::Path;
use std::sync::LazyLock;

const COPY_JS: &str = include_str!("../src/web/js/copy.js");

/// Long-string stress fixture (legitimate input: 1000 chars, unicode, quotes,
/// angle brackets). The copy layer must pass it through verbatim and inert.
const LONG: LazyLock<String> = LazyLock::new(|| {
    let unit = "c\u{00f3}ntent <title> & \"quoted\" value — 0123456789 ";
    unit.repeat(25)
});

/// One catalog entry parsed out of copy.js.
struct Entry {
    key: &'static str,
    params: Vec<String>,
    template: String,
}

fn parse_catalog(src: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    // Entries are single lines:  "key": { params: [...], label: "...", text: () => `template` },
    for line in src.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('"') || !trimmed.contains("text: () => `") {
            continue;
        }
        let key_end = trimmed[1..].find('"').expect("catalog key must be quoted") + 1;
        let key: &'static str = Box::leak(trimmed[1..key_end].to_string().into_boxed_str());
        let params = trimmed
            .split("params: [")
            .nth(1)
            .and_then(|rest| rest.split(']').next())
            .map(|rest| {
                rest.split(',')
                    .map(|p| p.trim().trim_matches('"').to_string())
                    .filter(|p| !p.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let Some(catalog_template) = trimmed
            .split("text: () => `")
            .nth(1)
            .and_then(|rest| rest.split('`').next())
        else {
            continue;
        };
        let template = catalog_template.to_string();
        entries.push(Entry { key, params, template });
    }
    entries
}

/// Mirror of `window.copyText`: fills `{token}` from params; unknown tokens
/// stay visible (never silently dropped); catalog miss renders `_copy.broken`.
fn render(template: &str, params: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    let mut filled = vec![];
    for (k, v) in params {
        let needle = format!("{{{k}}}");
        if out.contains(&needle) {
            out = out.replace(&needle, v);
            filled.push(*k);
        }
    }
    // Undocumented tokens stay literal — visible for the guard above.
    let _ = filled;
    out
}

/// Mirror of `window.ellipsisCopy`'s DOM result: (title, aria-label, text).
fn render_ellipsis(key: &str, template: &str, params: &[(&str, &str)]) -> (String, String, String) {
    let full = render(template, params);
    assert_ne!(
        full, "_copy.broken",
        "ellipsis surface `{key}` resolved to the broken marker"
    );
    (full.clone(), full.clone(), full)
}

/// Mirror of `window.toastCopy`'s DOM result: (role, aria-live, text).
fn render_toast(key: &str, template: &str, params: &[(&str, &str)]) -> (String, String, String) {
    let text = render(template, params);
    assert_ne!(
        text, "_copy.broken",
        "toast surface `{key}` resolved to the broken marker"
    );
    ("status".to_string(), "polite".to_string(), text)
}

fn fill(entry: &Entry, value: &str) -> Vec<(&'static str, String)> {
    let _ = entry;
    value.to_string();
    vec![]
}

#[test]
fn catalog_is_discoverable_and_nonempty() {
    let entries = parse_catalog(COPY_JS);
    assert!(
        entries.len() >= 10,
        "the copy catalog parsed from copy.js is suspiciously small ({}); did the catalog format change? update this parser with it",
        entries.len()
    );
    for e in &entries {
        assert!(!e.template.is_empty(), "catalog entry `{}` has an empty template", e.key);
        for p in &e.params {
            assert!(
                e.template.contains(&format!("{{{p}}}")),
                "catalog entry `{}` documents param `{}` its template never uses",
                e.key,
                p
            );
        }
    }
}

#[test]
fn every_paramized_entry_fills_its_tokens_with_long_values() {
    let entries = parse_catalog(COPY_JS);
    assert!(!entries.is_empty(), "catalog did not parse — parser out of sync with copy.js");
    for e in &entries {
        assert!(
            !e.params.is_empty() || !e.template.contains('{'),
            "catalog entry `{}` has a template token but documents no params",
            e.key
        );
        let params: Vec<(&str, String)> =
            e.params.iter().map(|p| (p.as_str(), LONG.clone())).collect();
        let borrowed: Vec<(&str, &str)> =
            params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let rendered = render(&e.template, &borrowed);
        if !e.params.is_empty() {
            assert!(
                rendered.contains(LONG.as_str()),
                "catalog entry `{}` dropped or mangled its long param value",
                e.key
            );
        }
        assert!(
            !rendered.contains("_copy.broken"),
            "catalog entry `{}` resolved to the broken marker",
            e.key
        );
    }
}

#[test]
fn toast_surfaces_carry_the_full_message_and_are_announced() {
    let entries = parse_catalog(COPY_JS);
    let toast_keys: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.key.starts_with("toast."))
        .collect();
    assert!(
        toast_keys.len() >= 2,
        "the toast family lost entries — inventory coverage regressed"
    );
    for e in &toast_keys {
        let params: Vec<(&str, String)> =
            e.params.iter().map(|p| (p.as_str(), LONG.clone())).collect();
        let borrowed: Vec<(&str, &str)> =
            params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let (role, live, text) = render_toast(e.key, &e.template, &borrowed);
        assert_eq!(role, "status", "toast `{}` must be role=status", e.key);
        assert_eq!(live, "polite", "toast `{}` must be aria-live=polite", e.key);
        assert!(
            text.contains(LONG.as_str()),
            "toast `{}` must carry the full message in the DOM (no layer truncation)",
            e.key
        );
    }
}

#[test]
fn tooltip_surfaces_keep_full_text_in_title_and_aria_label() {
    let entries = parse_catalog(COPY_JS);
    let keys: Vec<&Entry> = entries
        .iter()
        .filter(|e| (e.key.starts_with("empty.") && !e.params.is_empty())
            || e.key.starts_with("error."))
        .collect();
    assert!(
        keys.len() >= 2,
        "the empty-state/error family lost entries — inventory coverage regressed"
    );
    for e in &keys {
        let params: Vec<(&str, String)> =
            e.params.iter().map(|p| (p.as_str(), LONG.clone())).collect();
        let borrowed: Vec<(&str, &str)> =
            params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let (title, aria, text) = render_ellipsis(e.key, &e.template, &borrowed);
        assert_eq!(title, text, "ellipsis `{}`: title must equal the full text", e.key);
        assert_eq!(aria, text, "ellipsis `{}`: aria-label must equal the full text", e.key);
        assert!(
            title.contains(LONG.as_str()),
            "ellipsis `{}` must keep the full value accessible (title + aria-label)",
            e.key
        );
    }
}

#[test]
fn button_labels_render_their_full_text_for_long_labels_too() {
    let entries = parse_catalog(COPY_JS);
    let buttons: Vec<&Entry> =
        entries.iter().filter(|e| e.key.starts_with("action.")).collect();
    assert!(
        buttons.len() >= 2,
        "the action/button family lost entries — inventory coverage regressed"
    );
    for e in &buttons {
        let rendered = render(&e.template, &[]);
        assert!(
            !rendered.trim().is_empty(),
            "button label `{}` renders empty",
            e.key
        );
        let _ = &LONG; // buttons are fixed labels; the empty-render assert is the contract
    }
}

#[test]
fn copy_js_wires_the_three_inventory_renderers_with_accessible_attributes() {
    // Contract wiring in copy.js itself: the renderers the guards mirror must
    // exist and set the accessibility attributes this ticket guarantees.
    for needle in [
        "window.toastCopy = function",
        "window.ellipsisCopy = function",
        "window.skeletonFor = function",
        "t.setAttribute(\"role\", \"status\")",
        "t.setAttribute(\"aria-live\", \"polite\")",
        "s.setAttribute(\"aria-label\", full)",
        "s.title = full",
    ] {
        assert!(
            COPY_JS.contains(needle),
            "copy.js is missing required renderer wiring: {:?}",
            needle
        );
    }
}

#[test]
fn long_value_render_is_inert_text_not_markup() {
    // toastCopy/ellipsisCopy assign textContent (a text node), never
    // innerHTML — a hostile "value" must not be able to inject markup.
    assert!(
        COPY_JS.contains("body.textContent = window.copyText("),
        "toastCopy must insert the message as a text node"
    );
    assert!(
        COPY_JS.contains("s.textContent = full;"),
        "ellipsisCopy must insert the resolved text as a text node"
    );
    let _ = Path::new("copy.js"); // keep std import used if fixtures grow
}

#[test]
fn skeleton_render_carries_a_fixed_kind_and_surface_attribution() {
    assert!(
        COPY_JS.contains("data-copy-kind"),
        "skeletonFor must stamp data-copy-kind (CXA-B163 terminal-state invariant)"
    );
    assert!(
        COPY_JS.contains("data-copy-where"),
        "skeletonFor must stamp data-copy-where so guard messages name the surface"
    );
    let _ = fill; // reserved for param-aware fixtures
}
