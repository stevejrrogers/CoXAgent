//! Analytical heart of the periodic tech-debt sweep.
//!
//! A sweep stops being a vague repeating chore once it carries evidence. This
//! module turns port-supplied snapshots - how many lint errors there are now,
//! which source modules lack doc headers - into debt findings plus rendered
//! acceptance-criteria lines for the ticket those findings justify.
//!
//! Every function here is PURE: all IO happened upstream behind ports, and this
//! layer only decides what those numbers mean, so each signal is unit-testable
//! with plain literals.

use super::super::super::ports::outbound::LintReport;

use coxagent_domain::{DebtSignal, DebtSignalKind};

/// Whether today's lint count has regressed above the recorded baseline.
#[must_use]
pub fn lint_delta(now: Option<&LintReport>, prior_baseline: Option<u64>) -> Option<DebtSignal> {
    let report = now?;
    let floor = prior_baseline.unwrap_or(0);
    if report.errors > floor && report.errors > 0 {
        Some(DebtSignal::new(
            DebtSignalKind::LintRegression,
            report.errors,
        ))
    } else {
        None
    }
}

/// Fold this cycle's measurements into one ordered list of real debt signals.
#[must_use]
pub fn assemble_signals(
    lint_now: Option<&LintReport>,
    prior_baseline: Option<u64>,
    missing_docs: usize,
) -> Vec<DebtSignal> {
    let mut signals = Vec::new();
    if let Some(lint) = lint_delta(lint_now, prior_baseline) {
        signals.push(lint);
    }
    if missing_docs > 0 {
        signals.push(DebtSignal::new(
            DebtSignalKind::MissingModuleDocs,
            missing_docs as u64,
        ));
    }
    signals
}

/// One measurable acceptance criterion per signal so whoever picks up the sweep
/// knows exactly what "done" means and can hold themselves to a number.
#[must_use]
pub fn acceptance_lines(signals: &[DebtSignal]) -> Vec<String> {
    signals.iter().map(line_for).collect()
}

fn line_for(signal: &DebtSignal) -> String {
    match signal.kind {
        DebtSignalKind::LintRegression => format!(
            "Reduce clippy errors below {} (the current tally driving this ticket)",
            signal.count
        ),
        DebtSignalKind::MissingModuleDocs => format!(
            "Add module docs to all {} source files lacking an inner-doc header",
            signal.count
        ),
        DebtSignalKind::DeadCodeSuspects => {
            format!("Resolve all {} dead-code suspects", signal.count)
        }
        // Non-exhaustive upstream enum: degrade unknown future kinds to their count.
        _ => format!(
            "Resolve the {} debt findings of an unrecognised kind",
            signal.count
        ),
    }
}

/// Raw description body for the filed chore from how many distinct findings it carries.
#[must_use]
pub fn describe(signal_count: usize) -> String {
    let noun = if signal_count == 1 {
        "finding"
    } else {
        "findings"
    };
    format!(
        "Scheduled tech-debt pass - no new features. Pay down {signal_count} quantified \
         {noun} surfaced by this cycle's analysis; full details are in its acceptance criteria."
    )
}

/// Count source modules that shipped without an inner-doc (`//!`) header, given
/// `(path, contents)` pairs. A file whose first non-blank line is an inner-doc
/// line or a leading attribute (e.g. `#![allow(...)]`) counts as documented;
/// every other non-empty source counts as missing its doc header.
#[must_use]
pub fn count_modules_missing_docs<'a>(pairs: impl Iterator<Item = (&'a str, &'a str)>) -> usize {
    pairs
        .filter(|(_, src)| !starts_with_module_doc(src))
        .count()
}

fn starts_with_module_doc(src: &str) -> bool {
    let text = src.strip_prefix('\u{feff}').unwrap_or(src);
    match text.lines().map(str::trim).find(|line| !line.is_empty()) {
        Some(first) => first.starts_with("//!") || first.starts_with('#'),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(errors: u64) -> LintReport {
        LintReport {
            errors,
            ..LintReport::default()
        }
    }

    #[test]
    fn lint_regression_is_reported_only_when_current_exceeds_baseline() {
        assert!(lint_delta(Some(&report(5)), Some(1)).is_some());
        assert_eq!(lint_delta(Some(&report(5)), Some(1)).unwrap().count, 5);
        // At or below baseline there is no newly-added debt to sweep.
        assert!(lint_delta(Some(&report(3)), Some(3)).is_none());
        assert!(lint_delta(Some(&report(2)), Some(3)).is_none());
        // No baseline recorded means any positive tally is a regression floor of zero.
        assert!(lint_delta(None, Some(0)).is_none());
        assert!(lint_delta(None, None).is_none());
    }

    #[test]
    fn assemble_signals_files_only_real_debt_and_acceptance_lines_name_counts() {
        let signals = assemble_signals(Some(&report(7)), Some(2), 4);
        assert_eq!(signals.len(), 2);
        assert_eq!(
            signals.first().unwrap(),
            &DebtSignal::new(DebtSignalKind::LintRegression, 7)
        );
        assert_eq!(
            signals.get(1).unwrap(),
            &DebtSignal::new(DebtSignalKind::MissingModuleDocs, 4)
        );

        let lines = acceptance_lines(&signals);
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0].contains("below 7"),
            "criterion should name the current lint tally"
        );
        assert!(
            lines[1].contains("4 source files"),
            "criterion should name the module-doc count"
        );

        // A clean state yields no signals and therefore no criteria.
        let clean = assemble_signals(Some(&report(1)), Some(5), 0);
        assert!(clean.is_empty());
    }

    #[test]
    fn modules_with_an_inner_doc_header_or_attributes_are_not_counted_as_missing() {
        let documented = [
            ("a.rs", "//! Module docs here.\nfn f() {}"),
            (
                "b.rs",
                "#![allow(unused)]\n//! Docs after an attribute.\nfn f() {}",
            ),
            ("c.rs", "\n\n //!\nmod inner_docs;\npub fn f() {}"),
            (
                "d.rs",
                "#[cfg(test)]\npub mod t {\npub fn helper() {}\n}\npub fn f() {}",
            ),
            (
                "e.rs",
                "\u{feff}//! Docs even after a byte-order mark.\npub fn f() {}",
            ),
            ("f.rs", "#![deny(warnings)]\npub mod x;\npub fn f() {}"),
        ];
        for (path, src) in documented {
            assert!(
                starts_with_module_doc(src),
                "{path} should not be flagged missing"
            );
        }
        let missing = [
            ("g.rs", "pub fn f() {}\n"),
            ("h.rs", "use std::fmt;\npub struct S;\n"),
            (
                "i.rs",
                "/// Outer doc is NOT a module header.\npub fn f() {}\n",
            ),
            ("j.rs", ""),
        ];
        for (path, src) in missing {
            assert!(
                !starts_with_module_doc(src),
                "{path} should be flagged missing"
            );
        }

        assert_eq!(count_modules_missing_docs(documented.iter().copied()), 0);
        assert_eq!(
            count_modules_missing_docs(missing.iter().copied()),
            missing.len()
        );
    }
}
