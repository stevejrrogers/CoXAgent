//! Bug burn-down analytics (CXA-F032): the per-day open-bug history behind
//! "is the backlog actually burning down, or growing?" — pure computes over
//! state, re-exported through `crate::metrics` so all readers share one path.

use crate::state::ProjectState;
use coxagent_domain::{Status, TicketType};
use serde::Serialize;

/// The bug-status mix right now, as one burn-down point. Pure.
#[must_use]
pub fn bug_status_counts(state: &ProjectState) -> crate::state::BugSnapshot {
    let mut open = 0_u32;
    let mut fixed = 0_u32;
    let mut verified = 0_u32;
    for t in &state.tickets {
        if t.ticket_type() != TicketType::Bug {
            continue;
        }
        match t.status() {
            Status::Open => open += 1,
            Status::Fixed => fixed += 1,
            Status::Verified => verified += 1,
            _ => {}
        }
    }
    crate::state::BugSnapshot {
        open,
        fixed,
        verified,
    }
}

/// Record the day's bug counts exactly once: a re-record for the same day is a
/// no-op (restarts and multi-operator races are idempotent — the same law
/// `last_digest_day` obeys), and the map is pruned to
/// [`crate::state::MAX_BUG_SNAPSHOT_DAYS`] so it cannot grow without bound.
/// Pure over state; the leader cycle calls it inside its usual once-a-day
/// mutate.
pub fn record_burndown_snapshot(state: &mut ProjectState, day: &str) {
    if state.bug_snapshots.contains_key(day) {
        return;
    }
    let counts = bug_status_counts(state);
    state.bug_snapshots.insert(day.to_owned(), counts);
    let overflow = state
        .bug_snapshots
        .len()
        .saturating_sub(crate::state::MAX_BUG_SNAPSHOT_DAYS);
    // BTreeMap iterates in key (date) order: the first keys are the oldest days.
    let oldest: Vec<String> = state.bug_snapshots.keys().take(overflow).cloned().collect();
    for day in oldest {
        state.bug_snapshots.remove(&day);
    }
}

/// One day of the burn-down series.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BurndownDay {
    pub day: String,
    pub open: u32,
    pub fixed: u32,
    pub verified: u32,
}

/// The bug burn-down answer: where the backlog has been, and whether today it
/// shrank or grew (CXA-F032).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Burndown {
    /// Recorded days, oldest first. A day with no snapshot is OMITTED — a gap
    /// is honest, an interpolated point is not.
    pub series: Vec<BurndownDay>,
    /// Net open-bug change across the last day pair (`prev.open - latest.open`):
    /// positive means the backlog burned down, negative means it grew. 0 when
    /// fewer than two days are known. With contiguous daily snapshots this is
    /// the 24h delta; if recording skipped days it is the change since the
    /// previous KNOWN day — the best available trend signal, not an invented one.
    pub delta_24h: i64,
}

/// Default dashboard window for the burn-down chart.
pub const BURNDOWN_WINDOW_DAYS: usize = 30;

/// Burn-down series over the last `days` calendar days ending at `now_day`
/// (`YYYY-MM-DD`, injected so the compute is fully deterministic — same
/// convention as `metrics_health::compute_cycle_perf`). Pure.
///
/// Today always appears: when the leader has not recorded its snapshot yet,
/// the LIVE counts stand in (exactly what the day's snapshot will record), so
/// "net burned today" is measurable all day, not only after the leader ran.
#[must_use]
pub fn compute_burndown(state: &ProjectState, now_day: &str, days: usize) -> Burndown {
    let span = days.max(1);
    let live_today = bug_status_counts(state);
    let mut series = Vec::with_capacity(span);
    for i in (0..span).rev() {
        let day = crate::metrics::days_back(now_day, u64::try_from(i).unwrap_or(0));
        let Some(snap) = state
            .bug_snapshots
            .get(&day)
            .or((day == now_day).then_some(&live_today))
        else {
            continue;
        };
        series.push(BurndownDay {
            day,
            open: snap.open,
            fixed: snap.fixed,
            verified: snap.verified,
        });
    }
    let delta_24h = match series.as_slice() {
        [.., prev, latest] => i64::from(prev.open) - i64::from(latest.open),
        _ => 0,
    };
    Burndown { series, delta_24h }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::BugSnapshot;

    /// A ticket placed directly in `status` (serde-built, the same fixture
    /// style as `metrics::tests` and `metrics_health::tests`).
    fn ticket(id: &str, status: Status) -> coxagent_domain::Ticket {
        let status_key = |s: Status| -> &'static str {
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
                Status::OnHold => "on_hold",
            }
        };
        let json = serde_json::json!({
            "id": id, "type": "bug", "title": "t", "description": "",
            "priority": "medium", "complexity": "small", "status": status_key(status),
            "has_ui": false, "design": {"technical": null, "ux": null},
            "parent_id": null, "depends_on": []
        });
        serde_json::from_value(json).expect("ticket")
    }

    /// A state with the given bug-status mix live in `tickets`.
    fn bug_state(mix: &[(&str, Status)]) -> ProjectState {
        let mut s = ProjectState::default();
        for (id, status) in mix {
            s.tickets.push(ticket(id, *status));
        }
        s
    }

    #[test]
    fn burndown_empty_history_shows_live_today_only() {
        let s = bug_state(&[
            ("B1", Status::Open),
            ("B2", Status::Fixed),
            ("B3", Status::Verified),
        ]);
        let b = compute_burndown(&s, "2026-08-28", 7);
        // No recorded history: today stands in with LIVE counts, past days are
        // honestly absent — nothing is fabricated.
        assert_eq!(b.series.len(), 1);
        assert_eq!(b.series[0].day, "2026-08-28");
        assert_eq!(b.series[0].open, 1);
        assert_eq!(b.series[0].fixed, 1);
        assert_eq!(b.series[0].verified, 1);
        assert_eq!(b.delta_24h, 0, "no previous day, no delta");
    }

    #[test]
    fn burndown_reads_recorded_days_and_live_today() {
        let mut s = bug_state(&[("B1", Status::Open), ("B2", Status::Open)]);
        s.bug_snapshots.insert(
            "2026-08-26".to_owned(),
            BugSnapshot {
                open: 5,
                fixed: 1,
                verified: 2,
            },
        );
        s.bug_snapshots.insert(
            "2026-08-27".to_owned(),
            BugSnapshot {
                open: 4,
                fixed: 2,
                verified: 3,
            },
        );
        let b = compute_burndown(&s, "2026-08-28", 3);
        assert_eq!(b.series.len(), 3, "two recorded days + live today");
        assert_eq!(b.series[0].day, "2026-08-26");
        assert_eq!(b.series[0].open, 5);
        assert_eq!(b.series[1].day, "2026-08-27");
        assert_eq!(b.series[2].day, "2026-08-28");
        assert_eq!(b.series[2].open, 2, "today uses the LIVE counts");
        // Net burned today: yesterday's 4 open minus today's 2 = 2 fixed-off.
        assert_eq!(b.delta_24h, 2);
    }

    #[test]
    fn burndown_delta_tracks_growth_and_decay() {
        let mut s = ProjectState::default();
        s.bug_snapshots.insert(
            "2026-08-27".to_owned(),
            BugSnapshot {
                open: 2,
                fixed: 0,
                verified: 9,
            },
        );
        s.bug_snapshots.insert(
            "2026-08-28".to_owned(),
            BugSnapshot {
                open: 5,
                fixed: 1,
                verified: 9,
            },
        );
        let grew = compute_burndown(&s, "2026-08-28", 2);
        assert_eq!(grew.delta_24h, -3, "backlog grew by 3 -> negative delta");

        let mut s = ProjectState::default();
        s.bug_snapshots.insert(
            "2026-08-27".to_owned(),
            BugSnapshot {
                open: 5,
                fixed: 0,
                verified: 9,
            },
        );
        s.bug_snapshots.insert(
            "2026-08-28".to_owned(),
            BugSnapshot {
                open: 2,
                fixed: 0,
                verified: 9,
            },
        );
        let burned = compute_burndown(&s, "2026-08-28", 2);
        assert_eq!(burned.delta_24h, 3, "backlog burned 3 -> positive delta");
    }

    #[test]
    fn burndown_gaps_are_omitted_not_fabricated() {
        let mut s = ProjectState::default();
        s.bug_snapshots.insert(
            "2026-08-25".to_owned(),
            BugSnapshot {
                open: 7,
                fixed: 0,
                verified: 0,
            },
        );
        let b = compute_burndown(&s, "2026-08-28", 5);
        // Only the recorded day and live today appear; the unrecorded middle
        // days are gaps, not interpolated zeroes.
        let days: Vec<&str> = b.series.iter().map(|d| d.day.as_str()).collect();
        assert_eq!(days, vec!["2026-08-25", "2026-08-28"]);
    }

    #[test]
    fn burndown_recorded_today_wins_over_live_counts() {
        // The leader already recorded today, then the live backlog changed
        // (a new bug filed, say): the series must stay pinned to what was
        // recorded — the snapshot is the day's truth, not a moving target.
        let mut s = bug_state(&[("B1", Status::Open), ("B2", Status::Open)]);
        s.bug_snapshots.insert(
            "2026-08-27".to_owned(),
            BugSnapshot {
                open: 4,
                fixed: 0,
                verified: 0,
            },
        );
        s.bug_snapshots.insert(
            "2026-08-28".to_owned(),
            BugSnapshot {
                open: 2,
                fixed: 0,
                verified: 0,
            },
        );
        let b = compute_burndown(&s, "2026-08-28", 2);
        assert_eq!(b.series[1].open, 2, "today uses the RECORDED snapshot");
        assert_eq!(b.delta_24h, 2, "delta reads the recorded pair");
        // A zero/negative window still yields exactly today's row.
        let b = compute_burndown(&s, "2026-08-28", 0);
        assert_eq!(b.series.len(), 1);
        assert_eq!(b.delta_24h, 0);
    }

    #[test]
    fn record_burndown_snapshot_is_once_per_day_and_bounded() {
        let mut s = bug_state(&[("B1", Status::Open), ("B2", Status::Verified)]);
        record_burndown_snapshot(&mut s, "2026-08-28");
        // Change the live counts, re-record the same day: the snapshot must
        // stay exactly the first recording (dedup like last_digest_day).
        s.tickets.push(ticket("B3", Status::Open));
        record_burndown_snapshot(&mut s, "2026-08-28");
        assert_eq!(s.bug_snapshots.len(), 1);
        assert_eq!(
            s.bug_snapshots["2026-08-28"],
            BugSnapshot {
                open: 1,
                fixed: 0,
                verified: 1
            }
        );
        // A different day records; exceeding the cap drops the OLDEST day.
        for d in 29..=31 {
            record_burndown_snapshot(&mut s, &format!("2026-08-{d}"));
        }
        assert!(s.bug_snapshots.contains_key("2026-08-29"));
        let kept = s.bug_snapshots.len();
        assert!(kept <= crate::state::MAX_BUG_SNAPSHOT_DAYS);
        // Oldest-first eviction: after a flood of days, the earliest are gone
        // and today survives.
        let mut s = ProjectState::default();
        for i in 0..(crate::state::MAX_BUG_SNAPSHOT_DAYS + 5) {
            let day = crate::metrics::days_back("2026-08-28", u64::try_from(i).unwrap_or(0));
            record_burndown_snapshot(&mut s, &day);
        }
        assert_eq!(s.bug_snapshots.len(), crate::state::MAX_BUG_SNAPSHOT_DAYS);
        assert!(
            !s.bug_snapshots.contains_key(&crate::metrics::days_back(
                "2026-08-28",
                crate::state::MAX_BUG_SNAPSHOT_DAYS as u64 + 4
            )),
            "the oldest days are evicted first"
        );
        assert!(s.bug_snapshots.contains_key("2026-08-28"));
    }
}
