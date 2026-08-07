//! Shared parsing of the agent "proposed item" JSON contract used by BA
//! (features) and TEST (bugs). One schema, one tolerant parser.

use coxagent_domain::{Complexity, Priority};
use serde::Deserialize;

/// A ticket proposed by an agent (a feature from BA, a bug from TEST).
#[derive(Debug, Clone, Deserialize)]
pub struct ProposedItem {
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub priority: Priority,
    pub complexity: Complexity,
    #[serde(default)]
    pub has_ui: bool,
    /// Up to 5 acceptance criteria the agent proposes as the "definition of done".
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
}

/// Extract the outermost JSON array from engine output, tolerating surrounding
/// prose and markdown code fences, and parse it into proposed items.
///
/// Strategy: find the first `[`, then scan forward through every subsequent
/// `]`, attempting to parse each candidate slice as a complete document. The
/// first candidate that parses in full wins — this naturally handles nested
/// arrays (e.g. `acceptance_criteria` inside each item object) as well as
/// trailing prose after the real array.
///
/// # Errors
/// Returns a message when no array is present or no candidate slice parses.
pub fn parse_items(raw: &str) -> Result<Vec<ProposedItem>, String> {
    let stripped = strip_code_fences(raw);
    // Prefer the prose-trimmed body; fall back to the full stripped text in case
    // trailing-trimming discarded content.
    for body in [trim_trailing_prose(&stripped), stripped] {
        if !body.contains('[') || !body.contains(']') {
            continue;
        }
        if let Some(candidate) = first_valid_slice(&body) {
            return serde_json::from_str::<Vec<ProposedItem>>(&candidate)
                .map_err(|e| e.to_string());
        }
    }
    Err("no JSON array found".to_owned())
}

/// If `raw` is wrapped in a markdown fenced code block (```json … ``` or a bare
/// ```), strip just the fence marker lines so bracketed content can be scanned.
fn strip_code_fences(raw: &str) -> String {
    let lines: Vec<&str> = raw.lines().collect();
    if lines.len() < 2 {
        return raw.to_owned();
    }
    let head_is_fence = lines[0].trim_start().starts_with("```");
    let tail_is_fence = lines.last().is_some_and(|l| l.trim().ends_with("```"));
    if head_is_fence && tail_is_fence {
        lines[1..lines.len() - 1].join("\n")
    } else {
        raw.to_owned()
    }
}

/// Drop non-JSON prose that trails a complete JSON document. Only text after the
/// final closing brace/bracket of a structurally valid suffix is removed; we
/// never touch interior content.
fn trim_trailing_prose(s: &str) -> String {
    match (s.rfind(']'), s.rfind('}')) {
        (Some(ab), Some(cb)) => s[..=ab.max(cb)].to_owned(),
        (Some(ab), None) => s[..=ab].to_owned(),
        (None, Some(cb)) => s[..=cb].to_owned(),
        (None, None) => s.to_owned(),
    }
}

/// Scan `s` from its first `[`, and for every subsequent `]` try parsing the
/// whole slice as a JSON array. Returns the first candidate whose full slice is
/// valid — handling nested arrays and trailing prose. Returns `None` when no
/// candidate parses.
fn first_valid_slice(s: &str) -> Option<String> {
    let start = s.find('[')?;
    // Walk forward from the first '[', and at every ']' try parsing the whole
    // slice up to and including it as an array. The first full parse wins; this
    // skips nested arrays (e.g. acceptance_criteria) and stops before trailing
    // prose, because those candidates fail a whole-document parse.
    for end in s
        .char_indices()
        .filter(|(i, c)| *c == ']' && *i > start)
        .map(|(i, _)| i)
    {
        let candidate = &s[start..=end];
        if serde_json::from_str::<serde_json::Value>(candidate).is_ok_and(|v| v.is_array()) {
            return Some(candidate.to_owned());
        }
    }
    None
}

/// Normalise a ticket title for duplicate detection: lowercase, keep only
/// alphanumerics, collapse runs to single spaces. So "Key Verification (Safety
/// Numbers)" and "key verification safety numbers" compare equal.
#[must_use]
pub fn normalize_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut prev_space = false;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.extend(c.to_lowercase());
            prev_space = false;
        } else if !prev_space && !out.is_empty() {
            out.push(' ');
            prev_space = true;
        }
    }
    out.trim().to_owned()
}

/// Content tokens of a title for semantic duplicate detection: normalise, split
/// on spaces, and drop short stop-words so "Delivery and read receipts" and
/// "Read receipts for delivery" share `{delivery, read, receipts}`.
#[must_use]
pub fn title_tokens(title: &str) -> std::collections::HashSet<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "with", "a", "an", "of", "to", "in", "on", "per", "via", "your",
        "our", "support", "feature", "add", "enable",
    ];
    normalize_title(title)
        .split(' ')
        .filter(|w| w.len() > 2 && !STOP.contains(w))
        .map(ToOwned::to_owned)
        .collect()
}

/// Jaccard similarity (|A∩B| / |A∪B|) of two token sets, in `0.0..=1.0`. Empty
/// sets are treated as dissimilar (`0.0`) so blank titles never match.
#[must_use]
#[allow(clippy::implicit_hasher, clippy::cast_precision_loss)]
pub fn jaccard<S: std::hash::BuildHasher>(
    a: &std::collections::HashSet<String, S>,
    b: &std::collections::HashSet<String, S>,
) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    }
}

/// Extract the outermost JSON array of strings (e.g. acceptance criteria),
/// tolerating surrounding prose.
///
/// # Errors
/// Returns a message when no array is present or the JSON is malformed.
pub fn parse_string_list(raw: &str) -> Result<Vec<String>, String> {
    let start = raw.find('[').ok_or("no JSON array found")?;
    let end = raw.rfind(']').ok_or("no closing bracket")?;
    if end < start {
        return Err("malformed array bounds".to_owned());
    }
    serde_json::from_str(&raw[start..=end]).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jaccard_flags_paraphrased_titles() {
        let a = title_tokens("Delivery and read receipts");
        let b = title_tokens("Read receipts for delivery");
        assert!(jaccard(&a, &b) >= 0.6, "paraphrase should be a near-dupe");
        let c = title_tokens("Disappearing messages timer");
        assert!(jaccard(&a, &c) < 0.6, "unrelated titles stay distinct");
    }

    #[test]
    fn title_tokens_drops_stopwords() {
        let t = title_tokens("Add support for group chats");
        assert!(t.contains("group") && t.contains("chats"));
        assert!(!t.contains("add") && !t.contains("for") && !t.contains("support"));
    }

    #[test]
    fn parses_array_amid_prose() {
        let raw = r#"thinking... [{"title":"X","priority":"low","complexity":"small"}] done"#;
        let items = parse_items(raw).expect("parse");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "X");
        assert!(!items[0].has_ui);
    }

    #[test]
    fn empty_array_is_ok() {
        assert!(parse_items("[]").expect("parse").is_empty());
    }

    #[test]
    fn no_array_errors() {
        assert!(parse_items("nope").is_err());
    }

    #[test]
    fn parses_fenced_json_block() {
        let raw = "```json\n[\n  {\"title\":\"Login\",\"priority\":\"high\",\"complexity\":\"medium\"}\n]\n```";
        let items = parse_items(raw).expect("parse fenced block");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Login");
    }

    #[test]
    fn balanced_close_bracket_when_trailing_prose_follows() {
        // Inner item object closes with '}' then the outer array with ']', and
        // prose trails both — the parser must stop at the real outer bracket.
        let raw = r#"Here is my analysis. [{"title":"X","priority":"low","complexity":"small","acceptance_criteria":["a","b"]}] And that's all."#;
        let items = parse_items(raw).expect("parse with trailing prose");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "X");
    }

    #[test]
    fn nested_acceptance_criteria_arrays_still_parse() {
        let raw = r#"[{"title":"Export","priority":"high","complexity":"large","acceptance_criteria":["csv downloads","headers correct"]}]"#;
        let items = parse_items(raw).expect("parse nested arrays");
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].acceptance_criteria,
            vec!["csv downloads".to_owned(), "headers correct".to_owned()]
        );
    }
}
