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
/// prose, and parse it into proposed items.
///
/// # Errors
/// Returns a message when no array is present or the JSON is malformed.
pub fn parse_items(raw: &str) -> Result<Vec<ProposedItem>, String> {
    let start = raw.find('[').ok_or("no JSON array found")?;
    let end = raw.rfind(']').ok_or("no closing bracket")?;
    if end < start {
        return Err("malformed array bounds".to_owned());
    }
    serde_json::from_str(&raw[start..=end]).map_err(|e| e.to_string())
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
}
