//! Project-alias policy: deriving an alias from a project name and deciding
//! whether an alias may become a workspace directory id. Pure — no IO; the
//! callers (`onboard_project`, the HTTP route) own the refusal.

/// Derive a short uppercase alias from a project name: its capital letters
/// (`CoXChat` -> `CXC`), else the first three alphanumerics uppercased.
#[must_use]
pub fn derive_alias(name: &str) -> String {
    let caps: String = name.chars().filter(char::is_ascii_uppercase).collect();
    if caps.len() >= 2 {
        return caps.chars().take(4).collect();
    }
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .take(3)
        .collect::<String>()
        .to_uppercase()
}

/// CXA-B138: a project alias becomes a workspace directory id under the hub's
/// workspace base (`base.join(id)`), so any path separator or dot component
/// lets an alias like `../name` scaffold — and DELETE `rm -rf` — OUTSIDE the
/// base. CXA-B140: a control character in the id (a raw newline or backspace,
/// say) mangles every listing it lands in and makes the project non-obviously
/// deletable — DELETE needs the byte percent-encoded to match. A safe id is a
/// single non-hidden path component of printable characters: never empty, no
/// '/', no '\', no ".." anywhere, no leading dot, no control characters.
#[must_use]
pub fn is_safe_workspace_id(id: &str) -> bool {
    !id.is_empty()
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains("..")
        && !id.starts_with('.')
        && !id.chars().any(char::is_control)
}

#[cfg(test)]
mod alias_tests {
    use super::{derive_alias, is_safe_workspace_id};

    #[test]
    fn derives_from_capitals() {
        assert_eq!(derive_alias("CoXChat"), "CXC");
        assert_eq!(derive_alias("CoXAgent"), "CXA");
    }

    /// CXA-B138: every traversal shape the ticket names must be refused —
    /// an unsafe id would be `base.join`-ed outside the workspace base.
    #[test]
    fn path_traversing_ids_are_never_safe() {
        for id in [
            "../qatrav-esc",
            "..\\qatrav",
            "qa/../x",
            "a\\b",
            "..",
            ".",
            ".hidden",
            "",
        ] {
            assert!(!is_safe_workspace_id(id), "{id:?} must be refused");
        }
    }

    /// CXA-B140: the ticket's repros and every other C0 control byte — a raw
    /// newline or backspace in the id mangles listings and is non-obviously
    /// deletable (DELETE needs the byte percent-encoded) — plus the C1 range
    /// and DEL, which `char::is_control` covers.
    #[test]
    fn control_character_ids_are_never_safe() {
        for id in [
            "bad\nid", // the ticket's newline repro
            "a\u{8}",  // the ticket's backspace repro (JSON "\b")
            "bad\rid",
            "bad\tid",
            "a\u{0}b",
            "a\u{1f}b",
            "a\u{7f}b", // DEL
            "a\u{9f}b", // C1 control
            "trailing\n",
        ] {
            assert!(!is_safe_workspace_id(id), "{id:?} must be refused");
        }
    }

    /// Ordinary single-component ids — the only kind onboarding may use.
    /// NBSP pins the boundary: the refusal class is control characters (Cc)
    /// only, so other non-ASCII components stay accepted.
    #[test]
    fn plain_component_ids_are_safe() {
        for id in [
            "qatrav",
            "QATRAV",
            "qa-trav_2",
            "cxa",
            "caf\u{e9}",
            "a\u{a0}b",
        ] {
            assert!(is_safe_workspace_id(id), "{id:?} must be accepted");
        }
    }

    #[test]
    fn falls_back_to_first_letters() {
        assert_eq!(derive_alias("quotes"), "QUO");
        assert_eq!(derive_alias("my app"), "MYA");
    }
}
