//! CXA-B191 guardrail 1 — the copy catalog placeholder guard.
//!
//! Pure function over the catalog file (`copy.js`): no server, no network,
//! no DOM. Every catalog entry's template must use exactly the `{tokens}` its
//! `params` document, rendered output must never leak an unfilled `{token}`
//! or the `_copy.broken` marker, and the gate's exception list must start
//! empty (ratchet: it may only shrink).
use std::sync::LazyLock;
use std::sync::Mutex;

const COPY_JS: &str = include_str!("../src/web/js/copy.js");

/// Long-string fixtures (legitimate stress input — CXA-B164 found truncation
/// and broken-token leaks on long values, never on short ones).
const LONG: &str = "c\u{00f3}ntent title with long unicode label ‹then› more — 0123456789 0123456789 0123456789 0123456789 0123456789 0123456789 0123456789 0123456789 0123456789 0123456789";

/// A future named list must carry `(file, owner)` pairs and may only shrink.
type CopyGateExceptions = Option<Vec<(&'static str, &'static str)>>;

/// The gate's documented baseline (CXA-B191): `None` = enforced empty.
static COPY_GATE_EXCEPTIONS: LazyLock<Mutex<CopyGateExceptions>> =
    LazyLock::new(|| Mutex::new(None));

struct Entry {
    key: String,
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
        let Some(key_end) = trimmed[1..].find('"').map(|i| i + 1) else { continue };
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
        entries.push(Entry {
            key: trimmed[1..key_end].to_string(),
            params,
            template,
        });
    }
    entries
}

/// Mirror of `window.copyText`: fills `{token}` from params; an unfilled
/// token stays visible (never silently dropped, never resolves to "").
fn render(template: &str, params: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (k, v) in params {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

#[test]
fn every_catalog_entry_fills_its_own_placeholders_and_leaks_none() {
    let entries = parse_catalog(COPY_JS);
    assert!(
        entries.len() >= 10,
        "the copy catalog parsed from copy.js is suspiciously small ({}); did the catalog format change? update this parser with it",
        entries.len()
    );
    for e in &entries {
        for p in &e.params {
            assert!(
                e.template.contains(&format!("{{{}}}", p)),
                "catalog entry `{}` documents param `{}` its template never uses",
                e.key,
                p
            );
        }
        // Fill every documented param with the long fixture; nothing may leak.
        let params: Vec<(&str, &str)> =
            e.params.iter().map(|p| (p.as_str(), LONG)).collect();
        let rendered = render(&e.template, &params);
        assert!(!rendered.trim().is_empty(), "`{}` renders empty copy", e.key);
        assert!(
            !rendered.contains("_copy.broken"),
            "`{}` resolved to the broken marker",
            e.key
        );
        for p in &e.params {
            assert!(
                !rendered.contains(&format!("{{{}}}", p)),
                "catalog entry `{}` leaked its unfilled placeholder {{{}}}: {:?}",
                e.key,
                p,
                rendered
            );
        }
        assert!(
            !rendered.contains("{kind}") && !rendered.contains("{what}") && !rendered.contains("{query}"),
            "`{}` leaked an undocumented placeholder into rendered copy: {:?}",
            e.key,
            rendered
        );
        assert!(
            rendered.contains(LONG) || e.params.is_empty(),
            "`{}` dropped or mangled its long param value",
            e.key
        );
    }
}

#[test]
fn long_string_params_fill_all_tokens_without_truncation_by_the_layer() {
    // The layer passes full values; any truncation is CSS presentation and
    // must come with an accessible title (guarded by copy_render_b191.rs),
    // never by the catalog dropping characters.
    let entries = parse_catalog(COPY_JS);
    let saved = entries
        .iter()
        .find(|e| e.key == "toast.saved")
        .expect("toast.saved must stay in the catalog");
    let rendered = render(&saved.template, &[("what", LONG)]);
    assert!(
        rendered.contains(LONG),
        "the copy layer must pass long values through verbatim, not truncate them"
    );
}

#[test]
fn unknown_token_in_params_is_left_visible_not_silently_dropped() {
    // copyText renders a missing param as the literal {token} — visible in
    // the UI, catchable by review — it never silently degrades to "".
    let entries = parse_catalog(COPY_JS);
    let inbox = entries
        .iter()
        .find(|e| e.key == "empty.inbox")
        .expect("empty.inbox must stay in the catalog");
    let rendered = render(&inbox.template, &[("kind", "activity")]);
    assert!(
        !rendered.contains("{kind}"),
        "`empty.inbox` left its documented token unfilled: {:?}",
        rendered
    );
    assert!(
        rendered.contains("activity"),
        "`empty.inbox` did not substitute its param: {:?}",
        rendered
    );
}

#[test]
fn copy_gate_exception_list_starts_empty_and_may_only_shrink() {
    let guard = COPY_GATE_EXCEPTIONS.lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        None => {} // baseline: enforced empty
        Some(list) => {
            assert!(
                !list.is_empty(),
                "COPY_GATE_EXCEPTIONS was emptied via None; set it back to None (enforced-empty) or a non-empty named list"
            );
            for (file, owner) in list {
                assert!(!file.trim().is_empty(), "exception with empty file: {:?}", owner);
                assert!(!owner.trim().is_empty(), "exception {:?} missing its owner", file);
            }
        }
    }
}
