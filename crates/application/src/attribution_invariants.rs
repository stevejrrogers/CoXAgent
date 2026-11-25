//! Attribution invariants (CXA-B199.3 / CXA-B273) — PURE domain checks over
//! the attribution data attached to recorded state changes: who (actor),
//! when (RFC3339 timestamp) and why (reason/kind).
//!
//! Like [`crate::use_cases::run_dev::gates`] this is a pure function over
//! data an adapter returns — zero IO, zero framework imports, no clock: the
//! caller supplies `now` so the check stays deterministic and testable. The
//! gate wiring that will consume this lives in a later slice; this module
//! only owns the invariants.
//!
//! Rules enforced (each surfaced as a structured [`AttributionViolation`]
//! naming the rule, the field, and found-vs-expected — never a bare bool):
//!
//! 1. `carries-attribution` — every recorded state change names an actor and
//!    stamps a timestamp.
//! 2. `reason-non-empty` — where the record type has a reason-like field the
//!    type makes mandatory, it is non-empty ([`EngineIncident::reason`]; the
//!    workspace run switch deliberately permits an empty reason on a plain
//!    drain-pause, so its `reason` stays unchecked here — see the note in
//!    [`check_workspace_run_flip`]).
//! 3. `timestamp-parseable` / `timestamp-order` / `timestamp-not-future` —
//!    timestamps parse as RFC3339, are internally ordered (detection never
//!    precedes the reverted commit it detects; a decision never precedes the
//!    event it decides), and never sit in the future relative to the
//!    reference instant the caller passes.

use std::fmt;

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::state::{
    ActivityEntry, EngineIncident, InterventionRecord, ProjectState, RevertDecision, RevertEvent,
    WorkspaceRun,
};

/// Rule id: a recorded change is missing its actor or its timestamp.
pub const RULE_CARRIES_ATTRIBUTION: &str = "carries-attribution";
/// Rule id: a mandatory reason-like field is blank.
pub const RULE_REASON_NON_EMPTY: &str = "reason-non-empty";
/// Rule id: a timestamp field does not parse as RFC3339.
pub const RULE_TIMESTAMP_PARSEABLE: &str = "timestamp-parseable";
/// Rule id: two timestamps inside one record are misordered.
pub const RULE_TIMESTAMP_ORDER: &str = "timestamp-order";
/// Rule id: a timestamp sits in the future relative to the supplied `now`.
pub const RULE_TIMESTAMP_NOT_FUTURE: &str = "timestamp-not-future";

/// One structured, human-readable attribution failure: which rule fired, on
/// which field of which record, what was found vs what was expected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributionViolation {
    /// Stable rule id (one of the `RULE_*` constants above).
    pub rule: &'static str,
    /// Where the offending record lives, e.g. `governance_interventions[3]`.
    pub location: String,
    /// The offending field within the record, e.g. `by`.
    pub field: &'static str,
    /// What the field actually contained (quoted for empty strings).
    pub found: String,
    /// What the invariant requires.
    pub expected: String,
}

impl fmt::Display for AttributionViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {}.{} — expected {}, found {}",
            self.rule, self.location, self.field, self.expected, self.found
        )
    }
}

impl AttributionViolation {
    fn new(
        rule: &'static str,
        location: impl Into<String>,
        field: &'static str,
        found: impl Into<String>,
        expected: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            location: location.into(),
            field,
            found: found.into(),
            expected: expected.into(),
        }
    }

    fn missing(rule: &'static str, location: impl Into<String>, field: &'static str) -> Self {
        let expected = match rule {
            RULE_TIMESTAMP_PARSEABLE => "an RFC3339 timestamp".to_owned(),
            _ => "a non-empty value".to_owned(),
        };
        Self::new(rule, location, field, "<empty>", expected)
    }
}

/// True when `value` carries visible content (blank/whitespace counts as absent).
fn present(value: &str) -> bool {
    !value.trim().is_empty()
}

/// Parse an RFC3339 stamp, or return a `timestamp-parseable` violation.
fn parse_stamp(
    location: &str,
    field: &'static str,
    raw: &str,
) -> Result<OffsetDateTime, AttributionViolation> {
    OffsetDateTime::parse(raw, &Rfc3339).map_err(|_| {
        AttributionViolation::new(
            RULE_TIMESTAMP_PARSEABLE,
            location,
            field,
            format!("\"{raw}\""),
            "an RFC3339 timestamp",
        )
    })
}

/// One activity-feed row (`activity[i]`): actor, action (the why) and when.
#[must_use]
pub fn check_activity_entry(entry: &ActivityEntry, location: &str) -> Vec<AttributionViolation> {
    let mut v = Vec::new();
    if !present(&entry.agent) {
        v.push(AttributionViolation::missing(
            RULE_CARRIES_ATTRIBUTION,
            location,
            "agent",
        ));
    }
    if !present(&entry.action) {
        v.push(AttributionViolation::missing(
            RULE_REASON_NON_EMPTY,
            location,
            "action",
        ));
    }
    if !present(&entry.at) {
        v.push(AttributionViolation::missing(
            RULE_CARRIES_ATTRIBUTION,
            location,
            "at",
        ));
    }
    v
}

/// One operator decision (`governance_interventions[i]`): the ledger freezes
/// `by` and `at` at action time, so both must always be present. An empty
/// `ticket`/`area` is legitimate (the unattributed bucket) and never flagged.
#[must_use]
pub fn check_intervention(
    record: &InterventionRecord,
    location: &str,
) -> Vec<AttributionViolation> {
    let mut v = Vec::new();
    if !present(&record.by) {
        v.push(AttributionViolation::missing(
            RULE_CARRIES_ATTRIBUTION,
            location,
            "by",
        ));
    }
    if !present(&record.at) {
        v.push(AttributionViolation::missing(
            RULE_CARRIES_ATTRIBUTION,
            location,
            "at",
        ));
    }
    v
}

/// The workspace master run switch after an admin flip. A switch nobody ever
/// touched (`Default` — empty `by`/`at`/`reason`) has NO state change behind
/// it, so there is nothing to attribute and the check accepts it. Once a flip
/// IS recorded, actor and timestamp become mandatory; the reason stays
/// optional because the run-control endpoint deliberately accepts a bare
/// drain-pause without prose (`work.rs` fills it with `unwrap_or_default`).
#[must_use]
pub fn check_workspace_run_flip(run: &WorkspaceRun) -> Vec<AttributionViolation> {
    let untouched = !present(&run.by) && !present(&run.at);
    if untouched {
        return Vec::new();
    }
    let location = "workspace_run";
    let mut v = Vec::new();
    if !present(&run.by) {
        v.push(AttributionViolation::missing(
            RULE_CARRIES_ATTRIBUTION,
            location,
            "by",
        ));
    }
    if !present(&run.at) {
        v.push(AttributionViolation::missing(
            RULE_CARRIES_ATTRIBUTION,
            location,
            "at",
        ));
    }
    v
}

/// One live engine incident (`engine_incidents[i]`): the incident IS the
/// explanation of a broken state, so `reason`, `role` and `since` are all
/// mandatory.
#[must_use]
pub fn check_engine_incident(
    incident: &EngineIncident,
    location: &str,
) -> Vec<AttributionViolation> {
    let mut v = Vec::new();
    if !present(&incident.reason) {
        v.push(AttributionViolation::missing(
            RULE_REASON_NON_EMPTY,
            location,
            "reason",
        ));
    }
    if !present(&incident.role) {
        v.push(AttributionViolation::missing(
            RULE_CARRIES_ATTRIBUTION,
            location,
            "role",
        ));
    }
    if !present(&incident.since) {
        v.push(AttributionViolation::missing(
            RULE_CARRIES_ATTRIBUTION,
            location,
            "since",
        ));
    }
    v
}

/// One detected revert (`reverted_work[i]`): detection may never precede the
/// reverted commit, and a decided verdict (Approved/Dismissed) must name WHO
/// decided and WHEN — with the decision never preceding the commit either.
#[must_use]
pub fn check_revert_event(event: &RevertEvent, location: &str) -> Vec<AttributionViolation> {
    let mut v = Vec::new();

    if !present(&event.role) {
        v.push(AttributionViolation::missing(
            RULE_CARRIES_ATTRIBUTION,
            location,
            "role",
        ));
    }

    let reverted = match parse_stamp(location, "reverted_at", &event.reverted_at) {
        Ok(ts) => Some(ts),
        Err(e) => {
            v.push(e);
            None
        }
    };
    let detected = match parse_stamp(location, "detected_at", &event.detected_at) {
        Ok(ts) => Some(ts),
        Err(e) => {
            v.push(e);
            None
        }
    };

    if let (Some(reverted), Some(detected)) = (reverted, detected) {
        if detected < reverted {
            v.push(AttributionViolation::new(
                RULE_TIMESTAMP_ORDER,
                location,
                "detected_at",
                format!("detected {detected} before reverted {reverted}"),
                "detection at or after the revert commit it detected",
            ));
        }
    }

    if event.decision != RevertDecision::Pending {
        if !present(event.decided_by.as_deref().unwrap_or("")) {
            v.push(AttributionViolation::missing(
                RULE_CARRIES_ATTRIBUTION,
                location,
                "decided_by",
            ));
        }
        match event
            .decided_at
            .as_deref()
            .filter(|s| present(s))
            .map(|s| parse_stamp(location, "decided_at", s))
        {
            None => v.push(AttributionViolation::missing(
                RULE_CARRIES_ATTRIBUTION,
                location,
                "decided_at",
            )),
            Some(Err(e)) => v.push(e),
            Some(Ok(decided)) => {
                if let Some(reverted) = reverted {
                    if decided < reverted {
                        v.push(AttributionViolation::new(
                            RULE_TIMESTAMP_ORDER,
                            location,
                            "decided_at",
                            format!("decided {decided} before reverted {reverted}"),
                            "decision at or after the revert commit it decides",
                        ));
                    }
                }
            }
        }
    }

    v
}

/// Audit every attribution-bearing record in the snapshot. `now_rfc3339` is
/// the reference instant for the not-in-the-future rule — supplied by the
/// caller so this stays a pure function (the clock is IO and belongs to the
/// adapter that will eventually wire this into the guardrail gate).
///
/// Returns ALL violations found (plural), each self-describing; an empty vec
/// means the snapshot honours every invariant.
#[must_use]
pub fn audit_project_attribution(
    state: &ProjectState,
    now_rfc3339: &str,
) -> Vec<AttributionViolation> {
    let now = parse_stamp("audit", "__reference_now__", now_rfc3339);

    let mut out = Vec::new();
    for (i, entry) in state.activity.iter().enumerate() {
        out.extend(check_activity_entry(entry, &format!("activity[{i}]")));
        stamp_not_in_future(&entry.at, "at", &format!("activity[{i}]"), &now, &mut out);
    }
    for (i, record) in state.governance_interventions.iter().enumerate() {
        let loc = format!("governance_interventions[{i}]");
        out.extend(check_intervention(record, &loc));
        stamp_not_in_future(&record.at, "at", &loc, &now, &mut out);
    }
    for (i, incident) in state.engine_incidents.iter().enumerate() {
        let loc = format!("engine_incidents[{i}]");
        out.extend(check_engine_incident(incident, &loc));
        stamp_not_in_future(&incident.since, "since", &loc, &now, &mut out);
    }
    for (i, event) in state.reverted_work.iter().enumerate() {
        let loc = format!("reverted_work[{i}]");
        out.extend(check_revert_event(event, &loc));
        stamp_not_in_future(&event.reverted_at, "reverted_at", &loc, &now, &mut out);
        stamp_not_in_future(&event.detected_at, "detected_at", &loc, &now, &mut out);
        if let Some(decided_at) = event.decided_at.as_deref().filter(|s| present(s)) {
            stamp_not_in_future(decided_at, "decided_at", &loc, &now, &mut out);
        }
    }
    out.extend(check_workspace_run_flip(&state.workspace_run));
    if present(&state.workspace_run.at) {
        stamp_not_in_future(
            &state.workspace_run.at,
            "at",
            "workspace_run",
            &now,
            &mut out,
        );
    }
    out
}

/// Helper reading one stamp off any borrowed string carrier, appending a
/// `timestamp-not-future` violation when it parses and lies past `now`.
fn stamp_not_in_future<T: AsRef<str>>(
    raw: T,
    field: &'static str,
    location: &str,
    now: &Result<OffsetDateTime, AttributionViolation>,
    out: &mut Vec<AttributionViolation>,
) {
    let Ok(now) = now.as_ref() else { return };
    let Ok(ts) = OffsetDateTime::parse(raw.as_ref(), &Rfc3339) else {
        return; // unparseable stamps are already reported by their own check
    };
    if ts > *now {
        out.push(AttributionViolation::new(
            RULE_TIMESTAMP_NOT_FUTURE,
            location,
            field,
            format!("{ts} is after the reference now {now}"),
            "a timestamp at or before the reference now",
        ));
    }
}
