//! Drift alerts (CXA-F226) — the operator-facing surface of architecture-
//! conformance violations. Each scan of [`crate::conformance::check`] output
//! reconciles this list: an alert opens when a violation appears, survives
//! re-scans keyed by (area, message), links to the bug filed for it, and is
//! cleared automatically once a scan finds the area conformant again. The
//! dashboard renders the list verbatim off the serialized state.

use super::ProjectState;
use crate::conformance::Violation;
use serde::{Deserialize, Serialize};

/// One open drift alert: a conformance violation an operator can still see.
/// The serialized field names (`area` / `message` / `ticket`) are the
/// dashboard's data contract — rendered per entry on the overview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftAlert {
    /// The violating area (e.g. `server`) — half of the dedupe key and the
    /// visible heading's subject ("Architecture drift in server").
    pub area: String,
    /// The violation message, verbatim from the check.
    pub message: String,
    /// Id of the bug ticket filed for this violation — the entry's link.
    /// Empty only if the bug could not be resolved (never in practice).
    #[serde(default)]
    pub ticket: String,
    /// RFC3339 moment the alert was first raised.
    #[serde(default)]
    pub at: String,
}

impl ProjectState {
    /// Reconcile the open drift alerts against one scan's violations. PURE:
    /// the decision is a function of the current list, the violations, and
    /// the bug-id resolver — the caller does the IO (load, file, save).
    ///
    /// Dedupe key is the (area, message) pair: a re-scan that finds the same
    /// violation keeps the existing alert untouched (no duplicate, no re-timestamp);
    /// a violation absent from the list opens a new alert linked through
    /// `bug_for`; an open alert whose (area, message) no longer violates is
    /// dropped — resolution clears it without a manual dismissal.
    ///
    /// Returns whether the surface changed, so the caller saves only then.
    pub fn sync_drift_alerts(
        &mut self,
        violations: &[Violation],
        bug_for: &dyn Fn(&Violation) -> Option<String>,
    ) -> bool {
        let mut next: Vec<DriftAlert> = Vec::with_capacity(violations.len());
        for v in violations {
            if let Some(open) = self
                .drift_alerts
                .iter()
                .find(|a| a.area == v.area && a.message == v.message)
            {
                next.push(open.clone());
            } else {
                next.push(DriftAlert {
                    area: v.area.clone(),
                    message: v.message.clone(),
                    ticket: bug_for(v).unwrap_or_default(),
                    at: super::now_rfc3339(),
                });
            }
        }
        let changed = next != self.drift_alerts;
        self.drift_alerts = next;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conformance::Violation;

    fn v(area: &str, message: &str) -> Violation {
        Violation {
            area: area.to_owned(),
            message: message.to_owned(),
        }
    }

    #[test]
    fn new_violation_opens_an_alert_linked_to_its_bug() {
        let mut s = ProjectState::default();
        let changed = s.sync_drift_alerts(
            &[v(
                "server",
                "Rust expected but no marker file (Cargo.toml) found in `server`",
            )],
            &|_| Some("CXC-B001".to_owned()),
        );
        assert!(changed);
        assert_eq!(s.drift_alerts.len(), 1);
        let a = &s.drift_alerts[0];
        assert_eq!(a.area, "server");
        assert_eq!(
            a.message,
            "Rust expected but no marker file (Cargo.toml) found in `server`"
        );
        assert_eq!(a.ticket, "CXC-B001");
        assert!(!a.at.is_empty(), "first-raised time is recorded");
    }

    #[test]
    fn rescan_of_the_same_violation_keeps_the_existing_alert_untouched() {
        let mut s = ProjectState::default();
        s.sync_drift_alerts(&[v("server", "contains forbidden .ts files")], &|_| {
            Some("B001".to_owned())
        });
        let before = s.drift_alerts.clone();
        let changed = s.sync_drift_alerts(&[v("server", "contains forbidden .ts files")], &|_| {
            Some("B999".to_owned())
        });
        assert!(!changed, "an identical violation is not a new alert");
        assert_eq!(
            s.drift_alerts, before,
            "kept as-is — not re-linked, not re-timestamped"
        );
    }

    #[test]
    fn same_area_different_message_is_a_second_alert() {
        let mut s = ProjectState::default();
        s.sync_drift_alerts(
            &[v(
                "server",
                "Rust expected but no marker file (Cargo.toml) found in `server`",
            )],
            &|_| Some("B001".to_owned()),
        );
        s.sync_drift_alerts(
            &[
                v(
                    "server",
                    "Rust expected but no marker file (Cargo.toml) found in `server`",
                ),
                v(
                    "server",
                    "`server` must be Rust but contains forbidden .ts files",
                ),
            ],
            &|_| Some("B001".to_owned()),
        );
        assert_eq!(
            s.drift_alerts.len(),
            2,
            "the key is (area, message), not area alone"
        );
    }

    #[test]
    fn a_resolved_violation_clears_its_alert_while_open_areas_remain() {
        let mut s = ProjectState::default();
        s.sync_drift_alerts(
            &[
                v("server", "contains forbidden .ts files"),
                v("web", "contains forbidden .rs files"),
            ],
            &|_| Some("B001".to_owned()),
        );
        let changed = s.sync_drift_alerts(&[v("web", "contains forbidden .rs files")], &|_| {
            Some("B001".to_owned())
        });
        assert!(changed);
        assert_eq!(s.drift_alerts.len(), 1);
        assert_eq!(s.drift_alerts[0].area, "web");
    }

    #[test]
    fn an_all_clean_scan_clears_every_alert() {
        let mut s = ProjectState::default();
        s.sync_drift_alerts(&[v("server", "contains forbidden .ts files")], &|_| None);
        let changed = s.sync_drift_alerts(&[], &|_| None);
        assert!(changed);
        assert!(s.drift_alerts.is_empty());
        let serialized = serde_json::to_value(&s).expect("serialize");
        assert!(
            serialized
                .get("drift_alerts")
                .and_then(serde_json::Value::as_array)
                .is_some(),
            "the surface is serialized even at zero — the dashboard reads 0, not absence"
        );
    }

    #[test]
    fn unpersisted_state_loads_with_an_empty_surface() {
        // serde-default round trip: state written before the field existed.
        let legacy = r#"{"schema_version":1,"current_version":"0.0.0","tickets":[]}"#;
        let s: ProjectState = serde_json::from_str(legacy).expect("legacy state loads");
        assert!(s.drift_alerts.is_empty());
    }
}
