# Domain contract surface (CXA-B254)

> **GENERATED FILE — do not hand-edit.** Regenerate after any DTO change with:
>
> ```sh
> cargo test -p coxagent-domain --test contract_docs_b288 write_contract_doc_file
> ```
>
> Then commit the diff. An outdated copy fails `cargo test -p coxagent-domain` (the `*_is_up_to_date` guard below).

Rendered from `crates/domain/tests/contract_surface/mod.rs` — the single source of truth shared verbatim with the CXA-B287 contract guards, so this page can never claim what the guards no longer prove.

## Enum wire forms

Every serialized enum variant uses lowercase `snake_case` tags; tags are unique within each enum. Wire-accurate as of the last regeneration.

### `TicketType`

| Rust variant | wire tag |
|---|---|
| `Feature` | `feature` |
| `Bug` | `bug` |
| `Chore` | `chore` |

### `Status`

| Rust variant | wire tag |
|---|---|
| `Pending` | `pending` |
| `InProgress` | `in_progress` |
| `OnHold` | `on_hold` |
| `Documented` | `documented` |
| `Verified` | `verified` |

### `Role`

| Rust variant | wire tag |
|---|---|
| `Ba` | `ba` |
| `Po` | `po` |
| `Sa` | `sa` |
| `Sm` | `sm` |
| `DevBug` | `dev_bug` |
| `DevFeature` | `dev_feature` |
| `Reviewer` | `reviewer` |
| `System` | `system` |

### `Priority`

| Rust variant | wire tag |
|---|---|
| `Low` | `low` |
| `Medium` | `medium` |
| `High` | `high` |

### `Complexity`

| Rust variant | wire tag |
|---|---|
| `Small` | `small` |
| `Medium` | `medium` |
| `Large` | `large` |

### `TestCaseStatus`

| Rust variant | wire tag |
|---|---|
| `Pending` | `pending` |
| `Passed` | `passed` |
| `Failed` | `failed` |

### `RecordKind`

| Rust variant | wire tag |
|---|---|
| `Verdict` | `verdict` |
| `EvidenceRefresh` | `evidence_refresh` |

### `CoverageStatus`

| Rust variant | wire tag |
|---|---|
| `Covered` | `covered` |
| `PartiallyCovered` | `partially_covered` |
| `NotTested` | `not_tested` |

### `DebtSignalKind`

| Rust variant | wire tag |
|---|---|
| `LintRegression` | `lint_regression` |
| `DeadCodeSuspects` | `dead_code_suspects` |
| `MissingModuleDoc` | `missing_module_doc` |

### `GoalStatus`

| Rust variant | wire tag |
|---|---|
| `Active` | `active` |
| `Retired` | `retired` |

## Struct field policies (required vs defaulted; unknown keys)

House decoding policy: unknown JSON keys are **tolerated and dropped**, payload fields not listed below are `#[serde(default)]`. `REQUIRED` fields must be present or loading fails.

| Type | Required fields | Unknown keys |
|---|---|---|
| `TestCase` | description | tolerated, dropped |
| `VerdictRecord` | status | tolerated, dropped |
| `CoverageEntry` | criterion, status | tolerated, dropped |
| `WipCheckpoint` | ref_name, sha, note, recorded_at | tolerated, dropped |
| `TechnicalDesign` | approach, files, api_contract, data_changes, test_plan | tolerated, dropped |
| `UxDesign` | user_flow, screens, component_states, responsive_notes | tolerated, dropped |
| `ArtifactVersion` | project, artifact, version | tolerated, dropped |

Types not registered here decode tolerantly with all fields defaulted.

## Golden wire fixtures

- `ArtifactVersion` — [`crates/domain/fixtures/artifact_version_cxa_api.json`](../../../fixtures/artifact_version_cxa_api.json) round-trips byte-stably in `dto_serde_round_trip_b254.rs`
- Envelopes: error `{ "error": <string> }`; version-tagged success `{ "status": "ok", "version": MAJOR.MINOR.PATCH }`

