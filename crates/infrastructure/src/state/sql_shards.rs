//! The SQL adapter's shard columns (CXA-C019b): the native per-bounded-context
//! home of each [`ShardKind`] payload beside the legacy `data` envelope.
//!
//! ONE column per shard (`shard_work`, `shard_social`, …) on `project_state`,
//! storing that shard's payload exactly as `ShardedState` serializes it —
//! the kind label lives in the column name, the payload in the column value.
//! The columns are nullable: rows written by a pre-shard writer leave them
//! NULL, and a shard read of a NULL column falls back to projecting the
//! envelope — which is also the rollback path, so the shard columns can never
//! be the only copy of a field.
//!
//! Everything here is a pure function over the application crate's shard
//! types: no IO, no connection, no clock (same discipline as
//! `state::shards`). The exhaustive matches are the anti-drift guard — a new
//! [`ShardKind`] without a column mapping, a default payload and a codec arm
//! fails compilation.

use coxagent_application::state::{
    DocsShard, GovernanceShard, OpsShard, ProjectState, ShardData, ShardKind, SocialShard,
    StateShard, WorkShard, SCHEMA_VERSION,
};
use coxagent_application::PortError;

/// The `project_state` column that natively stores `kind`'s payload.
#[must_use]
pub(crate) fn shard_column(kind: ShardKind) -> &'static str {
    match kind {
        ShardKind::Work => "shard_work",
        ShardKind::Social => "shard_social",
        ShardKind::Docs => "shard_docs",
        ShardKind::Governance => "shard_governance",
        ShardKind::Ops => "shard_ops",
    }
}

/// The payload a shard read serves for a NEVER-WRITTEN project — a fresh
/// slice of the aggregate, identical to what `load()` yields on an empty
/// store. The Work shard's default carries [`SCHEMA_VERSION`], not 0 (see
/// `WorkShard::default`), so a shard-native writer starting from it produces
/// a document the stores accept.
#[must_use]
pub(crate) fn default_shard(kind: ShardKind) -> StateShard {
    match kind {
        ShardKind::Work => StateShard::new(ShardData::Work(WorkShard::default())),
        ShardKind::Social => StateShard::new(ShardData::Social(SocialShard::default())),
        ShardKind::Docs => StateShard::new(ShardData::Docs(DocsShard::default())),
        ShardKind::Governance => StateShard::new(ShardData::Governance(GovernanceShard::default())),
        ShardKind::Ops => StateShard::new(ShardData::Ops(OpsShard::default())),
    }
}

/// Decode one shard column's payload. Refuses a payload from a FUTURE schema
/// exactly like the envelope path refuses a newer document (`load_versioned`'s
/// `schema_version` check) — a shard read must never silently serve data the
/// full read would fail closed on. Only the Work shard carries the aggregate's
/// `schema_version`, so that is where the guard can live.
///
/// # Errors
/// [`PortError::Corrupt`] when the payload does not decode or is newer than
/// the supported schema.
pub(crate) fn decode_shard(
    kind: ShardKind,
    payload: serde_json::Value,
) -> Result<StateShard, PortError> {
    let data = match kind {
        ShardKind::Work => serde_json::from_value(payload).map(ShardData::Work),
        ShardKind::Social => serde_json::from_value(payload).map(ShardData::Social),
        ShardKind::Docs => serde_json::from_value(payload).map(ShardData::Docs),
        ShardKind::Governance => serde_json::from_value(payload).map(ShardData::Governance),
        ShardKind::Ops => serde_json::from_value(payload).map(ShardData::Ops),
    }
    .map_err(|e| PortError::Corrupt(format!("{kind:?} shard payload not decodable: {e}")))?;
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

/// Serialize the aggregate's shard payloads as ONE document keyed by
/// `ShardedState`'s field names (`work`, `social`, …) — the shape the
/// guarded save and the transactional claim both use to refresh every shard
/// column in a single statement, so the columns and the `data` envelope can
/// never disagree about a committed write. Borrows the aggregate: both
/// callers need the state alive afterwards for the local mirror.
///
/// # Errors
/// [`PortError::Backend`] when serialization fails.
pub(crate) fn shard_doc(state: &ProjectState) -> Result<serde_json::Value, PortError> {
    serde_json::to_value(state.clone().into_shards())
        .map_err(|e| PortError::Backend(format!("encode shard columns: {e}")))
}

#[cfg(test)]
mod sql_shards_tests {
    use super::*;

    /// `ShardedState`'s serde field names are the JSON keys the guarded save
    /// and the claim both extract (`$doc->'work'`), and the columns are named
    /// `shard_` + those keys. This pins the whole vocabulary to the one
    /// source of truth: `ShardKind`'s own serde name.
    #[test]
    fn the_column_vocabulary_is_derived_from_the_shard_kinds_serde_names() {
        for kind in ShardKind::ALL {
            let serde_name = serde_json::to_value(kind)
                .expect("ShardKind serializes")
                .as_str()
                .expect("snake_case string")
                .to_owned();
            assert_eq!(
                shard_column(kind),
                format!("shard_{serde_name}"),
                "column for {kind:?} must be shard_{serde_name}"
            );
        }
        // Uniqueness: five kinds, five columns.
        let columns: Vec<&str> = ShardKind::ALL.iter().map(|k| shard_column(*k)).collect();
        let mut sorted = columns.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), columns.len(), "columns must be distinct");
    }

    /// The never-written slice must equal what a full default load projects —
    /// a shard-native writer starting cold behaves exactly like today's
    /// envelope reader.
    #[test]
    fn default_shard_matches_the_default_aggregates_projection() {
        let fresh = ProjectState::default();
        for kind in ShardKind::ALL {
            assert_eq!(
                default_shard(kind),
                fresh.shard(kind),
                "default_shard({kind:?}) must equal the default aggregate's slice"
            );
        }
    }

    /// The DEFAULT payloads survive their column codec losslessly (populated
    /// payloads are covered end to end by `shard_doc_keys_…` and the Postgres
    /// contract suite), and a future-schema Work payload is refused like a
    /// newer envelope would be.
    #[test]
    fn decode_round_trips_the_default_shard_payloads_and_refuses_a_future_schema() {
        let mut sharded = ProjectState::default().into_shards();
        for kind in ShardKind::ALL {
            let expected = sharded.take(kind);
            let payload = serde_json::to_value(&expected).expect("payload serializes");
            assert_eq!(
                decode_shard(kind, payload).expect("decodes"),
                expected,
                "{kind:?} payload must survive the column codec losslessly"
            );
        }

        let future = WorkShard {
            schema_version: SCHEMA_VERSION + 1,
            ..WorkShard::default()
        };
        let err = decode_shard(
            ShardKind::Work,
            serde_json::to_value(&future).expect("work serializes"),
        )
        .expect_err("future schema must be refused");
        assert!(
            err.to_string().contains("newer than supported"),
            "same fail-closed envelope as the full read: {err}"
        );
    }

    /// The dual-write document keys each payload under the kind's serde name —
    /// the exact shape the SQL extracts with `->` — and each extracted payload
    /// decodes back to the aggregate's own slice for that kind.
    #[test]
    fn shard_doc_keys_each_payload_by_its_kinds_serde_name() {
        let state = ProjectState {
            lessons: vec!["shard the state".to_owned()],
            ..ProjectState::default()
        };
        let doc = shard_doc(&state).expect("serializes");
        let object = doc.as_object().expect("shard doc object");
        assert_eq!(object.len(), ShardKind::ALL.len(), "one key per shard");
        for kind in ShardKind::ALL {
            let serde_name = serde_json::to_value(kind)
                .expect("ShardKind serializes")
                .as_str()
                .expect("snake_case string")
                .to_owned();
            let payload = object
                .get(&serde_name)
                .unwrap_or_else(|| panic!("shard doc must key {kind:?} as {serde_name:?}"));
            assert_eq!(
                decode_shard(kind, payload.clone()).expect("decodes"),
                state.shard(kind),
                "the doc's {serde_name} payload must decode to the aggregate's slice"
            );
        }
    }
}
