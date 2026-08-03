//! Which tests could this change possibly break?
//!
//! A per-ticket gate that runs the WHOLE suite spends minutes proving things
//! nobody touched. This module turns a list of changed paths into a narrower
//! test command per toolchain — and returns `None` whenever it cannot be sure,
//! so the caller falls back to running everything. Being wrong here means
//! shipping an untested change, so every rule is deliberately conservative.
//!
//! Pure: a path list in, a command out. No filesystem beyond reading the
//! manifests that define a package boundary.

use std::collections::BTreeSet;
use std::path::Path;

/// Paths whose change invalidates any narrowing — build config, CI, lockfiles,
/// workspace manifests. Touch one of these and the full suite runs.
fn is_global(path: &str) -> bool {
    let p = path.to_lowercase();
    p.ends_with("cargo.lock")
        || p.ends_with("package-lock.json")
        || p.ends_with("go.sum")
        || p.ends_with("poetry.lock")
        || p.starts_with(".github/")
        || p.starts_with("scripts/")
        || p.contains("dockerfile")
        || p.contains("docker-compose")
        || p == "cargo.toml"
        || p == "package.json"
        || p == "go.mod"
        || p == "pyproject.toml"
}

/// The narrowed command for `changed`, or `None` to run everything.
#[must_use]
pub fn scoped_test_command(work_dir: &Path, changed: &[String]) -> Option<(String, Vec<String>)> {
    if changed.is_empty() || changed.iter().any(|c| is_global(c)) {
        return None;
    }
    let has = |f: &str| work_dir.join(f).exists();
    if has("Cargo.toml") {
        return rust_scope(work_dir, changed);
    }
    if has("go.mod") {
        return go_scope(changed);
    }
    if has("pyproject.toml") || has("pytest.ini") || has("requirements.txt") {
        return python_scope(changed);
    }
    if has("package.json") {
        return node_scope(work_dir, changed);
    }
    None
}

/// Rust: map each changed file to the crate that owns it (nearest ancestor
/// with a `Cargo.toml`) and test just those with `-p`.
fn rust_scope(work_dir: &Path, changed: &[String]) -> Option<(String, Vec<String>)> {
    let mut crates: BTreeSet<String> = BTreeSet::new();
    for c in changed {
        let mut dir = work_dir.join(c);
        dir.pop();
        loop {
            if dir.join("Cargo.toml").exists() {
                // The workspace root manifest is not a package boundary.
                if dir == work_dir {
                    return None;
                }
                let name = crate_name(&dir.join("Cargo.toml"))?;
                crates.insert(name);
                break;
            }
            if !dir.pop() || !dir.starts_with(work_dir) {
                return None; // outside the workspace: do not guess
            }
        }
    }
    if crates.is_empty() || crates.len() > 4 {
        return None; // a change spanning most of the tree IS the full suite
    }
    let mut args = vec!["test".to_owned(), "--quiet".to_owned()];
    for c in crates {
        args.push("-p".to_owned());
        args.push(c);
    }
    Some(("cargo".to_owned(), args))
}

/// `name = "..."` from the `[package]` section, or `None` when the manifest is
/// a virtual workspace.
fn crate_name(manifest: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let mut in_package = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_package = l == "[package]";
            continue;
        }
        if in_package {
            if let Some(rest) = l.strip_prefix("name") {
                let v = rest.trim_start_matches([' ', '=']).trim();
                return Some(v.trim_matches(['"', '\'']).to_owned());
            }
        }
    }
    None
}

/// Go: test the packages (directories) that changed, plus nothing else.
fn go_scope(changed: &[String]) -> Option<(String, Vec<String>)> {
    let mut pkgs: BTreeSet<String> = BTreeSet::new();
    for c in changed {
        if !std::path::Path::new(c)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("go"))
        {
            return None;
        }
        let dir = c.rsplit_once('/').map_or(".".to_owned(), |(d, _)| d.to_owned());
        pkgs.insert(format!("./{dir}"));
    }
    if pkgs.is_empty() || pkgs.len() > 6 {
        return None;
    }
    let mut args = vec!["test".to_owned()];
    args.extend(pkgs);
    Some(("go".to_owned(), args))
}

/// Python: hand pytest the changed directories; it collects the tests there.
fn python_scope(changed: &[String]) -> Option<(String, Vec<String>)> {
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    for c in changed {
        if !std::path::Path::new(c)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("py"))
        {
            return None;
        }
        dirs.insert(c.rsplit_once('/').map_or(".".to_owned(), |(d, _)| d.to_owned()));
    }
    if dirs.is_empty() || dirs.len() > 6 {
        return None;
    }
    let mut args = vec!["-q".to_owned()];
    args.extend(dirs);
    Some(("pytest".to_owned(), args))
}

/// Node: only when the project declares how to scope. Jest and Vitest both
/// support related-file selection, but guessing the runner from package.json
/// is how a "fast" gate silently tests nothing — so the project opts in with
/// a `test:related` script that receives the changed files.
fn node_scope(work_dir: &Path, changed: &[String]) -> Option<(String, Vec<String>)> {
    let pkg = std::fs::read_to_string(work_dir.join("package.json")).ok()?;
    if !pkg.contains("\"test:related\"") {
        return None;
    }
    let mut args = vec!["run".to_owned(), "test:related".to_owned(), "--".to_owned()];
    args.extend(changed.iter().cloned());
    Some(("npm".to_owned(), args))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tmp");
        let root = dir.path();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers=[\"crates/*\"]\n").expect("w");
        for (name, pkg) in [("app", "coxagent-app"), ("domain", "coxagent-domain")] {
            let d = root.join("crates").join(name).join("src");
            std::fs::create_dir_all(&d).expect("mk");
            std::fs::write(
                root.join("crates").join(name).join("Cargo.toml"),
                format!("[package]\nname = \"{pkg}\"\nversion = \"0.1.0\"\n"),
            )
            .expect("w");
            std::fs::write(d.join("lib.rs"), "").expect("w");
        }
        dir
    }

    #[test]
    fn rust_change_tests_only_the_crates_it_touched() {
        let ws = workspace();
        let (cmd, args) = scoped_test_command(
            ws.path(),
            &["crates/app/src/lib.rs".to_owned()],
        )
        .expect("scoped");
        assert_eq!(cmd, "cargo");
        assert_eq!(
            args,
            vec!["test", "--quiet", "-p", "coxagent-app"]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn two_crates_become_two_package_flags() {
        let ws = workspace();
        let (_, args) = scoped_test_command(
            ws.path(),
            &[
                "crates/app/src/lib.rs".to_owned(),
                "crates/domain/src/lib.rs".to_owned(),
            ],
        )
        .expect("scoped");
        assert_eq!(args.iter().filter(|a| *a == "-p").count(), 2);
    }

    #[test]
    fn build_and_ci_changes_refuse_to_narrow() {
        let ws = workspace();
        for global in [
            "Cargo.lock",
            ".github/workflows/ci.yml",
            "Dockerfile",
            "scripts/build.sh",
            "Cargo.toml",
        ] {
            assert!(
                scoped_test_command(ws.path(), &[global.to_owned()]).is_none(),
                "{global} must fall back to the full suite"
            );
        }
    }

    #[test]
    fn an_empty_or_sprawling_change_runs_everything() {
        let ws = workspace();
        assert!(scoped_test_command(ws.path(), &[]).is_none());
        let many: Vec<String> = (0..9)
            .map(|i| format!("crates/c{i}/src/lib.rs"))
            .collect();
        assert!(scoped_test_command(ws.path(), &many).is_none());
    }

    #[test]
    fn go_and_python_scope_by_directory() {
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::write(dir.path().join("go.mod"), "module x\n").expect("w");
        let (cmd, args) =
            scoped_test_command(dir.path(), &["pkg/auth/token.go".to_owned()]).expect("go");
        assert_eq!((cmd.as_str(), args.as_slice()), ("go", &["test".to_owned(), "./pkg/auth".to_owned()][..]));

        let py = tempfile::tempdir().expect("tmp");
        std::fs::write(py.path().join("pyproject.toml"), "[project]\n").expect("w");
        let (cmd, args) =
            scoped_test_command(py.path(), &["app/svc/user.py".to_owned()]).expect("py");
        assert_eq!(cmd, "pytest");
        assert!(args.contains(&"app/svc".to_owned()));
    }

    #[test]
    fn node_narrows_only_when_the_project_opted_in() {
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::write(dir.path().join("package.json"), "{\"scripts\":{\"test\":\"jest\"}}")
            .expect("w");
        assert!(scoped_test_command(dir.path(), &["src/a.js".to_owned()]).is_none());
        std::fs::write(
            dir.path().join("package.json"),
            "{\"scripts\":{\"test\":\"jest\",\"test:related\":\"jest --findRelatedTests\"}}",
        )
        .expect("w");
        let (cmd, args) =
            scoped_test_command(dir.path(), &["src/a.js".to_owned()]).expect("node");
        assert_eq!(cmd, "npm");
        assert!(args.ends_with(&["src/a.js".to_owned()]));
    }
}
