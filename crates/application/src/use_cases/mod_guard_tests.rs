//! Re-export wiring guard for CXA-F027.
//!
//! Every top-level module declared in this directory (`use_cases/mod.rs`) must be surfaced through
//! this same crate-root facade via a matching `pub use`, so downstream code can construct it as
//! [`crate::use_cases::XxxUseCase`] instead of reaching into an internal module path directly.
//! A half-wired use case — its module declared but never re-exported — fails silently far away at
//! each call site rather than here.
//!
//! This test scans the single canonical manifest for this tree ([`mod.rs`], pulled in via
//! [`include_str!`], so fully hermetic and immune to CWD drift) and asserts every declared top-level
//! module either appears as a facade-renderable qualifier or is an explicitly allow-listed internal
//! helper consumed through a sibling-by-crate-path rather than the public surface.
//!
//! The test-only submodules (`#[cfg(test)] mod ...`) are excluded automatically because they are not
//! declared with a leading `pub`.

#[cfg(test)]
mod tests {
    const MOD_SRC: &str = include_str!("mod.rs");

    /// Internal helper modules consumed via sibling crate paths rather than surfaced through this facade.
    const INTERNAL_HELPERS: &[&str] = &["ceremony", "approval_memory", "approval_risk"];

    /// Drop keyword `kw` plus exactly one following run of whitespace from the head of `text`.
    /// Returns the remainder, or `None` when `text` does not start with the keyword.
    fn strip_kw<'a>(text: &'a str, kw: &str) -> Option<&'a str> {
        let rest = text.strip_prefix(kw)?;
        match rest.as_bytes().first() {
            None => Some(""),
            Some(&b) if b.is_ascii_whitespace() => Some(rest[1..].trim_start()),
            _ => None,
        }
    }

    /// Collect every top-level single-token name introduced by `pub mod <name> ... ;`.
    ///
    /// Tolerates any content between the identifier and its terminating ';' (e.g. an inline
    /// trailing comment), so adding one can never silently hide a declared-but-half-wired module.
    fn parse_declarations(src: &str) -> Vec<String> {
        src.lines()
            .filter_map(|line| {
                let after_mod = strip_kw(strip_kw(line.trim(), "pub")?, "mod")?;
                let ident_end = after_mod.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
                if ident_end == 0 {
                    return None;
                }
                // A terminating ';' must follow (possibly after whitespace / an inline comment).
                if !after_mod[ident_end..].trim_start().starts_with(';') {
                    return None;
                }
                Some(after_mod[..ident_end].to_string())
            })
            .collect()
    }

    /// Collect every identifier immediately followed by the path separator (`::`) across all lines that
    /// open a facade re-export statement (start with optional whitespace then `pub use`). These are the
    /// module qualifiers being surfaced, e.g. `run_releases` in `run_releases::RunReleasesUseCase`.
    fn facade_path_prefixes(src: &str) -> Vec<String> {
        let mut out = std::collections::HashSet::new();
        for line in src.lines() {
            if !line.trim_start().starts_with("pub use") {
                continue;
            }
            let bytes = line.as_bytes();
            for j in 0..bytes.len() {
                // `bytes[j]` opens a "::" pair AND the byte before it closes an identifier
                // run, i.e. this is a module qualifier of the form `<ident>::`.
                if j > 0
                    && bytes[j] == b':'
                    && bytes.get(j + 1).copied() == Some(b':')
                    && is_ident_char(bytes[j - 1])
                {
                    out.insert(ident_before(bytes, j));
                }
            }
        }
        let mut sorted: Vec<String> = out.into_iter().collect();
        sorted.sort();
        sorted
    }

    fn is_ident_char(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'_'
    }

    /// Return the identifier run ending just before byte index `j`.
    fn ident_before(bytes: &[u8], j: usize) -> String {
        let mut start = j;
        while start > 0 && is_ident_char(bytes[start - 1]) {
            start -= 1;
        }
        std::str::from_utf8(&bytes[start..j])
            .expect("valid utf8")
            .to_string()
    }

    /// Names declared in mod.rs but neither facaded nor allow-listed — empty when fully wired.
    fn missing_from_facade(declared: &[String], prefixes: &[String]) -> Vec<String> {
        let prefix_set: std::collections::HashSet<&str> =
            prefixes.iter().map(String::as_str).collect();
        declared
            .iter()
            .filter(|m| !INTERNAL_HELPERS.contains(&m.as_str()) && !prefix_set.contains(m.as_str()))
            .cloned()
            .collect()
    }

    #[test]
    fn every_declared_module_is_facade_exposed_or_internal_helper() {
        // Sanity check the scanner actually sees something — otherwise any change could silently
        // disable this guard while still passing.
        let declared = parse_declarations(MOD_SRC);
        assert!(declared.len() >= 20, "guard scanned too few declarations");
        assert!(
            declared.iter().any(|m| m == "run_releases"),
            "guard missed expected module declarations"
        );

        // The specific re-export this guard exists to protect must stay in place so downstream
        // wiring (cycle/wiring.rs) can keep constructing it through crate::use_cases.
        assert!(
            MOD_SRC.contains("RunReleasesUseCase"),
            "expected RunReleasesUseCase to be re-exported from this facade"
        );

        let prefixes = facade_path_prefixes(MOD_SRC);
        let missing = missing_from_facade(&declared, &prefixes);
        assert!(
            missing.is_empty(),
            "declared but not facade-re-exported (and not an internal helper): {}",
            missing.join(", ")
        );
    }

    // ---- Parser-level regression tests over synthetic manifest strings ----
    //
    // These pin down how each piece of the scanner behaves independently of whatever happens to be
    // wired in mod.rs today, so an accidental parse change cannot slip past a coincidentally-passing
    // integration assertion, and so a genuinely half-wired module is proven to fail loudly here.

    #[test]
    fn declares_every_top_level_pub_mod() {
        let src = "//! doc\npub mod foo;\npub mod bar;\n";
        assert_eq!(
            parse_declarations(src),
            vec!["foo".to_string(), "bar".to_string()]
        );
    }

    #[test]
    fn declaration_with_inline_trailing_comment_is_still_detected() {
        // An inline comment after a module declaration must not hide it from the guard — otherwise
        // adding such a comment could silently conceal a declared-but-half-wired module.
        let src = "pub mod foo; // wires legacy path\npub mod baz;\n";
        assert_eq!(
            parse_declarations(src),
            vec!["foo".to_string(), "baz".to_string()]
        );
    }

    #[test]
    fn test_only_and_non_pub_modules_are_not_declarations() {
        let src = "pub use other::Thing;\n#[cfg(test)]\nmod internal;\n";
        assert_eq!(parse_declarations(src), Vec::<String>::new());
    }

    #[test]
    fn skips_non_pub_module_lines_when_declaring() {
        // A private test module (`mod internal;`) must not count as a facade
        // declaration — only `pub mod` lines are part of the public surface.
        let src = "//! header\n\npub use other::Thing;\n#[cfg(test)]\nmod internal;\n";
        assert_eq!(parse_declarations(src), Vec::<String>::new());
    }

    #[test]
    fn collects_path_qualifiers_from_reexports_including_multiline_groups() {
        let src =
            "pub use alpha::{AUseCase, BType};\npub use beta::{\n  C,\n  D,\n};\npub use gamma::G;\n";
        let prefixes = facade_path_prefixes(src);
        for expected in ["alpha", "beta", "gamma"] {
            assert!(
                prefixes.contains(&expected.to_string()),
                "{expected} not collected"
            );
        }
    }

    #[test]
    fn half_wired_module_is_reported_as_missing() {
        let declared = vec!["foo".to_string(), "baz".to_string()];
        let prefixes = vec!["foo".to_string()];
        assert_eq!(
            missing_from_facade(&declared, &prefixes),
            vec!["baz".to_string()]
        );
    }

    #[test]
    fn allow_listed_internal_helpers_are_not_missing() {
        for helper in INTERNAL_HELPERS {
            let declared = vec![(*helper).to_string(), "other".to_string()];
            let prefixes = vec!["other".to_string()];
            assert_eq!(
                missing_from_facade(&declared, &prefixes),
                Vec::<String>::new()
            );
        }
    }
}
