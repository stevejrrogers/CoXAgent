# COX-B008: Docker Release Build Fails on Linux Dead-Code Warning

**Keywords:** dead code, platform gate, Linux CI, Docker build, Rust warnings, seatbelt, macOS-only helpers, cfg attribute, compilation error

## Overview

The Docker release build and Linux CI fail during `cargo build --release` with a compile error: `error: function seatbelt_profile is never used`. The root cause is that `seatbelt_profile` is only called from a macOS-only code path, but the function definition carries no platform guard. On Linux, the compiler marks it as unreachable; when `warnings = "deny"` is set globally, this dead-code warning becomes a fatal error. The Docker builder exits with code 101 before producing an image, so the app never comes up on the published host port.

**Who it affects:** Anyone building CoXAgent with `cargo build --release` on Linux or in CI pipelines, including the primary Docker self-host deployment path.

**Root cause:** Helper functions that are called only from platform-specific code paths (e.g., inside `#[cfg(target_os = "macos")]` blocks) must themselves carry a matching `#[cfg(...)]` gate. Without it, the caller is unreachable on other platforms, leaving the helper as dead code. A global `warnings = "deny"` setting (Cargo.toml) upgrades this to a compilation error, blocking the entire build.

---

## How It Works

### Platform-Gated Code Paths

CoXAgent runs on macOS (Seatbelt confinement), Linux (Bubblewrap confinement), and both use shared Rust code. Confinement mechanisms differ significantly:

- **macOS:** Seatbelt (`sandbox-exec`) profile generation requires `seatbelt_profile()` to format a policy string
- **Linux:** Bubblewrap arguments are built differently; `seatbelt_profile()` is never called
- **Tests:** Both macOS and Linux test runs need to exercise `seatbelt_profile()` to verify the policy format is correct

The `confined_command()` function branches at compile time:

```rust
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn confined_command(program: impl AsRef<OsStr>, work_dir: &Path) -> Command {
    #[cfg(target_os = "macos")]
    {
        let profile = seatbelt_profile(&sandbox_writable(work_dir));  // ← macOS only
        // ... build sandbox-exec command ...
    }
    #[cfg(target_os = "linux")]
    {
        // ... build bwrap command, no seatbelt_profile call ...
    }
}
```

### The Bug

`seatbelt_profile` originally had no guard:

```rust
fn seatbelt_profile(writable: &[PathBuf]) -> String {  // ← NO GUARD
    // ... format policy ...
}
```

**On macOS:** The function is reachable (called from `confined_command`'s macOS block), so no warning.

**On Linux (release build):** 
1. The `confined_command` macOS branch is compiled away
2. The call site disappears
3. `seatbelt_profile` remains in the compilation unit but is unreachable
4. Compiler issues a `dead_code` warning
5. `warnings = "deny"` in Cargo.toml escalates this to `error`
6. Build fails; Docker image is not produced; app never comes up

**In test mode:** The function is reachable from unit tests (`seatbelt_profile_denies_outside_allowlist`), so no warning even on Linux. This masked the bug during local and test-mode validation.

### The Fix

Add a matching `#[cfg(...)]` gate to the function definition so it only compiles on macOS or during tests:

```rust
#[cfg(any(test, target_os = "macos"))]
fn seatbelt_profile(writable: &[PathBuf]) -> String {
    // ... now only compiled when needed ...
}
```

Now:
- **macOS release:** Compiled (needed by `confined_command`)
- **Linux release:** Not compiled (not needed; no dead-code warning)
- **Linux test:** Compiled (test cfg includes it; unit test runs)
- **macOS test:** Compiled (macOS cfg includes it; unit test runs)

### Regression Prevention

The `platform_gates.rs` test scans the Rust source code to verify that helper functions called only from platform-specific blocks are themselves gated. It:

1. Parses each `.rs` file to find top-level `fn` definitions
2. For each definition, checks if it carries a `#[cfg(...)]` gate
3. For each call site, checks if it is inside a `#[cfg(...)]` block
4. Reports an error if a caller's gate is stricter than the callee's (dead-code risk)

The test is a canary: it uses `seatbelt_profile` as the sentinel to ensure the parser is working. If the scan stops detecting platform-gated helpers, the test fails immediately.

---

## Usage

### For Developers

When writing helpers that are called only from a platform-specific code path:

1. **Add the gate to the helper definition** matching the caller's platform:

```rust
// Only called from #[cfg(target_os = "macos")] block
#[cfg(target_os = "macos")]
fn helper_for_macos(arg: &str) -> String {
    // ...
}
```

2. **Add test to the gate** so tests on all platforms can exercise it:

```rust
// Called from tests on all platforms; available in test mode everywhere
#[cfg(any(test, target_os = "macos"))]
fn seatbelt_profile(writable: &[PathBuf]) -> String {
    // ...
}
```

3. **Verify the gate covers all platforms:** Check the comment above the function to document which platforms need it and why.

### For Debugging a Dead-Code Error

If you see `error: function X is never used` during `cargo build --release`:

1. **Find the function definition** (search for `fn X(`)
2. **Identify where it's called** (search for `X(`)
3. **Check if all call sites are inside `#[cfg(...)]` blocks**
4. **If yes:** Add a matching `#[cfg(...)]` gate to the definition
5. **If no:** The function truly is unused; delete it or add a `#[allow(dead_code)]` attribute if it's intentional

### Building on Linux

Ensure the release build works on Linux:

```bash
# Full compile check
cargo build --release

# Or target Linux specifically
cargo build --release --target x86_64-unknown-linux-gnu
```

Both should complete without dead-code errors. If they do not, run the `platform_gates` test to identify which helper needs a gate:

```bash
cargo test -p coxagent-app platform_gates
```

---

## Interface

### Function Gate

```rust
#[cfg(any(test, target_os = "macos"))]
fn seatbelt_profile(writable: &[PathBuf]) -> String
```

**Gate:** `#[cfg(any(test, target_os = "macos"))]`
- Compiles on: macOS (always), Linux (test mode only)
- Does not compile on: Linux release builds

**Visibility:** Private (`fn`, not `pub fn`) — only called within the same file.

**Callers:**
- `confined_command()` (line 235, inside `#[cfg(target_os = "macos")]` block)
- `seatbelt_profile_denies_outside_allowlist()` unit test (line 443)

### Regression Gate

```rust
#[test]
fn platform_gates_are_properly_attached() {
    // Scans all Rust source files for helpers gated on a narrower platform
    // than their callers. Fails if any platform-specific helper is ungated.
}
```

**Checks:** Every top-level `fn` definition in the source tree for missing `#[cfg(...)]` gates when all call sites are platform-specific.

**Sentinel:** `seatbelt_profile` — the gate passes only if this function is detected and verified to have a gate.

---

## Configuration

### Cargo.toml Global Setting

```toml
[lints.rust]
warnings = "deny"
```

This setting forces the compiler to treat all warnings as errors. It catches dead-code regressions early:

- **On:** Compilation fails if unreachable code is left behind
- **Off:** Dead-code warnings are printed but don't block the build

### Platform Detection

Rust's built-in `#[cfg(...)]` attributes determine which code compiles:

```rust
#[cfg(target_os = "macos")]     // Compiles only on macOS
#[cfg(target_os = "linux")]     // Compiles only on Linux
#[cfg(test)]                    // Compiles in test mode (any platform)
#[cfg(any(...))]                // Compiles if ANY condition is true
```

For CoXAgent:

| Gate | Platforms | Use Case |
|------|-----------|----------|
| `#[cfg(target_os = "macos")]` | macOS only | Seatbelt (macOS-specific sandbox mechanism) |
| `#[cfg(target_os = "linux")]` | Linux only | Bubblewrap (Linux-specific sandbox mechanism) |
| `#[cfg(test)]` | All (test mode) | Unit tests that exercise platform-specific logic |
| `#[cfg(any(test, target_os = "macos"))]` | macOS + test mode | Helpers needed by both macOS code AND tests |

---

## Edge Cases and Limits

### Dead Code Masking in Test Mode

Unit tests always compile in test mode, which includes `#[cfg(test)]` items. This masks unreachable-code bugs:

```rust
#[cfg(target_os = "macos")]
fn confined_command(...) { seatbelt_profile(...); }

fn seatbelt_profile(...) { }  // ← No gate; compile-time error on Linux release

// But: cargo test --release (on Linux) succeeds because #[cfg(test)] includes seatbelt_profile
```

**Lesson:** Always test release builds explicitly:

```bash
cargo build --release        # Catches dead code on this platform
cargo test --release         # Tests in release mode (separate from build)
```

### Platform-Specific Tests Must Be Gated Too

```rust
#[test]
#[cfg(target_os = "macos")]
fn test_seatbelt_confinement() {
    // This test only runs on macOS
}

#[test]
fn test_seatbelt_profile_format() {
    // This runs on all platforms (test cfg includes seatbelt_profile)
    let p = seatbelt_profile(&[...]);
}
```

### Silent Failures in CI

Docker builds and Linux CI pipelines run on Linux release mode. Dead-code errors appear **only** in these environments:

- **Local macOS dev:** No error (seatbelt_profile is reachable)
- **Local Linux dev:** No error (test cfg includes seatbelt_profile)
- **Docker build:** Fatal error; image not produced
- **Linux CI:** Fatal error; run fails

This is why regression gates like `platform_gates.rs` are essential—they catch the error on any host before deployment.

### Manual Allow Attributes Are Not a Solution

```rust
#[allow(dead_code)]
fn seatbelt_profile(...) { }
```

This silences the warning but does NOT fix the underlying problem. The function is still dead code on Linux, and if someone later adds a Linux caller, the bug resurfaces. Always use `#[cfg(...)]` instead.

---

## Code Map

- `crates/infrastructure/src/proc.rs` — `seatbelt_profile()` function (line 143), `confined_command()` caller (line 232-245), unit test `seatbelt_profile_denies_outside_allowlist` (line 443)
- `crates/app/tests/platform_gates.rs` — Regression guard; scans source for ungated platform-specific helpers, uses `seatbelt_profile` as canary
- `crates/app/tests/deploy_smoke.rs` — Docker build smoke test; verifies image builds and app comes up (references the COX-B006 error message as a regression marker)
- `Cargo.toml` — Global `warnings = "deny"` setting that escalates dead-code warnings to errors
- `.github/workflows/ci.yml` (if present) — Linux CI pipeline that would catch this during automated builds

---

## Related

- [COX-B006](COX-B006.md) — Seatbelt confinement implementation; the first-pass guard that added `#[cfg(...)]` to `seatbelt_profile`
- [COX-B002](COX-B002.md) — Sandboxing silent no-ops; documents the broader sandboxing architecture and why Seatbelt/Bubblewrap matter
- [COX-B004](COX-B004.md) — Post-deploy health gate; verifies the running app actually bound its port (assumes image built successfully)
- `crates/infrastructure/src/proc.rs` — Confinement mechanisms and platform-gated code paths
- `crates/app/tests/platform_gates.rs` — The regression guard (also documents the COX-B006/COX-B008 story in its module comment)
