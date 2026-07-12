//! Team metrics — computed purely from state so the dashboard, reports, and the
//! eventual SM retro all read the same numbers. No IO, fully testable.

use crate::state::ProjectState;
use coxagent_domain::{Status, TicketType};
use serde::Serialize;
use std::collections::BTreeMap;

/// A snapshot of team health derived from the project state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Metrics {
    pub version: String,
    pub total_tickets: usize,
    pub by_status: BTreeMap<String, usize>,
    pub features_shipped: usize,
    pub features_in_flight: usize,
    pub bugs_open: usize,
    pub bugs_verified: usize,
    pub releases: usize,
    /// Deploys per day (RFC3339 date -> count), oldest first — a burndown source.
    pub deploys_by_day: Vec<DayCount>,
    /// Share of shipped work that was features vs bug fixes (0-100).
    pub feature_ratio_pct: u32,
}

/// A single day's deploy count for the activity sparkline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DayCount {
    pub day: String,
    pub count: usize,
}

/// Compute metrics from the current state.
#[must_use]
pub fn compute(state: &ProjectState) -> Metrics {
    let mut by_status: BTreeMap<String, usize> = BTreeMap::new();
    let mut features_shipped = 0;
    let mut features_in_flight = 0;
    let mut bugs_open = 0;
    let mut bugs_verified = 0;

    for t in &state.tickets {
        *by_status
            .entry(status_key(t.status()).to_owned())
            .or_default() += 1;
        let is_feature = matches!(t.ticket_type(), TicketType::Feature | TicketType::Chore);
        if is_feature && matches!(t.status(), Status::Done | Status::Documented) {
            features_shipped += 1;
        }
        if is_feature && matches!(t.status(), Status::Ready | Status::InProgress) {
            features_in_flight += 1;
        }
        if t.ticket_type() == TicketType::Bug {
            match t.status() {
                Status::Open => bugs_open += 1,
                Status::Verified => bugs_verified += 1,
                _ => {}
            }
        }
    }

    let mut day_map: BTreeMap<String, usize> = BTreeMap::new();
    let mut feature_deploys: usize = 0;
    for rec in &state.history {
        let day = rec.at.split('T').next().unwrap_or(&rec.at).to_owned();
        *day_map.entry(day).or_default() += 1;
        if is_feature_id(rec.ticket.as_str()) {
            feature_deploys += 1;
        }
    }
    let deploys_by_day = day_map
        .into_iter()
        .map(|(day, count)| DayCount { day, count })
        .collect();

    let releases = state.history.len();
    let feature_ratio_pct = (feature_deploys * 100)
        .checked_div(releases)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);

    Metrics {
        version: state.current_version.to_string(),
        total_tickets: state.tickets.len(),
        by_status,
        features_shipped,
        features_in_flight,
        bugs_open,
        bugs_verified,
        releases,
        deploys_by_day,
        feature_ratio_pct,
    }
}

/// Whether a ticket id denotes a feature or chore. Ids are `F001` / `C001` or
/// alias-prefixed `CXC-F001`; the type code is the first char of the last
/// dash-segment.
fn is_feature_id(id: &str) -> bool {
    let seg = id.rsplit('-').next().unwrap_or(id);
    matches!(seg.chars().next(), Some('F' | 'C'))
}

fn status_key(s: Status) -> &'static str {
    match s {
        Status::Pending => "pending",
        Status::Ready => "ready",
        Status::InProgress => "in_progress",
        Status::Done => "done",
        Status::Documented => "documented",
        Status::Rejected => "rejected",
        Status::Open => "open",
        Status::Fixed => "fixed",
        Status::Verified => "verified",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DeployRecord;
    use coxagent_domain::{SemVer, Ticket, TicketId};

    fn ticket(id: &str, ty: TicketType, status: Status) -> Ticket {
        // Build via serde to place it directly in a target status for the test.
        let json = serde_json::json!({
            "id": id, "type": ty_key(ty), "title": "t", "description": "",
            "priority": "medium", "complexity": "small", "status": status_key(status),
            "has_ui": false, "design": {"technical": null, "ux": null},
            "parent_id": null, "depends_on": []
        });
        serde_json::from_value(json).expect("ticket")
    }
    fn ty_key(t: TicketType) -> &'static str {
        match t {
            TicketType::Feature => "feature",
            TicketType::Bug => "bug",
            TicketType::Chore => "chore",
        }
    }

    #[test]
    fn counts_shipped_features_and_open_bugs() {
        let state = ProjectState {
            current_version: SemVer::new(1, 0, 0),
            tickets: vec![
                ticket("F001", TicketType::Feature, Status::Done),
                ticket("F002", TicketType::Feature, Status::InProgress),
                ticket("B001", TicketType::Bug, Status::Open),
                ticket("B002", TicketType::Bug, Status::Verified),
            ],
            history: vec![DeployRecord {
                version: SemVer::new(1, 0, 0),
                ticket: TicketId::new("F001").expect("id"),
                title: "t".to_owned(),
                at: "2026-07-12T10:00:00Z".to_owned(),
            }],
            ..ProjectState::default()
        };
        let m = compute(&state);
        assert_eq!(m.features_shipped, 1);
        assert_eq!(m.features_in_flight, 1);
        assert_eq!(m.bugs_open, 1);
        assert_eq!(m.bugs_verified, 1);
        assert_eq!(m.releases, 1);
        assert_eq!(m.feature_ratio_pct, 100);
        assert_eq!(m.deploys_by_day.len(), 1);
    }

    #[test]
    fn empty_state_is_all_zeroes() {
        let m = compute(&ProjectState::default());
        assert_eq!(m.total_tickets, 0);
        assert_eq!(m.releases, 0);
        assert_eq!(m.feature_ratio_pct, 0);
    }
}
