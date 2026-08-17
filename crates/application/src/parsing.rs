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

/// Partially salvage corrupt agent output: extract every VALID top-level object
/// individually instead of discarding everything because one bad segment broke
/// an otherwise-good array's whole-document parse.
///
/// LLM output frequently mixes genuinely good items with hallucinated garbage —
/// extra arrays nested between objects, malformed escapes like `{\"...\"}`, token
/// noise — which makes serde reject <i>all</i> of it at once even though most of
/// it is perfectly fine. This scans for every top-level JSON object using balanced-
/// brace matching that respects quoted strings and escape sequences (`\"`, `\\`),
/// keeps each one that deserializes on its own, and drops only what truly breaks.
///
/// # Errors
/// Returns a message when no candidate object parses anywhere in `raw`.
pub fn parse_items_lenient(raw: &str) -> Result<Vec<ProposedItem>, String> {
    let stripped = strip_code_fences(raw);
    let mut valid = Vec::new();
    for obj in top_level_objects(&stripped) {
        if let Ok(item) = serde_json::from_str::<ProposedItem>(&obj) {
            valid.push(item);
        }
    }
    if valid.is_empty() {
        Err("no valid feature items found".to_owned())
    } else {
        Ok(valid)
    }
}

/// Scan `s` and return each TOP-LEVEL JSON object substring (a balanced run of
/// braces at depth zero), never descending into nested arrays or objects. Braces
/// inside quoted string values are ignored, as are escape sequences (`\"`, `\\`)
/// so a backslash-quote never prematurely closes a string. Sibling garbage —
/// stray brackets, tokens, extra malformed segments — simply falls between the
/// returned slices.
fn top_level_objects(s: &str) -> Vec<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escaped = false;
    let mut start: Option<usize> = None;

    for (i, b) in bytes.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if *b == b'\\' {
                escaped = true;
            } else if *b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => {
                if depth == 0 && start.is_none() {
                    start = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(st) = start.take() {
                        out.push(s[st..=i].to_owned());
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// If `raw` is wrapped in a markdown fenced code block (a json-tagged or bare
/// triple-backtick fence), strip just the fence marker lines so bracketed
/// content can be scanned.
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

/// Whether a title is a backlog process/ceremony ticket — "triage the bugs",
/// "burn-down sprint", "stabilization sprint", "backlog grooming". These are
/// the SM/PO's recurring rituals, not features, and the BA kept re-filing a
/// fresh one every cycle (six near-identical "bug triage / burndown sprint"
/// tickets piled into one inbox). At most ONE should ever be open at a time,
/// so this lets the caller keep a single active slot for the whole family
/// regardless of how the wording drifts.
#[must_use]
pub fn is_backlog_meta(title: &str) -> bool {
    let t = normalize_title(title);
    let ritual = ["triage", "burndown", "burn down", "stabiliz", "grooming"]
        .iter()
        .any(|k| t.contains(k));
    let about_backlog = ["bug", "backlog", "sprint"].iter().any(|k| t.contains(k));
    ritual && about_backlog
}

/// Whether `title` duplicates one of `existing` — either an exact normalised
/// match or a near-paraphrase (Jaccard ≥ 0.6 on content tokens), or, for a
/// backlog-ceremony ticket, any existing ceremony ticket at all. One place so
/// the BA insert loop and any future caller agree on what "already covered"
/// means.
#[must_use]
pub fn duplicates_existing(title: &str, existing: &[String]) -> bool {
    let norm = normalize_title(title);
    if norm.is_empty() {
        return true; // a blank title is never worth filing
    }
    let meta = is_backlog_meta(title);
    let toks = title_tokens(title);
    existing.iter().any(|e| {
        normalize_title(e) == norm
            || (meta && is_backlog_meta(e))
            || jaccard(&toks, &title_tokens(e)) >= 0.6
    })
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

/// The structured TEST response — a list of discovered bugs plus a per-acceptance-
/// criterion verdict for every shipped ticket it verified.
#[derive(Debug, Clone, Deserialize)]
pub struct TestOutput {
    #[serde(default)]
    pub bugs: Vec<ProposedItem>,
    #[serde(default)]
    pub verdicts: Vec<TestVerdict>,
}

/// One acceptance-criterion verdict from the TEST agent.
#[derive(Debug, Clone, Deserialize)]
pub struct TestVerdict {
    /// The EXACT acceptance-criterion text this verdict is for. The system
    /// matches it word-for-word against the ticket's test cases.
    #[serde(default)]
    pub ac: String,
    #[serde(default)]
    pub passed: bool,
    /// One line of concrete evidence (command/request + actual response).
    #[serde(default)]
    pub note: String,
    /// URL path that demonstrates this criterion (per-case screenshot target),
    /// or empty when none applies.
    #[serde(default)]
    pub route: String,
}

/// Parse the TEST engine output into (bugs, verdicts). Accepts BOTH the new
/// `{"bugs":[…], "verdicts":[…]}` object and the legacy bare bug array — the
/// legacy form yields an empty verdict list. Lenient: prose/code fences are
/// tolerated, and a malformed `verdicts` array still yields the bugs.
///
/// # Errors
/// Returns a message when neither form yields any parseable bugs.
pub fn parse_test_output(raw: &str) -> Result<(Vec<ProposedItem>, Vec<TestVerdict>), String> {
    let stripped = strip_code_fences(raw);
    // New object format first: a complete {bugs, verdicts} document.
    for obj in top_level_objects(&stripped) {
        if obj.contains("\"bugs\"") || obj.contains("\"verdicts\"") {
            if let Ok(out) = serde_json::from_str::<TestOutput>(&obj) {
                return Ok((out.bugs, out.verdicts));
            }
        }
    }
    // Legacy bare bug array (or an object whose `bugs` couldn't parse) — the
    // tolerant array parser still recovers the bugs; verdicts are simply none.
    let bugs = parse_items(&stripped)?;
    Ok((bugs, Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recurring_bug_ceremony_family_files_once() {
        // The exact six that piled into one inbox — different wording, one idea.
        let titles = [
            "Bug triage and burn-down allocation (40% capacity)",
            "Bug burndown cadence — Q3 backlog triage",
            "COX-BUG: Sprint bug triage and critical burn-down",
            "Bug stabilization sprint (2 weeks)",
            "Bug burndown sprint — data loss + crash fixes",
        ];
        for t in titles {
            assert!(is_backlog_meta(t), "{t} should read as a ceremony ticket");
        }
        // Once one is on the board, every later paraphrase is a duplicate.
        let board = vec![titles[0].to_owned()];
        for t in &titles[1..] {
            assert!(
                duplicates_existing(t, &board),
                "{t} should collapse onto the existing ceremony ticket"
            );
        }
    }

    #[test]
    fn genuinely_different_features_are_not_dupes() {
        let board = vec!["Bug triage and burn-down sprint".to_owned()];
        assert!(!duplicates_existing(
            "Add CORS + rate-limiting middleware",
            &board
        ));
        assert!(!duplicates_existing("Per-engine cost leaderboard", &board));
        // A real feature that merely mentions "bug" is not a ceremony ticket.
        assert!(!is_backlog_meta("Fix the avatar upload bug"));
    }

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

    #[test]
    fn lenient_salvages_good_when_one_neighbor_is_garbage() {
        // One good object followed by malformed garbage that would break a
        // whole-array parse; only the valid item must survive.
        let raw = r#"[
          {"title":"Login","priority":"high","complexity":"medium"},
          {"title":
        "#;
        let items = parse_items_lenient(raw).expect("salvage good item");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Login");
    }

    #[test]
    fn lenient_nested_acceptance_criteria_not_mis_split() {
        // The nested array inside acceptance_criteria must not be treated as a
        // sibling top-level object; only one item should come out.
        let raw = r#"[{"title":"Export","priority":"high","complexity":"large","acceptance_criteria":["a","b"]}]"#;
        let items = parse_items_lenient(raw).expect("parse nested arrays");
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].acceptance_criteria,
            vec!["a".to_owned(), "b".to_owned()]
        );
    }

    #[test]
    fn lenient_handles_braces_and_escaped_quotes_in_strings() {
        // Braces inside a description and an escaped quote must not break the
        // object's brace balancing.
        let raw = r#"[{"title":"A","priority":"low","complexity":"small","description": "has {json} and \"quote\" here"}, {"title":"B","priority":"high","complexity":"large"}]"#;
        let items = parse_items_lenient(raw).expect("parse braces in strings");
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn lenient_fenced_block_still_works() {
        let raw =
            "```json\n[{\"title\":\"Login\",\"priority\":\"high\",\"complexity\":\"medium\"}]\n```";
        let items = parse_items_lenient(raw).expect("parse fenced block");
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn lenient_fully_garbage_is_err() {
        for raw in ["", "nope", "[{", "{broken"] {
            assert!(
                parse_items_lenient(raw).is_err(),
                "expected Err for {raw:?}"
            );
        }
    }

    #[test]
    fn test_output_object_parses_bugs_and_verdicts() {
        let raw = r#"```json
{"bugs":[{"title":"B","priority":"high","complexity":"medium","has_ui":true}],
 "verdicts":[{"ac":"login works","passed":true,"note":"GET /login 200","route":"/login"},{"ac":"logout works","passed":false,"note":"500","route":"/logout"}]}
```"#;
        let (bugs, verdicts) = parse_test_output(raw).expect("parse object");
        assert_eq!(bugs.len(), 1);
        assert_eq!(bugs[0].title, "B");
        assert_eq!(verdicts.len(), 2);
        assert_eq!(verdicts[0].ac, "login works");
        assert!(verdicts[0].passed);
        assert_eq!(verdicts[0].route, "/login");
        assert!(!verdicts[1].passed);
    }

    #[test]
    fn test_output_legacy_bare_array_yields_no_verdicts() {
        let (bugs, verdicts) =
            parse_test_output(r#"[{"title":"B","priority":"low","complexity":"small"}]"#)
                .expect("legacy array still parses");
        assert_eq!(bugs.len(), 1);
        assert!(verdicts.is_empty());
    }

    #[test]
    fn test_output_all_pass_yields_empty_bugs() {
        let (bugs, verdicts) = parse_test_output(
            r#"{"bugs":[],"verdicts":[{"ac":"a","passed":true,"note":"ok","route":""}]}"#,
        )
        .expect("empty bugs okay");
        assert!(bugs.is_empty());
        assert_eq!(verdicts.len(), 1);
    }
}
