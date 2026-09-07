//! The SQL adapter's shard-row codec (CXA-C019b): decoding one
//! [`ShardKind`]'s payload from its `project_state_shard` row.
//!
//! A shard row's `data` is the externally-tagged `ShardData` JSON (a
//! single-key object naming the kind — the same shape `ShardedState`
//! serializes), so the row is self-describing and one decode path serves
//! every kind.
//!
//! Everything here is a pure function over the application crate's shard
//! types: no IO, no connection, no clock (same discipline as
//! `state::shards`).

use coxagent_application::state::{ShardData, ShardKind, StateShard, SCHEMA_VERSION};
use coxagent_application::PortError;

/// Decode one shard row's payload. The payload must carry the kind its row
/// labels — a row whose payload disagrees with its label is corrupt, and
/// refusing beats silently trusting either (the same rule the composed load
/// applies). Refuses a payload from a FUTURE schema exactly like the whole
/// document read refuses a newer aggregate (`decode_checked`'s
/// `schema_version` check) — a shard read must never silently serve data the
/// full read would fail closed on. Only the Work shard carries the
/// aggregate's `schema_version`, so that is where the guard can live.
///
/// # Errors
/// [`PortError::Corrupt`] when the payload does not decode, disagrees with
/// `kind`, or is newer than the supported schema.
pub(crate) fn decode_shard(
    kind: ShardKind,
    payload: serde_json::Value,
) -> Result<StateShard, PortError> {
    let data: ShardData = serde_json::from_value(payload)
        .map_err(|e| PortError::Corrupt(format!("{kind:?} shard payload not decodable: {e}")))?;
    if data.kind() != kind {
        return Err(PortError::Corrupt(format!(
            "shard row labelled '{kind:?}' carries a '{:?}' payload",
            data.kind()
        )));
    }
    if let ShardData::Work(work) = &data {
        if work.schema_version > SCHEMA_VERSION {
            return Err(PortError::Corrupt(format!(
                "work shard schema_version {} newer than supported {SCHEMA_VERSION}",
                work.schema_version
            )));
        }
    }
    Ok(StateShard::new(data))
}

#[cfg(test)]
mod sql_shards_tests {
    use super::*;
    use coxagent_application::state::{ProjectState, SocialShard, WorkShard};

    /// The DEFAULT payloads survive their row codec losslessly (populated
    /// payloads are covered end to end by the Postgres contract suite), and
    /// a future-schema Work payload is refused like a newer envelope would
    /// be.
    #[test]
    fn decode_round_trips_the_default_shard_payloads_and_refuses_a_future_schema() {
        let mut sharded = ProjectState::default().into_shards();
        for kind in ShardKind::ALL {
            let StateShard { data, .. } = sharded.take(kind);
            let payload = serde_json::to_value(&data).expect("payload serializes");
            assert_eq!(
                decode_shard(kind, payload).expect("decodes").data,
                data,
                "{kind:?} payload must survive the row codec losslessly"
            );
        }

        let future = WorkShard {
            schema_version: SCHEMA_VERSION + 1,
            ..WorkShard::default()
        };
        let err = decode_shard(
            ShardKind::Work,
            serde_json::to_value(ShardData::Work(future)).expect("work serializes"),
        )
        .expect_err("future schema must be refused");
        assert!(
            err.to_string().contains("newer than supported"),
            "same fail-closed envelope as the full read: {err}"
        );
    }

    /// A row whose payload disagrees with its label is corrupt — the decode
    /// refuses rather than trusting either side.
    #[test]
    fn decode_refuses_a_payload_that_disagrees_with_its_row_label() {
        let payload =
            serde_json::to_value(ShardData::Social(SocialShard::default())).expect("serializes");
        let err = decode_shard(ShardKind::Work, payload).expect_err("label mismatch refused");
        assert!(
            err.to_string().contains("carries a"),
            "the refusal must name the disagreement: {err}"
        );
    }
}
