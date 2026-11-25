# CXA-B254 / B195b — terminal-state contract closure checklist

Parts **a/b** landed via `feat/CXA-B287-contract-guards` (merged into this
branch at c898e4b0); part **c = CXA-B288** closes the tracking item.

## Checklist

- [x] **Part a — CXA-B286/1: golden wire fixtures + serde round-trips.**
      `crates/domain/tests/dto_serde_round_trip_b254.rs` plus golden fixture
      `crates/domain/tests/fixtures/artifact_version_cxa_api.json`.
- [x] **Part b — CXA-B287/2: contract guard tests.**
      `crates/domain/tests/contract_guard_b287.rs`: partial-payload loading,
      required-field whitelist enforcement, unique `snake_case` enum tags.
- [x] **Part c — CXA-B288/3: generated contract docs.**
      `crates/domain/docs/wiki/engineering/cxa-b254-contract-surface.md`,
      rendered from `crates/domain/tests/contract_surface/mod.rs`; update-check
      test `rendered_doc_matches_the_checked_in_copy` fails any drift;
      contributor instructions in `README.md` ("Domain contract docs").
- [x] Five spot-check goldens tie doc rows to real serialize output
      (`golden_*` tests in `contract_docs_b288.rs`).
- [x] Full domain suite green (`cargo test -p coxagent-domain`, 6+7 tests).

## Outstanding escalations / gaps carried forward

None outstanding on this chain: no `ASK SA:` or `ASK BA:` blocks were raised
during parts a/b; the only known gap is documented rather than blocking —
`ArtifactVersion` is the only DTO with a committed golden fixture so far
(other types round-trip but have no fixture), and no struct currently opts
into `deny_unknown_fields` (`STRICT_UNKNOWN_KEY_TYPES` is empty by design).
