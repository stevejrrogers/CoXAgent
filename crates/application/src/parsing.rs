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
