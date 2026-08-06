//! Repo-wide platform-gate validator — the COX-B006/COX-B008 regression guard.
//!
//! The bug: `fn seatbelt_profile` (crates/infrastructure/src/proc.rs) was
//! called only from a `#[cfg(target_os = "macos")]` block but carried no gate
//! of its own. On Linux it compiled as unreachable code, and the workspace's
//! `warnings = "deny"` (Cargo.toml) turned the resulting `dead_code` warning
//! into `error: could not compile ...`. That killed the Docker builder — this
//! repo's only documented deploy path — so no image was produced and nothing
//! ever answered on the published host port.
//!
//! Why the guard reads source instead of compiling: the failure is invisible
//! to `cargo test` (the `test` cfg keeps the item alive) and to a macOS dev
//! box (the item IS reachable there). Only a Linux, non-test build sees it.
//! Asserting on the source catches the landmine from any host, before it
//! reaches CI. `deploy_smoke` covers the other half — that the image actually
//! builds and the app comes up.
//!
//! Scope: private (non-`pub`) top-level `fn`s, whose call sites are all in the
//! same file, which is exactly the class of item that goes dead when a caller
//! is platform-gated and the callee is not.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

/// The `#[cfg(...)]` gate attached to the top-level definition of `name`, or
/// `None` when that definition carries no gate.
fn cfg_gate_above(src: &str, name: &str) -> Option<String> {
    let lines: Vec<&str> = src.lines().collect();
    let def = format!("fn {name}(");
    // Top-level definitions start at column 0; call sites are indented.
    let at = lines.iter().position(|l| l.starts_with(&def))?;
    lines[..at]
        .iter()
        .rev()
        .take_while(|l| {
            let t = l.trim_start();
            t.starts_with("#[") || t.starts_with("//") || t.is_empty()
        })
        .find(|l| l.trim_start().starts_with("#[cfg("))
        .map(|l| (*l).to_owned())
}

/// The `#[cfg(...)]` attributes enclosing each source line, innermost last. A
/// `#[cfg(...)]` applies to the next item or block, so the gate is pushed when
/// the following line opens a brace and popped when that brace closes — enough
/// structure to tell "called from the macOS branch" from "called
/// unconditionally" without pulling in a parser.
fn cfg_scopes(src: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut pending: Option<String> = None;
    let mut stack: Vec<(i32, String)> = Vec::new();
    for line in src.lines() {
        out.push(stack.iter().map(|(_, g)| g.clone()).collect());
        let t = line.trim();
        if t.starts_with("#[cfg(") {
            pending = Some(t.to_owned());
            continue;
        }
        // Other attributes and comments sit between the gate and its item.
        if t.starts_with('#') || t.starts_with("//") || t.is_empty() {
            continue;
        }
        let opens = i32::try_from(line.matches('{').count()).unwrap();
        let closes = i32::try_from(line.matches('}').count()).unwrap();
        if opens > closes {
            if let Some(gate) = pending.take() {
                stack.push((depth, gate));
            }
        }
        depth += opens - closes;
        while stack.last().is_some_and(|(d, _)| depth <= *d) {
            stack.pop();
        }
        pending = None;
    }
    out
}

/// The `target_os` values named by a `#[cfg(...)]` attribute, e.g.
/// `["macos", "linux"]` for `#[cfg(any(target_os = "macos", target_os =
/// "linux"))]`. Empty when the gate is not platform-specific.
fn targets_in(gate: &str) -> Vec<String> {
    const KEY: &str = "target_os = \"";
    gate.match_indices(KEY)
        .filter_map(|(at, _)| {
            let rest = &gate[at + KEY.len()..];
            rest.find('"').map(|end| rest[..end].to_owned())
        })
        .collect()
}

/// The single `target_os` every non-test call site of `name` sits under, or
/// `None` when it is reachable unconditionally or from more than one platform
/// (then it is compiled and used everywhere, so no gate is required).
fn only_platform_calling(
    lines: &[&str],
    scopes: &[Vec<String>],
    name: &str,
    def_at: usize,
) -> Option<String> {
    let call = format!("{name}(");
    let mut targets: Vec<String> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i == def_at || !line.contains(&call) || line.trim_start().starts_with("//") {
            continue;
        }
        // A `cfg(test)` region keeps nothing alive in a release build.
        if scopes[i].iter().any(|g| g.contains("test")) {
            continue;
        }
        let Some(gate) = scopes[i].iter().rev().find(|g| !targets_in(g).is_empty()) else {
            // Called from unconditional code: alive on every platform.
            return None;
        };
        targets.extend(targets_in(gate));
    }
    targets.sort_unstable();
    targets.dedup();
    match targets.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// Names of the private helpers in `src` that this file checked, plus the
/// gate violations found. A violation is a helper reachable from exactly one
/// platform whose definition is missing (or contradicts) that gate.
fn scan(src: &str) -> (Vec<String>, Vec<String>) {
    let lines: Vec<&str> = src.lines().collect();
    let scopes = cfg_scopes(src);
    let (mut checked, mut violations) = (Vec::new(), Vec::new());

    for (at, def) in lines.iter().enumerate() {
        // Private top-level definitions only: `pub` items are never dead
        // code, and call sites are indented.
        let Some(name) = def
            .strip_prefix("fn ")
            .and_then(|rest| rest.split(['(', '<']).next())
        else {
            continue;
        };
        let Some(target) = only_platform_calling(&lines, &scopes, name, at) else {
            continue;
        };
        match cfg_gate_above(src, name) {
            None => violations.push(format!(
                "`fn {name}` is only called from {target}-gated code but has no \
                 #[cfg(...)] gate: it becomes dead code on other targets and \
                 `warnings = \"deny\"` fails the Docker (Linux) release build"
            )),
            Some(gate) if !gate.contains(&format!("target_os = \"{target}\"")) => {
                violations.push(format!(
                    "`fn {name}` must be gated on target_os = \"{target}\", found: {gate}"
                ));
            }
            Some(_) => {}
        }
        checked.push(name.to_owned());
    }
    (checked, violations)
}

/// Every Rust source file under the workspace's `crates/`.
fn rust_sources() -> Vec<PathBuf> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut out = Vec::new();
    let mut stack = vec![crates];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn single_platform_helpers_are_cfg_gated() {
    let mut checked: Vec<String> = Vec::new();
    let mut violations: Vec<String> = Vec::new();

    for path in rust_sources() {
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let (file_checked, file_violations) = scan(&src);
        checked.extend(file_checked);
        violations.extend(
            file_violations
                .into_iter()
                .map(|v| format!("{}: {v}", path.display())),
        );
    }

    assert!(violations.is_empty(), "{}", violations.join("\n"));

    // The scan must not quietly degrade into a no-op if the parsing
    // heuristics drift: COX-B006's helper is the canary.
    assert!(
        checked.contains(&"seatbelt_profile".to_owned()),
        "gate scan found no platform-only helpers (checked: {checked:?}) — \
         the guard itself is broken"
    );
}

#[test]
fn scan_flags_an_ungated_single_platform_helper() {
    let src = "\
#[cfg(target_os = \"macos\")]
fn confined(program: &str) -> String {
    profile(program)
}

fn profile(program: &str) -> String {
    program.to_owned()
}
";
    let (_, violations) = scan(src);
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(violations[0].contains("`fn profile`"), "{violations:?}");

    let gated = src.replace("fn profile", "#[cfg(target_os = \"macos\")]\nfn profile");
    assert!(
        scan(&gated).1.is_empty(),
        "a helper gated on the platform that calls it is fine"
    );
}
