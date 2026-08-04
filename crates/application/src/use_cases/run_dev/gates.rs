//! The mechanical Definition-of-Done gates, as PURE functions over a
//! [`WorkingTreeDiff`] snapshot.
//!
//! These used to shell out to `git` from inside the application layer — the
//! one place the architecture says IO must go through a port. That did more
//! than bend a rule: it meant every gate test had to build a real git repo in
//! a temp directory, which is exactly the kind of test that flakes under a
//! parallel run. The adapter now takes the snapshot once
//! ([`crate::ports::outbound::GitPort::working_tree`]); the decisions here are
//! plain functions of it, testable with a struct literal.

use crate::ports::outbound::WorkingTreeDiff;

/// Whether any lint location sits in a file this working diff touches.
/// Paths are compared by suffix so a repo-relative lint path still matches
/// a git path listed from the same root.
#[must_use]
pub(super) fn lints_touch_changed_files(tree: &WorkingTreeDiff, lint_files: &[String]) -> bool {
    if tree.changed_paths.is_empty() {
        // Nothing changed on disk — nothing here is attributable.
        return false;
    }
    lint_files.iter().any(|lint| {
        let lint = lint.trim();
        !lint.is_empty()
            && tree
                .changed_paths
                .iter()
                .any(|c| lint.ends_with(c.as_str()) || c.ends_with(lint))
    })
}

/// Whether the working diff is documentation/assets only — README fixes,
/// docs, images, licences. Such a "bug fix" has no runtime surface, so
/// demanding a regression test just parks the ticket.
#[must_use]
pub(super) fn diff_is_docs_only(tree: &WorkingTreeDiff) -> bool {
    let mut any = false;
    for f in &tree.changed_paths {
        any = true;
        let lower = f.to_lowercase();
        let doc_ext = [
            ".md", ".txt", ".adoc", ".rst", ".png", ".jpg", ".jpeg", ".svg", ".gif",
        ]
        .iter()
        .any(|e| lower.ends_with(e));
        let doc_name = lower.ends_with("license") || lower.ends_with(".gitignore");
        let doc_dir = lower.starts_with("docs/") || lower.contains("/docs/");
        if !(doc_ext || doc_name || doc_dir) {
            return false;
        }
    }
    any
}

/// Whether the working diff touches tests: a test-ish path, an added line
/// carrying a test marker, or a change inside a file's `#[cfg(test)]` module.
#[must_use]
pub(super) fn diff_touches_tests(tree: &WorkingTreeDiff) -> bool {
    if tree.changed_paths.iter().any(|f| {
        let f = f.to_lowercase();
        f.contains("/tests/")
            || f.starts_with("tests/")
            || f.ends_with("_test.rs")
            || f.ends_with("_test.go")
            || f.ends_with(".test.ts")
            || f.ends_with(".test.js")
            || f.contains("test_")
    }) {
        return true;
    }
    if tree.full_diff.lines().any(|l| {
        l.starts_with('+')
            && (l.contains("#[test]")
                || l.contains("#[tokio::test]")
                || l.contains("def test_")
                || l.contains("it(")
                || l.contains("func Test"))
    }) {
        return true;
    }
    // Rust keeps most tests in an inline `#[cfg(test)] mod tests` at the foot
    // of the file it tests. A fix that hardens or extends one of those adds no
    // `#[test]` line and lives in no test-shaped path, so the two checks above
    // miss the single most common way a Rust regression test actually lands —
    // and the ticket gets failed for shipping without one.
    diff_touches_inline_test_module(tree)
}

/// Whether any changed line falls at or below its file's `#[cfg(test)]`
/// marker. The `-U0` diff's line numbers are the changed lines themselves,
/// not context that happens to sit near the boundary.
fn diff_touches_inline_test_module(tree: &WorkingTreeDiff) -> bool {
    let mut file: Option<&str> = None;
    for line in tree.unified0_diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            file = Some(rest.trim());
            continue;
        }
        let Some(hunk) = line.strip_prefix("@@ ") else {
            continue;
        };
        let Some(f) = file else { continue };
        // `@@ -a,b +c,d @@` — c is the first changed line on the new side.
        let Some(new_side) = hunk.split('+').nth(1) else {
            continue;
        };
        let Ok(start) = new_side
            .split([',', ' '])
            .next()
            .unwrap_or("")
            .parse::<usize>()
        else {
            continue;
        };
        if let Some(test_mod_line) = tree.cfg_test_line.get(f) {
            // Diff line numbers are 1-based; the map is 0-based from position().
            if start > *test_mod_line {
                return true;
            }
        }
    }
    false
}

/// Directories and files that appear in `git status` but cannot change what
/// the build produces: agent scratch space, backups, per-engine config the
/// runner writes itself, vendored/build output.
///
/// This matters more than it looks. A workspace with only these dirty made the
/// boot check believe the tree was modified; nothing mapped to a package, so
/// the scoped run fell back to the FULL suite, timed out, and the failure was
/// then read as "the project does not compile" — spawning a self-heal on a
/// tree where not one source file had changed.
const BUILD_IRRELEVANT: &[&str] = &[
    ".claude/",
    ".gitnexus/",
    "backups/",
    "node_modules/",
    "target/",
    "cox-opencode.json",
    "cox-config.json",
    ".DS_Store",
];

/// The changed paths that could actually affect a build or its tests.
#[must_use]
pub(super) fn build_relevant(changed: &[String]) -> Vec<String> {
    changed
        .iter()
        .filter(|p| {
            let p = p.trim_start_matches("./");
            !BUILD_IRRELEVANT
                .iter()
                .any(|skip| p.starts_with(skip) || p.contains(&format!("/{skip}")) || p == *skip)
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_scratch_never_counts_as_a_build_change() {
        let changed: Vec<String> = [
            ".claude/worktrees/",
            "backups/",
            "cox-opencode.json",
            "target/debug/foo",
            "crates/app/src/lib.rs",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        assert_eq!(build_relevant(&changed), vec!["crates/app/src/lib.rs"]);
    }

    #[test]
    fn a_tree_dirty_only_with_scratch_is_effectively_clean() {
        let changed: Vec<String> = [".claude/worktrees/", "backups/db.sql"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        assert!(build_relevant(&changed).is_empty());
    }

    fn tree(paths: &[&str]) -> WorkingTreeDiff {
        WorkingTreeDiff {
            changed_paths: paths.iter().map(|s| (*s).to_owned()).collect(),
            ..WorkingTreeDiff::default()
        }
    }

    #[test]
    fn docs_only_spares_readme_fixes_but_not_code() {
        assert!(
            !diff_is_docs_only(&tree(&[])),
            "empty diff is not docs-only"
        );
        assert!(diff_is_docs_only(&tree(&["README.md", "docs/setup.md"])));
        assert!(
            !diff_is_docs_only(&tree(&["README.md", "src/lib.rs"])),
            "any code file breaks the exemption"
        );
    }

    #[test]
    fn test_paths_and_added_markers_count_as_tests() {
        assert!(diff_touches_tests(&tree(&["crates/app/tests/gate.rs"])));
        let mut t = tree(&["src/lib.rs"]);
        assert!(!diff_touches_tests(&t), "a plain code change is not a test");
        t.full_diff = "+#[test]\n+fn t() {}\n".to_owned();
        assert!(diff_touches_tests(&t), "an added #[test] counts");
    }

    #[test]
    fn editing_an_existing_inline_test_counts_changing_production_does_not() {
        // The case that cost cox four attempts and part of a budget cap: a fix
        // inside `#[cfg(test)] mod tests` adds no #[test] line and sits in no
        // test-shaped path, but it IS a test change.
        let mut t = tree(&["src/engine.rs"]);
        t.cfg_test_line.insert("src/engine.rs".to_owned(), 10);
        t.unified0_diff =
            "+++ b/src/engine.rs\n@@ -14,1 +14,1 @@\n-let timeout = 10;\n+let timeout = 30;\n"
                .to_owned();
        assert!(diff_touches_tests(&t), "line 14 is below the marker at 10");
        t.unified0_diff =
            "+++ b/src/engine.rs\n@@ -3,1 +3,1 @@\n-let x = 0;\n+let x = 1;\n".to_owned();
        assert!(
            !diff_touches_tests(&t),
            "line 3 is above the test module — production code"
        );
    }

    #[test]
    fn lint_blame_stays_inside_the_files_the_change_touched() {
        let t = tree(&["crates/app/src/lib.rs"]);
        assert!(lints_touch_changed_files(
            &t,
            &["crates/app/src/lib.rs".to_owned()]
        ));
        assert!(
            !lints_touch_changed_files(&t, &["crates/domain/src/ticket.rs".to_owned()]),
            "a lint somewhere else — e.g. pulled in by a rebase — is not ours"
        );
        assert!(
            !lints_touch_changed_files(&tree(&[]), &["src/lib.rs".to_owned()]),
            "a clean tree can't have caused any lint"
        );
    }
}
