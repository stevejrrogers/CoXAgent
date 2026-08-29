//! Test-to-AC traceability matching (CXA-F024) — turns the TEST agent's
//! per-criterion verdicts into the coverage matrix's data.
//!
//! Pure functions over real state: no IO, no engine, no ports. The TEST agent
//! is asked for word-for-word criterion text, but it is an LLM and sometimes
//! paraphrases — so a verdict is matched to its criterion EXACTLY first, then
//! by keyword + fuzzy (Levenshtein distance ≤ 0.3) overlap. On a match the
//! verdict is recorded and the criterion's evidence sources (test files, or an
//! API request/response for non-UI tickets) are attached for the matrix to
//! surface.

use crate::parsing::TestVerdict;
use crate::state::ProjectState;
use coxagent_domain::Ticket;

/// Normalized Levenshtein similarity at which two texts count as the same
/// wording: distance ≤ 0.3 of the longer text (the AC's "Levenshtein ≤ 0.3").
const FUZZY_SIMILARITY: f64 = 0.7;
/// Fraction of a criterion's meaningful words that must appear in the
/// candidate text for a keyword match.
const KEYWORD_OVERLAP: f64 = 0.6;

/// Common function words that carry no criterion-specific meaning; dropped
/// before computing keyword overlap so boilerplate cannot match.
const STOPWORDS: &[&str] = &[
    "a", "an", "the", "is", "are", "be", "been", "to", "of", "and", "or", "on", "in", "for",
    "with", "that", "this", "it", "its", "as", "at", "by", "from", "when", "then", "than", "into",
    "not", "no", "after", "before", "each", "every", "any", "all", "should", "must", "can", "will",
    "may", "do", "does", "done",
];

/// Classic Levenshtein edit distance (two-row DP; inputs are short lines).
#[must_use]
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0_usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// 1 − normalized edit distance: 1.0 for identical texts, ≥ 0.7 when the
/// Levenshtein distance is at most 0.3 of the longer text.
// Inputs are bounded text lines (criteria ≤ 5, ≤ 160 chars) — far below f64's
// 53-bit mantissa, so the usize→f64 casts cannot lose meaning here.
#[allow(clippy::cast_precision_loss)]
#[must_use]
pub fn similarity(a: &str, b: &str) -> f64 {
    let longest = a.chars().count().max(b.chars().count());
    if longest == 0 {
        return 1.0;
    }
    1.0 - (levenshtein(a, b) as f64) / (longest as f64)
}

/// Meaningful word tokens, lowercased, stopwords and punctuation dropped.
fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .collect()
}

/// Fraction of the criterion's meaningful words that also appear in `text`.
/// Empty criterion (nothing meaningful to match) scores 0, never 1.
// Same bound as `similarity`: token counts of short text lines, no mantissa risk.
#[allow(clippy::cast_precision_loss)]
#[must_use]
pub fn keyword_overlap(criterion: &str, text: &str) -> f64 {
    let ac = tokens(criterion);
    if ac.is_empty() {
        return 0.0;
    }
    let t = tokens(text);
    let hits = ac.iter().filter(|w| t.contains(w)).count();
    hits as f64 / ac.len() as f64
}

/// How strongly a piece of test text addresses a criterion: 1.0 for exact
/// wording, otherwise the stronger of fuzzy similarity and keyword overlap.
#[must_use]
pub fn match_score(criterion: &str, text: &str) -> f64 {
    if criterion.trim() == text.trim() {
        return 1.0;
    }
    similarity(criterion, text).max(keyword_overlap(criterion, text))
}

/// Does this test text address this criterion, per the AC's rule: fuzzy
/// Levenshtein ≤ 0.3, or keyword overlap ≥ [`KEYWORD_OVERLAP`]?
#[must_use]
pub fn matches_criterion(criterion: &str, text: &str) -> bool {
    criterion.trim() == text.trim()
        || similarity(criterion, text) >= FUZZY_SIMILARITY
        || keyword_overlap(criterion, text) >= KEYWORD_OVERLAP
}

/// Does an evidence note read as an API request/response ("GET /login 200",
/// "POST /api/users → 201") rather than prose? Those count as coverage for
/// non-UI tickets without any file-based test (CXA-F024 edge case).
#[must_use]
pub fn is_api_request_response(note: &str) -> bool {
    let s = note.trim();
    let mut words = s.split_whitespace();
    let method = words.next().unwrap_or("");
    let path = words.next().unwrap_or("");
    (matches!(
        method,
        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
    ) && path.starts_with('/'))
        || s.starts_with("curl ")
}

/// Where the proof for a verdict lives, most specific first: the test files
/// the agent named, else the API request/response line, else the UI route the
/// criterion was demonstrated on. Empty when the verdict carries no usable
/// source (prose-only note).
#[must_use]
pub fn sources_for(verdict: &TestVerdict) -> Vec<String> {
    let files: Vec<String> = verdict
        .tests
        .iter()
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .collect();
    if !files.is_empty() {
        return files;
    }
    if is_api_request_response(&verdict.note) {
        return vec![verdict.note.trim().to_owned()];
    }
    if !verdict.route.trim().is_empty() {
        return vec![verdict.route.trim().to_owned()];
    }
    Vec::new()
}

/// Record one verdict onto its criterion: the verdict itself (a pass/fail on
/// the case) plus the evidence sources. Provenance never changes a verdict's
/// meaning; a re-run simply overwrites with fresher evidence.
fn apply_verdict(ticket: &mut Ticket, criterion: &str, verdict: &TestVerdict, at: &str) -> bool {
    ticket.ensure_test_cases_from_acceptance();
    let note = if verdict.note.trim().is_empty() {
        None
    } else {
        Some(verdict.note.trim().to_owned())
    };
    let mut changed =
        ticket.set_test_case_result(criterion, verdict.passed, note, None, at.to_owned());
    let sources = sources_for(verdict);
    if !sources.is_empty() {
        changed |= ticket.set_test_case_sources(criterion, sources);
    }
    changed
}

/// Apply the TEST agent's verdicts to the project's tickets, returning whether
/// anything changed. Each verdict lands on the ticket whose acceptance
/// criterion it names — exactly when possible, otherwise by the best keyword +
/// fuzzy match above threshold across every ticket's criteria (the agent
/// paraphrased; the evidence still belongs to that criterion).
pub fn record_verdicts(state: &mut ProjectState, verdicts: &[TestVerdict], at: &str) -> bool {
    let mut changed = false;
    for v in verdicts {
        let ac = v.ac.trim();
        if ac.is_empty() {
            continue;
        }
        // Contract path: the prompt demands word-for-word criterion text.
        if let Some(t) = state
            .tickets
            .iter_mut()
            .find(|t| t.acceptance_criteria().iter().any(|c| c == ac))
        {
            changed |= apply_verdict(t, ac, v, at);
            continue;
        }
        // Fuzzy path: one best criterion across all tickets, above threshold.
        let best =
            state
                .tickets
                .iter()
                .enumerate()
                .fold(None::<(f64, usize, String)>, |acc, (i, t)| {
                    t.acceptance_criteria().iter().fold(acc, |acc, c| {
                        if !matches_criterion(ac, c) {
                            return acc;
                        }
                        let s = match_score(ac, c);
                        match acc {
                            // Ties keep the first candidate — deterministic order.
                            Some((bs, _, _)) if bs >= s => acc,
                            _ => Some((s, i, (*c).clone())),
                        }
                    })
                });
        if let Some((_, i, criterion)) = best {
            changed |= apply_verdict(&mut state.tickets[i], &criterion, v, at);
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levenshtein_counts_edits() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("same", "same"), 0);
        assert_eq!(levenshtein("abc", ""), 3);
    }

    #[test]
    fn similarity_thresholds_match_the_ac_ratio() {
        assert!((similarity("identical", "identical") - 1.0).abs() < 1e-9);
        // 1 edit in 10 chars = distance 0.1 ≤ 0.3.
        assert!(similarity("criteria ten", "criteria tens") >= FUZZY_SIMILARITY);
        // Half the text rewritten = distance 0.5 > 0.3.
        assert!(similarity("aaaabbbbbb", "bbbbaaaaaa") < FUZZY_SIMILARITY);
        assert!((similarity("", "") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn keyword_overlap_ignores_stopwords_and_punctuation() {
        // 3 of 4 meaningful words shared.
        let ac = "repro steps are listed on the ticket";
        assert!((keyword_overlap(ac, "the ticket lists repro steps") - 0.75).abs() < 1e-9);
        assert!(
            keyword_overlap("repro steps", "unrelated nonsense entirely") < 1e-9,
            "disjoint vocabularies share no keywords"
        );
        // A criterion with no meaningful tokens cannot match by keyword.
        assert!(keyword_overlap("the of and", "the of and") < 1e-9);
    }

    #[test]
    fn matching_accepts_exact_fuzzy_and_keyword_but_not_disjoint() {
        let ac = "repro steps are listed on the ticket";
        assert!(matches_criterion(ac, ac));
        assert!(matches_criterion(ac, "repro steps listed on ticket"));
        assert!(!matches_criterion(ac, "login returns 500 on bad input"));
    }

    #[test]
    fn api_request_response_notes_are_recognized() {
        assert!(is_api_request_response("GET /login 200"));
        assert!(is_api_request_response("POST /api/users → created 201"));
        assert!(is_api_request_response("  DELETE /items/3 204  "));
        assert!(is_api_request_response("curl -s localhost:8080/health"));
        assert!(!is_api_request_response(
            "cargo test -p coxagent-domain passes"
        ));
        assert!(!is_api_request_response(""));
    }

    #[test]
    fn sources_prefer_files_then_api_then_route() {
        let files = TestVerdict {
            ac: "ac".into(),
            passed: true,
            note: "GET /health 200".into(),
            route: "/health".into(),
            tests: vec!["crates/domain/tests/gate.rs".into()],
        };
        assert_eq!(sources_for(&files), vec!["crates/domain/tests/gate.rs"]);

        let api = TestVerdict {
            tests: Vec::new(),
            ..files.clone()
        };
        assert_eq!(sources_for(&api), vec!["GET /health 200"]);

        let routed = TestVerdict {
            note: String::new(),
            ..api
        };
        assert_eq!(sources_for(&routed), vec!["/health"]);

        let prose = TestVerdict {
            route: String::new(),
            ..routed
        };
        assert!(
            sources_for(&prose).is_empty(),
            "prose-only note has no source"
        );
    }
}
