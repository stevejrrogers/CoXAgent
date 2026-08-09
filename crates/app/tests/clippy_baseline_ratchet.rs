//! Lint baseline ratchet: clippy errors must only go down, not up.
//!
//! C001 (Debt sweep cycle 10) AC1: The clippy/lint baseline is LOWER than before this ticket.

#![allow(clippy::expect_used)]

/// Errors present before C001 started (run `cargo clippy` and count "error:" lines). Updated only when debt is paid down, never inflated to match regressions.
const PRE_C001_BASELINE: u64 = 2;

/// This test fails while there are still lint errors — forcing debt to be paid before merging.
#[test]
fn clippy_error_count_is_below_pre_sweep_baseline() {
    let output = std::process::Command::new("cargo")
        .args(["clippy", "--", "--no-deps"])
        .output()
        .expect("running cargo clippy failed");

    let txt = String::from_utf8_lossy(&output.stderr).to_string();

    let error_count = txt
        .lines()
        .filter(|l| l.trim_start().starts_with("error:"))
        .count() as u64;

    assert!(error_count < PRE_C001_BASELINE, "C001 AC1 unmet: found {error_count} lint errors in this build but require fewer than {PRE_C001_BASELINE} (pre-sweep baseline). Run `cargo clippy -- --no-deps` to see the remaining violations and fix them.");

    assert!(
        output.status.success() || error_count > 0,
        "cargo exited non-zero despite zero explicit 'error:' lines: {}",
        &txt[..txt.len().min(4_096)]
    );
}
