# Frozen above-the-fold panel inventory (`__inventories__/`)

`above_the_fold_panels.golden` is the committed golden snapshot of the
Overview screen's above-the-fold panels (CXA-B281.a): one canonical JSON
record per line — `{component_name, order_index, panel_id, route,
visibility_rules_hash}` — keyed by dense ascending `order_index`.

## How it was generated

By `freeze_inventory` (`crates/application/src/use_cases/freeze_fold_inventory.rs`)
running against the typed panel manifest through
`ManifestFreezeSource` (`crates/presentation/src/fold_freeze_adapter.rs`).
The record shape lives in `coxagent_domain::PanelInventoryEntry`; hashes are
domain FNV-1a over the declared visibility-rule tokens.

## Regenerating (the only sanctioned way to change data lines)

```sh
cargo run -p coxagent-app --bin freeze_fold_inventory
```

Byte-stability is enforced by
`crates/app/tests/fold_inventory_freeze_b281.rs`, which regenerates from the
live manifest and diffs against this file — if that test fails, the manifest
changed; regenerate and commit the whole file rather than hand-editing.
Never edit data lines by hand; header comment lines may be updated alongside.

This directory ships the freeze mechanism ONLY — drift enforcement is a later
subtask (CXA-B281.b/c).
