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

/// CXA-B147: the id becomes a filesystem component name (`base.join(id)`),
/// and filesystems refuse a single component past ~255 units (bytes on Linux,
/// UTF-16 code units on macOS) — a 300-char alias used to pass the character
/// checks and die in `create_dir_all` with ENAMETOOLONG (os error 36), a pure
/// request-validation failure reported as a 500. The bound counts BYTES,
/// which dominates both caps for UTF-8 names, and sits far below the limit,
/// so the `unique_id` collision suffixes (`-2` … `-9999`), case-folding and
/// the `cox-{id}-…` container names can never push the real directory name
/// over it.
pub const MAX_WORKSPACE_ID_BYTES: usize = 64;

/// The invisible-format (Cf) characters an id must never carry — the same
/// classes [`crate::brief_screening`] already treats as hostile content:
/// bidi overrides/isolates render text visually reordered (U+202E makes a
/// listing read as a different name than the bytes say), zero-width and
/// joiner characters make two visually identical ids distinct, and the BOM
/// is pure invisible payload. Scope: brief screening's hostile set plus the
/// soft hyphen — deliberately NOT every Unicode Cf char (std has no
/// general-category API to enumerate them) — so ordinary non-ASCII ids
/// (é, NBSP) stay accepted.
#[must_use]
fn is_invisible_format_char(c: char) -> bool {
    let u = c as u32;
    u == 0x00AD // soft hyphen
        || (0x200B..=0x200F).contains(&u) // zero-width chars, LRM/RLM
        || (0x202A..=0x202E).contains(&u) // bidi embedding/overrides
        || (0x2060..=0x206F).contains(&u) // word joiner, isolates, deprecated formats
        || u == 0xFEFF // BOM / zero-width no-break space
}

/// CXA-B138: a project alias becomes a workspace directory id under the hub's
/// workspace base (`base.join(id)`), so any path separator or dot component
/// lets an alias like `../name` scaffold — and DELETE `rm -rf` — OUTSIDE the
/// base. CXA-B140: a control character in the id (a raw newline or backspace,
/// say) mangles every listing it lands in and makes the project non-obviously
/// deletable — DELETE needs the byte percent-encoded to match. CXA-B144: a
/// bidi override (U+202E, a Cf format char, invisible to `char::is_control`)
/// mangles listings the same way while rendering visually REORDERED — a
/// spoofable id — so the invisible-format class is refused too. CXA-B147: an
/// id longer than [`MAX_WORKSPACE_ID_BYTES`] can never exist as a directory
/// name at all. A safe id is a single non-hidden path component of printable
/// characters that fits the filesystem: never empty, no '/', no '\', no ".."
/// anywhere, no leading dot, no control characters, no invisible format
/// characters, at most [`MAX_WORKSPACE_ID_BYTES`] bytes.
#[must_use]
pub fn is_safe_workspace_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_WORKSPACE_ID_BYTES
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains("..")
        && !id.starts_with('.')
        && !id.chars().any(char::is_control)
        && !id.chars().any(is_invisible_format_char)
}

/// Why `id` may not become a workspace directory id — "too long" vs "contains
/// a separator" — so every refusal site composes a message naming the ACTUAL
/// defect: triage must not chase a separator rule for an alias that merely
/// overflows the filesystem's component limit (CXA-B147). Pure; pairs with
/// [`is_safe_workspace_id`] (safe ⇔ no refusal).
#[must_use]
pub fn workspace_id_refusal_reason(id: &str) -> String {
    if id.len() > MAX_WORKSPACE_ID_BYTES {
        format!("must be at most {MAX_WORKSPACE_ID_BYTES} bytes (it becomes a workspace directory name)")
    } else {
        "must not contain '/', '\\', '..', leading dots, control or invisible formatting characters"
            .to_owned()
    }
}

#[cfg(test)]
mod alias_tests {
    use super::{
        derive_alias, is_safe_workspace_id, workspace_id_refusal_reason, MAX_WORKSPACE_ID_BYTES,
    };

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

    /// CXA-B144: the ticket's bidi-override repro and the rest of the
    /// invisible-format (Cf) class — a bidi override in the id renders every
    /// listing visually reordered (a spoofable name) and is non-obviously
    /// deletable (DELETE needs the byte percent-encoded), exactly the B140
    /// listing-mangling class, but Cf chars are invisible to
    /// `char::is_control`.
    #[test]
    fn bidi_override_and_invisible_format_ids_are_never_safe() {
        for id in [
            "qa\u{202E}gpd", // the ticket's repro: reads as "qapg" reversed
            "a\u{202A}b",
            "a\u{202B}b",
            "a\u{202C}b",
            "a\u{202D}b",
            "a\u{200E}b", // LRM
            "a\u{200F}b", // RLM
            "a\u{200B}b", // zero-width space
            "a\u{200D}b", // zero-width joiner
            "a\u{2060}b", // word joiner
            "a\u{2066}b", // bidi isolate
            "a\u{FEFF}b", // BOM
            "a\u{AD}b",   // soft hyphen
            "trailing\u{202E}",
        ] {
            assert!(!is_safe_workspace_id(id), "{id:?} must be refused");
        }
    }

    /// Ordinary single-component ids — the only kind onboarding may use.
    /// NBSP pins the boundary: the refusal class is control (Cc) and
    /// invisible-format (Cf) characters only, so other non-ASCII components
    /// stay accepted.
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

    /// CXA-B147: the ticket's 300-char repro is a legal string but an
    /// impossible directory name — it must be refused at the byte boundary,
    /// which is exactly one id wide: the longest acceptable alias and the
    /// first refused one.
    #[test]
    fn over_long_ids_are_never_safe() {
        assert!(
            !is_safe_workspace_id(&"a".repeat(300)),
            "the ticket's 300-char alias must be refused"
        );
        assert!(!is_safe_workspace_id(
            &"a".repeat(MAX_WORKSPACE_ID_BYTES + 1)
        ));
        assert!(is_safe_workspace_id(&"a".repeat(MAX_WORKSPACE_ID_BYTES)));
        // The bound is BYTES, not chars: 32 'é' are 64 UTF-8 bytes and fit,
        // 33 of them (66 bytes) do not — pins `id.len()` against a future
        // "fix" to `chars().count()`.
        assert!(is_safe_workspace_id(&"é".repeat(32)));
        assert!(!is_safe_workspace_id(&"é".repeat(33)));
    }

    /// The refusal names the actual defect: an over-long alias must not be
    /// told it "contains '/'", or triage chases the wrong validation.
    #[test]
    fn the_refusal_reason_names_the_actual_defect() {
        let long = "a".repeat(300);
        assert!(
            workspace_id_refusal_reason(&long).contains("at most 64 bytes"),
            "an over-long alias must be refused for its length: {}",
            workspace_id_refusal_reason(&long)
        );
        assert!(
            workspace_id_refusal_reason("../x").contains("must not contain"),
            "a traversing alias keeps the separator refusal"
        );
        // CXA-B144: the wording must cover the new class — a bidi alias gets
        // the character-class refusal, never a stale message or a length one.
        let bidi = workspace_id_refusal_reason("qa\u{202E}gpd");
        assert!(
            bidi.contains("invisible formatting"),
            "a bidi alias must be refused for its character class: {bidi}"
        );
    }

    #[test]
    fn falls_back_to_first_letters() {
        assert_eq!(derive_alias("quotes"), "QUO");
        assert_eq!(derive_alias("my app"), "MYA");
    }
}
