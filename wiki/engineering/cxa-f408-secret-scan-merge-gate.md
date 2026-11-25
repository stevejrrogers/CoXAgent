FOLDER: Engineering
# Secret & Dependency Scan Merge Gate

**Keywords:** secret scan, credential leak, merge gate, CVE, dependency audit, ScannerPort, CXA-F406, CXA-F407, CXA-F408, lockfile, verification merge, fail closed

## Overview

Before an auto-merge lands a PR, the run's merged tree is scanned for leaked credentials and urgent dependency vulnerabilities; findings block the merge with a narrated reason. It exists because SA prose review was the only gate between a pushed key and `main`. Users are the agent operators (who see the blocking message in #agents) and any teammate whose PR gets held by it.

The epic split: **CXA-F406** = the pure secret-scanner core (`secret_scan.rs`), **CXA-F407** = the port + adapter + wiring into the merge flow, **CXA-F408** = this documented end-to-end merge gate.

## How it works

1. **Adapter builds the report.** `WorkspaceScanner` (infrastructure) implements the single method of `ScannerPort::scan(work_dir)` (`crates/application/src/ports/outbound/scan.rs`). It walks the tree/diff and returns a plain-data `ScanReport { findings: Vec<ScanFinding>, secret_findings: Vec<SecretFinding>, scanned_files }` — deps results from `deps_scan`, secrets from `use_cases::secret_scan`. All IO lives in the adapter; the decision logic is pure.
2. **Pure verdict.** `scanner_gate_verdict(knob, Option<&ScanReport>) -> GateVerdict` in `use_cases/merge_scan.rs` turns a report into `Pass` or `Blocked { reason }`: knob OFF → always Pass (zero behaviour change); clean report → Pass; deps finding where `deps_scan::is_urgent_cve(severity)` or a `secret_scan::SecretFinding` passes `blocking_reasons(&ScanReport)` → Blocked. A `None` report or port `Err` fails **closed** (gate precedent: same as deploy health).
3. **Merge integration.** In `use_cases/cycle/forge_merge.rs`, `verify_merged_result` checks `self.scanner.is_some() && self.config.git.scanner_gate`; when active it runs `scanner_gate_on_merged_tree(pr_number, head, target, &dir)` on the throwaway pre-merge worktree (`cox-premerge-*`) created for test verification. `GateVerdict::Blocked` becomes `Err("PR #{n} ({head} -> {target}): verification merge blocked by the credential-leak merge gate (CXA-F407): {reason}")`; a clean pass logs "credential-leak gate: PR #{n} … clean over N scanned files".
4. **Wiring.** `builders.rs` (~line 496, CXA-F407 comment) constructs `Arc<WorkspaceScanner>` when the git flow is enabled and calls `CycleOrchestrator::with_scanner(scanner)` (`use_cases/cycle/mod.rs`, line 668). Default is `None` → gate skipped entirely.

## Usage

Nothing to invoke manually — the gate fires inside every automatic merge:

```text
PR #42 (feat/fix-login -> main) passes tests on the merged tree
→ scanner_gate_on_merged_tree scans the worktree
→ report has one SecretFinding (AWS access key in src/main.rs:17)
→ verify_merged_result returns Err:
   "PR #42 (feat/fix-login -> main): verification merge blocked by the
    credential-leak merge gate (CXA-F407): AWS_ACCESS_KEY_ID src/main.rs:17"
→ the PR stays open and the failure is narrated instead of merging.
```

To add a new detection rule: extend `secret_scan.rs` (pure `scan_line(path, line_no, line) -> Vec<SecretFinding>`), then add a TDD case in `crates/application/tests/secret_scan_f406_tdd.rs` or `crates/app/tests/secret_scan_f406_gate.rs`.

## Interface

- `trait ScannerPort { async fn scan(&self, work_dir: &Path) -> Result<ScanReport, PortError>; }` — outbound port, one method, never destructive.
- `struct ScanReport { findings, secret_findings, scanned_files }` — `scanned_files > 0` distinguishes "clean" from "did not scan".
- `enum GateVerdict { Pass, Blocked { reason } }` in `use_cases/merge_scan.rs`.
- `fn scanner_gate_verdict(...) -> GateVerdict`, `fn blocking_reasons(&ScanReport) -> Vec<String>` — pure functions, unit-testable without IO.
- `fn scan_line(path, line_no, line) -> Vec<SecretFinding>` / `FIXTURE_PATH_ALLOWLIST` (`tests/fixtures/`, `examples/`, `*.md`) / `MAX_LINE_LEN = 64 KiB` — scanner core knobs and entry points.
- `CycleOrchestrator::with_scanner(Arc<dyn ScannerPort>)` — presentation/builder-side binding.
- `config.git.scanner_gate: bool` — the on/off knob read at merge time.

## Configuration

| Setting | Location | Default | Effect |
|---|---|---|---|
| `git.scanner_gate` | application config (Git section) | off in older configs; see `config.rs` docs near the F407 comment | master switch; `false` ⇒ `Pass` unconditionally |
| scanner binding | `builders.rs` | bound only when git flow enabled | no `with_scanner` ⇒ gate silently skipped |

## Edge cases and limits

- **Deliberately does NOT**: quarantine or rotate leaked keys, edit the offending file, or rescan after fixing — it blocks once and reports; a follow-up commit re-triggers the gate on the next merge attempt.
- **Fail closed**: a port error or missing report blocks rather than merges blind.
- **Fixture allowlist**: test fixtures/examples/markdown hits are suppressed so demo data doesn't wedge every merge.
- **64 KiB line cap** protects against pathological single-line blobs.
- If neither scanner nor git/deploy ports are configured the merge proceeds "unverified" with a tracing warning — silence is NOT security here, check the log.

## Code map

Paths below were verified present in the F408 working tree:

- `crates/application/src/ports/outbound/scan.rs` — `ScannerPort` trait + `ScanReport`; deliberately thin, IO-free.
- `crates/application/src/use_cases/secret_scan.rs` — pure secret scanner core: rules, `SecretFinding`, `scan_line`, severity, fixture allowlist, `MAX_LINE_LEN`.
- `crates/application/src/use_cases/merge_scan.rs` — `GateVerdict`, `scanner_gate_verdict`, `blocking_reasons`: the pure merge decision.
- `crates/application/src/deps_scan.rs` — dependency/CVE parsing: `is_urgent_cve`, `is_lockfile`, lockfile version maps, `ScanFinding::parse`.
- `crates/application/src/use_cases/cycle/forge_merge.rs` — `verify_merged_result` + `scanner_gate_on_merged_tree`: where the gate blocks the merge and formats the narration.
- `crates/application/src/use_cases/cycle/mod.rs` — `CycleOrchestrator::with_scanner` binding and the `scanner` field.
- `crates/application/src/config.rs` — `git.scanner_gate` knob and its design note ("BLOCKS the verification merge on urgent-CVE or secret-class findings").
- `crates/infrastructure/src/scanner.rs` — `WorkspaceScanner`: the real adapter doing tree walk/diff parsing behind the port.
- `crates/app/src/builders.rs` — construction + wiring of `WorkspaceScanner` into the orchestrator (CXA-F407 block).
- `crates/application/tests/secret_scan_f406_tdd.rs` — TDD suite for the pure scanner rules (CXA-F406 ACs).
- `crates/app/tests/secret_scan_f406_gate.rs` — end-to-end gate tests: rule positives/negatives, raw-secret non-exposure, placeholder suppression.

## Related

- `docs/CXA-F003.md` — orchestration context the merge gate lives in.
- `docs/CXA-F006.md` — ticket/spec conventions referenced by the F40x series.
- `ARCHITECTURE.md` — hexagonal rule this page follows (port out, pure decision in).
