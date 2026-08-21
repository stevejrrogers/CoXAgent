FOLDER: Engineering

# Use-Case Re-Export Wiring Guard

**Keywords:** re-export guard, facade re-export, crate-root facade, RunReleasesUseCase, run_releases wiring, use_cases/mod.rs manifest, half-wired module, INTERNAL_HELPERS allow-list

## Overview

CXA-F027 completes the wiring of `RunReleasesUseCase` through the application facade and adds a regression guard (`mod_guard_tests.rs`) that fails any future use case whose module is declared in `use_cases/mod.rs` but never surfaced by a matching `pub use`. It exists so a half-wired use case breaks loudly here in CI instead of silently far away at each call site when downstream code tries to construct it as `crate::use_cases::SomeUseCase`. It is for anyone adding or renaming a use case module under `crates/application/src/use_cases/`.

## How it works

The guard is one test module compiled only under `#[cfg(test)]`, registered from the canonical manifest:

1. [`mod.rs`](crates/application/src/use_cases/mod.rs) declares `` #[cfg(test)] mod mod_guard_tests; `` next to the other run_releases test modules.
2. Inside `mod_guard_tests.rs`, a const pulls in that same manifest text hermetically via `` include_str!("mod.rs") `` — no filesystem access at runtime and immune to CWD drift.
3. Two tiny parsers scan that text:
   - `declared_modules` collects every top-level single-token `` pub mod <name>; `` declaration.
   - `reexport_names` walks every statement rooted on a `` use `` keyword (including multiline brace groups) across terminating semicolons and collects all identifier tokens.
4. The single integration assertion `every_declared_module_is_facade_exposed_or_internal_helper` checks three things:
   - The scanner actually saw declarations (so an edit cannot silently disable it),
   - specifically that both `` run_releases `` and `` RunReleasesUseCase `` are present,
   - that every declared module appears among re-exports unless named in `INTERNAL_HELPERS`.
5. A set of parser-level regression tests over hand-built synthetic source strings pin down each piece independently of whatever happens to be wired today.

The allow-list exists because `ceremony`, `approval_memory` and `approval_risk` are deliberately internal helpers consumed by sibling use cases through their own public crate paths — they legitimately carry no facade re-export.

The concrete wiring this ticket guards already lives in place: [cycle/wiring.rs:44](crates/application/src/use_cases/cycle/wiring.rs#L44)'s `.releases()` builder constructs it with config + git ports attached (`RunReleasesUseCase::new(...).with_config(...).with_git(...)`), invoked from cycle/mod.rs:845 (`self.releases().execute()`).

## Usage

Add a new use case end-to-end so its call sites can build it from the facade path:

```rust
// 1. new module file    crates/application/src/use_cases/my_feature.rs
pub struct MyFeatureUseCase { /* ... */ }

// 2a. declare it        crates/application/src/use_cases/mod.rs
pub mod my_feature;

// 2b. re-export it      crates/application/src/use_cases/mod.rs
pub use my_feature::MyFeatureUseCase;
```

Forget step 2b (or rename without updating both sides) and running tests fails here rather than at some distant caller:

```
FAILED -- declared but not facade-re-exported (and not an internal helper): my_feature
```

To authorise an intentionally internal helper instead of exposing it publicly, add its name to `INTERNAL_HELPERS`:

```rust
const INTERNAL_HELPERS: &[&str] = &["ceremony", "approval_memory", "approval_risk", "my_helper"];
```

## Interface

Test-only surface defined inside `mod_guard_tests`, none exported publicly:

| Item | Signature / value |
|------|-------------------|
| const | MOD_SRC = include_str!("mod.rs") |
| const | INTERNAL_HELPERS: &[&str] = ["ceremony","approval_memory","approval_risk"] |
| fn | is_valid_name(name: &str) -> bool |
| fn | strip_kw(text: &str, kw: &str) -> Option<&str> |
| fn | byte_count(text: &str, needle: u8) -> usize |
| fn | declared_modules(src: &str) -> Vec<String> |
| fn | reexport_names(src: &str) -> Vec<String> (sorted unique tokens) |
| test | every_declared_module_is_facade_exposed_or_internal_helper |
| test | declares_every_top_level_pub_mod / skips_non_pub_module_lines_when_declaring / reexports_names_from_multiline_brace_groups / half_wired_module_is_reported_as_missing / allow_listed_internal_helpers_are_not_missing |

Protected contract on `mod.rs`: line 26 (`pub mod run_releases;`) and line 56 (`pub use run_releases::RunReleasesUseCase;`) must stay in sync.

## Configuration

No runtime configuration keys change this feature's behaviour; everything happens at compile/test time behind `#[cfg(test)]`. The only tunable is source-level:

- **Allow-list** (`INTERNAL_HELPERS`) — names internal helper modules exempt from requiring a facade re-export; defaults to ceremony / approval_memory / approval_risk.
- The shipping gate implied by release config (`releases.enabled`) belongs to RunReleasesUseCase itself and is documented under CXA-B006; this ticket adds none.

There is deliberately no env var or flag to disable the guard — weakening surface-integrity checks should require editing source with intent rather than flipping configuration.

## Edge cases and limits

It deliberately does NOT do these things:

- **Not a compiler guarantee.** It parses text heuristically rather than using rustdoc/HIR metadata; contrived formatting could defeat either parser (the parser-level tests pin realistic shapes but not exhaustive ones).
- **Only catches missing *facade* exports.** A fully private module kept out of downstream reach still passes if allow-listed — nothing forces internal helpers onto consumers.
- **Depends on declaration style.** Only single-token top-level declarations are scanned; attributes split across lines or macro-generated modules are not matched.
- **Silent outside test builds.** Because registration is behind `#[cfg(test)]`, production builds never compile this file — breaking changes surface through CI/tests alone.

## Code map

CXA-F027's intended deliverable (`mod_guard_tests.rs`) has NOT merged into this working tree's HEAD. Verified via `git merge-base --is-ancestor <sha> HEAD`, which returns failure for each of commits `9c64b4e` (CXA-F027), `94cf2c0` (CXA-B052) and `9137452` (CXA-B055) — none is an ancestor of HEAD today. So what follows separates the code that exists now from what arrives only once those commits land.

Present in the current working tree (verified against HEAD):

- crates/application/src/use_cases/mod.rs -- carries both sides of the protected contract today: line 26 (`pub mod run_releases;`) and line 56 (`pub use run_releases::RunReleasesUseCase;`) must stay in sync.
- crates/application/src/use_cases/cycle/wiring.rs -- `.releases()` builder at wiring.rs:44 attaches config + git ports to a fresh `RunReleasesUseCase`.
- crates/application/src/use_cases/cycle/mod.rs -- invokes it at cycle/mod.rs:845 (`self.releases().execute()`).
- crates/application/src/use_cases/run_releases.rs -- `RunReleasesUseCase` itself (the automated release pipeline).

Tests alongside (already on disk, present under use_cases/):

- crates/application/src/use_cases/run_releases_tdd_tests.rs
- crates/application/src/use_cases/run_releases_tests.rs

Docs:

- docs/CXA-B006-release-pipeline.md -- sibling documentation for that pipeline's full behaviour and configuration.

Not present in this working tree yet:

- No file named mod_guard_tests.rs exists on HEAD. When CXA-F027 merges, it arrives as a new test module at crates/application/src/use_cases/mod_guard_tests.rs, and `mod.rs` additionally gains `` #[cfg(test)] mod mod_guard_tests; `` alongside `run_releases_tdd_tests` / `run_releases_tests` (after line 60 today).

## Related

- docs/CXA-B006-release-pipeline.md -- the release pipeline feature this ticket finished wiring.
- RunMilestonesUseCase -- producer of the milestones RunReleasesUseCase consumes; lives at crates/application/src/use_cases/run_milestones.rs:27 (`pub struct RunMilestonesUseCase`).
- Commit trail: `9c64b4e` (CXA-F027) -> `94cf2c0` (CXA-B052: deliverable never merged into build/main) -> `9137452` (CXA-B055: still not merged). None is an ancestor of HEAD today (`git merge-base --is-ancestor <sha> HEAD` returns failure for all three), so whether mod_guard_tests.rs is present depends on the base being built.
