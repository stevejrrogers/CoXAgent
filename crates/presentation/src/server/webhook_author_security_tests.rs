//! The public chat webhook (`/api/chat/hook/:token`, token-only, no login)
//! must never let an external poster author under an internal identity. The
//! chat UI renders gate *action cards* for `SM`/`SYSTEM` senders, so a leaked
//! token that could forge those names would be an in-channel approval-phishing
//! surface. `neutralize_reserved_author` prefixes any collision with `hook:`.

use super::chat::neutralize_reserved_author;

#[test]
fn reserved_internal_identities_are_neutralized_case_insensitively() {
    // The two that drive gate action cards — the sharp edge.
    assert_eq!(neutralize_reserved_author("SM"), "hook:SM");
    assert_eq!(neutralize_reserved_author("SYSTEM"), "hook:SYSTEM");
    // Case does not let it through.
    assert_eq!(neutralize_reserved_author("sm"), "hook:sm");
    assert_eq!(neutralize_reserved_author("System"), "hook:System");
    // Every agent role label is reserved, not just the two gate senders.
    for r in [
        "BA",
        "PO",
        "SA",
        "PD",
        "DEV-BUG",
        "DEV-FEATURE",
        "TEST",
        "DOCS",
    ] {
        assert_eq!(neutralize_reserved_author(r), format!("hook:{r}"));
    }
}

#[test]
fn ordinary_external_authors_pass_through_untouched() {
    for name in [
        "GitHub",
        "ci-bot",
        "Grafana",
        "smith",
        "docs-team",
        "pd-review",
    ] {
        assert_eq!(neutralize_reserved_author(name), name);
    }
}

#[test]
fn a_neutralized_author_cannot_match_the_gate_card_trigger() {
    // The UI fires gate cards only on an exact "SM"/"SYSTEM" sender; the
    // prefixed form must not equal either.
    assert_ne!(neutralize_reserved_author("SM"), "SM");
    assert_ne!(neutralize_reserved_author("SYSTEM"), "SYSTEM");
}
