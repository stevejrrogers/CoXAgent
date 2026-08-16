//! Re-export wiring guard for CXA-F027.
//!
//! Every use case module declared in this directory (`use_cases/mod.rs`) must also be surfaced
//! through the crate-root facade via a matching `pub use`, so downstream code can construct it as
//! `crate::use_cases::SomeUseCase` without importing an internal module path directly. A half-wired
//! use case — one whose module is declared but never re-exported — fails silently far away at each
//! call site instead of here.
//!
//! This test scans the single canonical manifest for this module tree (`mod.rs`, pulled in via
//! [`include_str!`], so fully hermetic and immune to CWD drift) and asserts every declared top-level
//! module is either re-exported or an explicitly allow-listed internal helper.
//!
//! The allow-list exists because [`ceremony`], [`approval_memory`] and [`approval_risk`] are
//! deliberately *internal* helpers consumed by sibling use cases through the fully-public crate path,
//! not members of the public facade.

#[cfg(test)]
mod tests {
    const MOD_SRC: &str = include_str!("mod.rs");

    /// Internal helper modules that may legitimately lack a facade re-export.
    const INTERNAL_HELPERS: &[&str] = &["ceremony", "approval_memory", "approval_risk"];

    /// True when `name` is a valid single-token Rust identifier (no paths/colons/spaces).
    fn is_valid_name(name: &str) -> bool {
        !name.is_empty()
            && !name.chars().next().is_some_and(char::is_numeric)
            && name.chars().all(|c| c.is_alphanumeric() || c == '_')
    }

    /// Strip a leading keyword token from `text`, returning everything after its trailing space.
    fn strip_kw<'a>(text: &'a str, kw: &str) -> Option<&'a str> {
        let rest = text.strip_prefix(kw)?;
        match rest.as_bytes().first() {
            None => Some(""),
            Some(&b) if b.is_ascii_whitespace() => Some(&rest[1..]),
            _ => None,
        }
    }

    /// Count occurrences of a byte in `text`.
    fn byte_count(text: &str, needle: u8) -> usize {
        text.bytes().filter(|b| *b == needle).count()
    }

    /// Collect every top-level single-token `pub mod <name>;` declaration in this file.
    fn declared_modules(src: &str) -> Vec<String> {
        src.lines()
            .filter_map(|line| {
                let mut rest = strip_kw(line.trim_start(), "pub")?;
                rest = strip_kw(rest.trim_start(), "mod")?;
                let name = rest.split_once(';')?.0.trim();
                if !is_valid_name(name) {
                    return None;
                }
                Some(name.to_string())
            })
            .collect()
    }

    /// Collect every identifier across all statements rooted on a `use` keyword, spanning multiple
    /// lines and brace groups until each statement's terminating ';'.
    fn reexport_names(src: &str) -> Vec<String> {
        let mut names = std::collections::HashSet::new();
        let mut collecting = false;
        let mut depth: usize = 0;

        for line in src.lines() {
            // Enter collection on either a bare statement (`use x;`) or an exported one
            // (`pub use x;`) — never on declarations (`pub mod x;`, which stays excluded).
            let kw_head = strip_kw(line.trim_start(), "pub")
                .unwrap_or_else(|| line.trim_start())
                .trim_start();
            if !collecting && strip_kw(kw_head, "use").is_none() {
                continue;
            }
            collecting = true;

            // Extract every contiguous identifier token ([A-Za-z0-9_] run) on this line.
            let bytes = line.as_bytes();
            let mut start: Option<usize> = None;
            for (idx, b) in bytes.iter().enumerate() {
                if b.is_ascii_alphanumeric() || *b == b'_' {
                    start.get_or_insert(idx);
                } else if let Some(s) = start.take() {
                    names.insert(String::from_utf8_lossy(&bytes[s..idx]).into_owned());
                }
            }
            if let Some(s) = start.take() {
                names.insert(String::from_utf8_lossy(&bytes[s..]).into_owned());
            }

            let opens = byte_count(line, b'{');
            let closes = byte_count(line, b'}');
            depth = depth.saturating_add(opens).saturating_sub(closes);
            if collecting && depth == 0 && line.contains(';') {
                collecting = false;
                depth = 0;
            }
        }

        let mut sorted: Vec<String> = names.into_iter().collect();
        sorted.sort();
        sorted
    }

    #[test]
    fn every_declared_module_is_facade_exposed_or_internal_helper() {
        // Sanity check the scanner actually sees something — otherwise any change could silently
        // disable this guard while still passing.
        let declared = declared_modules(MOD_SRC);
        assert!(!declared.is_empty(), "guard scanned no declarations");
        assert!(
            declared.contains(&"run_releases".to_string()),
            "guard missed expected module declarations"
        );

        // The run_releases wiring this guard protects must stay re-exported.
        assert!(
            MOD_SRC.contains("RunReleasesUseCase"),
            "expected RunReleasesUseCase to be re-exported from this facade"
        );

        let exported: std::collections::HashSet<String> =
            reexport_names(MOD_SRC).into_iter().collect();

        // Every declared module must appear among the facade's re-exports unless it is an
        // explicitly allow-listed internal helper.
        let missing: Vec<String> = declared_modules(MOD_SRC)
            .into_iter()
            .filter(|m| !INTERNAL_HELPERS.contains(&m.as_str()) && !exported.contains(m))
            .collect();

        assert!(
            missing.is_empty(),
            "declared but not facade-re-exported (and not an internal helper): {}",
            missing.join(", ")
        );
    }

    // ---- Parser-level regression tests over synthetic source strings ----
    //
    // These pin down how each piece of the scanner behaves independently of
    // whatever happens to be wired in mod.rs today, so an accidental parse
    // change can't slip past a coincidentally-passing integration assertion,
    // and so we lock in that a genuinely half-wired module fails loudly here
    // instead of far away at its call site tomorrow.
    //
    // Each uses small hand-built manifest texts rather than include_str!, which
    // keeps them immune to edits elsewhere in this directory's facade file —
    // they assert about *parsing/matching behaviour*, not current wiring state.


    fn missing_for(src: &str) -> Vec<String> {
        let exported: std::collections::HashSet<String> =
            reexport_names(src).into_iter().collect();
        declared_modules(src)
            .into_iter()
            .filter(|m| !INTERNAL_HELPERS.contains(&m.as_str()) && !exported.contains(m))
            .collect()
    }

    #[test]
    fn declares_every_top_level_pub_mod() {
        let src = "//! doc\npub mod foo;\npub mod bar;\n";
        assert_eq!(declared_modules(src), vec!["foo".to_string(), "bar".to_string()]);
    }

    #[test]
    fn skips_non_pub_module_lines_when_declaring() {
        // A private test module (`mod internal;`) must not count as a facade
        // declaration — only `pub mod` lines are part of the public surface.
        let src = "//! header\n\npub use other::Thing;\n#[cfg(test)]\nmod internal;\n";
        assert_eq!(declared_modules(src), Vec::<String>::new());
    }

    #[test]
    fn reexports_names_from_multiline_brace_groups() {
        let src = "pub use alpha::{AUseCase, BType};\npub use beta::{\n  C,\n  D,\n};\n";
        let exported: std::collections::HashSet<String> =
            reexport_names(src).into_iter().collect();
        for expected in ["alpha", "beta", "AUseCase", "BType", "C", "D"] {
            assert!(
                exported.contains(expected),
                "expected token {expected:?} to be collected, got {exported:?}"
            );
        }
    }

    #[test]
    fn half_wired_module_is_reported_as_missing() {
        // `baz` is declared but never re-exported — exactly the half-wired case
        // this guard exists to catch. It must fail fast (be reported missing).
        let src = "pub mod foo;\npub mod baz;\npub use foo::{FooUseCase};";
        assert_eq!(missing_for(src), vec!["baz".to_string()]);
    }

    #[test]
    fn allow_listed_internal_helpers_are_not_missing() {
        for helper in INTERNAL_HELPERS {
            let src = format!("pub mod {helper};\npub use other::{{OtherUseCase}};");
            assert!(
                missing_for(&src).is_empty(),
                "{helper} should be allowed as an internal helper"
            );
        }
    }
}
