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
