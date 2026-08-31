//! Fleet spend cockpit (CXA-F278) — the pure aggregation core behind the
//! hub-level cost panel. Every number it produces is derived from data the
//! per-project surfaces already publish ([`ProjectState::spend`],
//! `spend_today_usd`, `spend_history`, and the live [`BudgetCaps`] the cycle
//! loop honours), so the cockpit can never drift from them: it reads the same
//! fields through the same threshold primitive
//! ([`crate::policy::approaching_cap`]) instead of re-deriving the arithmetic.
//!
//! Zero IO lives here — snapshots go in, cockpit/alert decisions come out —
//! so the whole feature is testable with struct literals and the HTTP/watchdog
//! layers stay thin adapters.

use crate::config::BudgetCaps;
use crate::policy::approaching_cap;
use crate::state::{ProjectState, SpendDay};
use serde::Serialize;

/// How many closed UTC days the trailing window spans (today + the previous
/// six) — the "trailing-7-day spend" the cockpit's totals chart.
const TRAILING_DAYS: i64 = 7;

/// Where a project's spend sits against its configured cap. Uncapped projects
/// are always [`CapStatus::Ok`] — there is no line to approach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CapStatus {
    Ok,
    Approaching,
    Over,
}

/// One project's spend slice, as the cockpit's input snapshot. Broken
/// registrations (`broken = true`) carry zero spend — they are kept in the
/// snapshot so the aggregate can FLAG the under-count instead of silently
/// dropping them (mirror of the broken-project listing).
#[derive(Debug, Clone, PartialEq)]
pub struct FleetProjectSpend {
    pub id: String,
    pub name: String,
    /// Space this project belongs to, when any (`None` = unassigned).
    pub space: Option<String>,
    pub spend_total_usd: f64,
    pub spend_today_usd: f64,
    /// Spend over the trailing 7-day window (today + previous six closed days).
    pub spend_7d_usd: f64,
    pub lifetime_cap: Option<f64>,
    pub daily_cap: Option<f64>,
    pub broken: bool,
}

impl FleetProjectSpend {
    /// Snapshot one live project from its persisted state and live caps.
    /// Pure: reads fields, never the store.
    #[must_use]
    pub fn live(
        id: &str,
        name: &str,
        space: Option<String>,
        state: &ProjectState,
        today: &str,
        caps: BudgetCaps,
    ) -> Self {
        Self {
            id: id.to_owned(),
            name: name.to_owned(),
            space,
            spend_total_usd: state.spend.total_cost_usd,
            spend_today_usd: state.spend_today_usd,
            spend_7d_usd: trailing7_usd(today, state.spend_today_usd, &state.spend_history),
            lifetime_cap: caps.lifetime_usd,
            daily_cap: caps.daily_usd,
            broken: false,
        }
    }

    /// Snapshot one registered-but-unloadable project: zero spend, explicit
    /// `broken` flag, no caps — visible in the aggregate, never silently dropped.
    #[must_use]
    pub fn broken(id: &str, space: Option<String>) -> Self {
        Self {
            id: id.to_owned(),
            name: id.to_owned(),
            space,
            spend_total_usd: 0.0,
            spend_today_usd: 0.0,
            spend_7d_usd: 0.0,
            lifetime_cap: None,
            daily_cap: None,
            broken: true,
        }
    }
}

/// The cockpit's whole input: one snapshot row per registered project.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FleetSnapshot {
    pub projects: Vec<FleetProjectSpend>,
}

/// A space the cockpit rolls up into (id/name/budget come from the spaces doc;
/// the spend numbers are computed from the snapshot rows).
#[derive(Debug, Clone, PartialEq)]
pub struct FleetSpaceInfo {
    pub id: String,
    pub name: String,
    /// Monthly USD cap for the space; `<= 0.0` = uncapped.
    pub budget_usd: f64,
}

/// One project row of the cockpit output (the API's wire shape).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FleetProjectRow {
    pub id: String,
    pub name: String,
    pub space_id: Option<String>,
    pub spend_usd: f64,
    pub today_usd: f64,
    pub spend_7d_usd: f64,
    pub lifetime_cap_usd: Option<f64>,
    pub daily_cap_usd: Option<f64>,
    /// Remaining lifetime headroom (cap − spend, saturated at 0); `None` when
    /// uncapped — `null` on the wire, and headroom alerts never fire for it.
    pub headroom_usd: Option<f64>,
    /// Same, against the per-day cap.
    pub headroom_today_usd: Option<f64>,
    pub status: CapStatus,
    pub broken: bool,
}

/// One space rollup of the cockpit output.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FleetSpaceRow {
    pub id: String,
    pub name: String,
    pub spend_usd: f64,
    pub today_usd: f64,
    pub spend_7d_usd: f64,
    pub budget_usd: f64,
    pub status: CapStatus,
}

/// Fleet-level totals. `broken` counts the registrations that could not be
/// loaded — the explicit flag that keeps the total from under-counting
/// silently.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FleetTotals {
    pub spend_usd: f64,
    pub today_usd: f64,
    pub spend_7d_usd: f64,
    pub projects: usize,
    pub over: usize,
    pub approaching: usize,
    pub broken: usize,
    /// Remaining headroom under the hub-level daily soft ceiling; `None`
    /// (`null`) when the hub is uncapped (`ceiling == 0`).
    pub hub_headroom_usd: Option<f64>,
}

/// The whole cockpit payload — also the exact GET /api/fleet/spend body.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FleetCockpit {
    pub hub_ceiling_usd: f64,
    pub hub_warn_pct: f64,
    pub totals: FleetTotals,
    /// Sorted by lifetime spend, highest burn first.
    pub projects: Vec<FleetProjectRow>,
    /// Sorted by lifetime spend, highest burn first.
    pub spaces: Vec<FleetSpaceRow>,
}

/// Where a project's spend sits against its cap, using
/// [`crate::policy::approaching_cap`] as the single threshold primitive: the
/// warning band starts at `warn_pct` of the cap and the cap itself is Over.
/// Uncapped (`None`) and non-positive caps are [`CapStatus::Ok`].
#[must_use]
pub fn cap_status(spend: f64, cap: Option<f64>, warn_pct: f64) -> CapStatus {
    match cap {
        Some(cap) if cap > 0.0 => {
            if spend >= cap {
                CapStatus::Over
            } else if approaching_cap(spend, Some(cap), warn_pct) {
                CapStatus::Approaching
            } else {
                CapStatus::Ok
            }
        }
        _ => CapStatus::Ok,
    }
}

/// Remaining headroom under `cap`, saturated at zero; `None` when uncapped
/// (`None` cap or non-positive cap — same "no line" rule as [`cap_status`]).
#[must_use]
pub fn headroom(spend: f64, cap: Option<f64>) -> Option<f64> {
    cap.filter(|cap| *cap > 0.0)
        .map(|cap| (cap - spend).max(0.0))
}

/// Spend across the trailing [`TRAILING_DAYS`]-day window: today's running
/// counter plus every closed day inside the calendar window. Zero-spend days
/// are absent from `history` by design, and absent days contribute nothing —
/// so a calendar-window filter (not "last N entries") is what keeps the sum
/// honest when the project idled for days. An unparsable `today` degrades to
/// today's counter only rather than guessing a window.
#[must_use]
pub fn trailing7_usd(today: &str, spend_today: f64, history: &[SpendDay]) -> f64 {
    let window_start = (|| {
        let d = time::Date::parse(
            today,
            &time::format_description::well_known::Iso8601::DEFAULT,
        )
        .ok()?;
        Some((d - time::Duration::days(TRAILING_DAYS - 1)).to_string())
    })();
    spend_today
        + history
            .iter()
            .filter(|d| {
                window_start
                    .as_deref()
                    .is_some_and(|start| d.day.as_str() >= start && d.day.as_str() < today)
            })
            .map(|d| d.usd)
            .sum::<f64>()
}

/// Build the whole cockpit from a snapshot. Pure aggregation: totals sum every
/// row (broken rows carry zero), rows sort by burn (spend desc, id tie-break),
/// spaces roll up their projects, and counts flag over/approaching/broken.
#[must_use]
pub fn build_cockpit(
    snapshot: &FleetSnapshot,
    spaces: &[FleetSpaceInfo],
    hub_ceiling: f64,
    warn_pct: f64,
) -> FleetCockpit {
    let mut rows: Vec<FleetProjectRow> = snapshot
        .projects
        .iter()
        .map(|p| FleetProjectRow {
            id: p.id.clone(),
            name: p.name.clone(),
            space_id: p.space.clone(),
            spend_usd: p.spend_total_usd,
            today_usd: p.spend_today_usd,
            spend_7d_usd: p.spend_7d_usd,
            lifetime_cap_usd: p.lifetime_cap,
            daily_cap_usd: p.daily_cap,
            headroom_usd: headroom(p.spend_total_usd, p.lifetime_cap),
            headroom_today_usd: headroom(p.spend_today_usd, p.daily_cap),
            status: cap_status(p.spend_total_usd, p.lifetime_cap, warn_pct),
            broken: p.broken,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.spend_usd
            .partial_cmp(&a.spend_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });

    let today_total = rows.iter().map(|r| r.today_usd).sum();
    let totals = FleetTotals {
        spend_usd: rows.iter().map(|r| r.spend_usd).sum(),
        today_usd: today_total,
        spend_7d_usd: rows.iter().map(|r| r.spend_7d_usd).sum(),
        projects: rows.len(),
        over: rows.iter().filter(|r| r.status == CapStatus::Over).count(),
        approaching: rows
            .iter()
            .filter(|r| r.status == CapStatus::Approaching)
            .count(),
        broken: rows.iter().filter(|r| r.broken).count(),
        hub_headroom_usd: headroom(today_total, Some(hub_ceiling)),
    };

    // Roll every snapshot row into its space, then keep every known space
    // (zero-spend ones included, so the panel lists the org as configured).
    let mut space_rows: Vec<FleetSpaceRow> = spaces
        .iter()
        .map(|s| {
            let members: Vec<&FleetProjectSpend> = snapshot
                .projects
                .iter()
                .filter(|p| p.space.as_deref() == Some(s.id.as_str()))
                .collect();
            let spend = members.iter().map(|p| p.spend_total_usd).sum();
            FleetSpaceRow {
                id: s.id.clone(),
                name: s.name.clone(),
                spend_usd: spend,
                today_usd: members.iter().map(|p| p.spend_today_usd).sum(),
                spend_7d_usd: members.iter().map(|p| p.spend_7d_usd).sum(),
                budget_usd: s.budget_usd,
                status: cap_status(spend, Some(s.budget_usd), warn_pct),
            }
        })
        .collect();
    space_rows.sort_by(|a, b| {
        b.spend_usd
            .partial_cmp(&a.spend_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });

    FleetCockpit {
        hub_ceiling_usd: hub_ceiling,
        hub_warn_pct: warn_pct,
        totals,
        projects: rows,
        spaces: space_rows,
    }
}

/// The soft-ceiling watchdog's dedupe/re-arm decision, extracted as a pure
/// data-in/data-out function so the loop holds no logic. Mirrors the cycle's
/// `apply_budget_warnings` pattern: fire exactly once when today's fleet total
/// enters the warning band, stay quiet while it stays inside, and re-arm the
/// moment it leaves (UTC-midnight reset, or the ceiling raised) so the next
/// crossing fires again. `ceiling <= 0` (and NaN) means uncapped: never fire.
///
/// Because `spend_today` only grows within a UTC day and resets at midnight,
/// "dedupe while in band" yields exactly one alert per day per threshold.
#[must_use]
pub fn hub_soft_alert(total_today: f64, ceiling: f64, warned: bool, warn_pct: f64) -> HubAlert {
    // `<= 0` (including NaN, which fails every comparison that follows) means
    // uncapped: never fire.
    if ceiling <= 0.0 {
        return if warned {
            HubAlert::Clear
        } else {
            HubAlert::Hold
        };
    }
    match (total_today >= ceiling * warn_pct, warned) {
        (true, false) => HubAlert::Fire,
        (true, true) | (false, false) => HubAlert::Hold,
        (false, true) => HubAlert::ReArm,
    }
}

/// What the watchdog should do after one [`hub_soft_alert`] evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubAlert {
    /// Emit exactly one alert now, then remember the crossing.
    Fire,
    /// Quiet; no state change.
    Hold,
    /// The total left the band — forget the crossing so a later one fires.
    ReArm,
    /// The ceiling was removed — drop any stale arming (never fires).
    Clear,
}

/// Today's burn as an ALERT must count it: `spend_today_usd` is only
/// meaningful while `spend_day` is today — a project that has not spent since
/// an earlier UTC day still carries that day's counter, which is history, not
/// today's burn. (The cockpit's display rows deliberately keep the raw
/// counter so they reconcile field-for-field with the per-project surfaces;
/// only the alert applies this stricter filter.)
#[must_use]
pub fn live_today_spend(state: &ProjectState, today: &str) -> f64 {
    if state.spend_day == today {
        state.spend_today_usd
    } else {
        0.0
    }
}

/// The projects whose spend today is at or above their pro-rata slice of the
/// warning line (`warn_pct * ceiling / n`), as `(id, today_usd)` pairs. At or
/// above (not strictly above) so an even burn still names its contributors;
/// when the fleet total is in band this list is provably non-empty. Broken
/// rows carry zero spend and never qualify.
#[must_use]
#[allow(clippy::cast_precision_loss)] // a project count divides a money line
pub fn above_pro_rata(
    today_by_project: &[(String, f64)],
    ceiling: f64,
    warn_pct: f64,
) -> Vec<(String, f64)> {
    if today_by_project.is_empty() || ceiling <= 0.0 {
        return Vec::new();
    }
    let share = ceiling * warn_pct / today_by_project.len() as f64;
    today_by_project
        .iter()
        .filter(|(_, today)| *today >= share)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]
    use super::*;

    fn day(day: &str, usd: f64) -> SpendDay {
        SpendDay {
            day: day.to_owned(),
            usd,
        }
    }

    fn project(
        id: &str,
        total: f64,
        today: f64,
        seven: f64,
        lifetime: Option<f64>,
    ) -> FleetProjectSpend {
        FleetProjectSpend {
            id: id.to_owned(),
            name: id.to_owned(),
            space: None,
            spend_total_usd: total,
            spend_today_usd: today,
            spend_7d_usd: seven,
            lifetime_cap: lifetime,
            daily_cap: None,
            broken: false,
        }
    }

    // --- cap_status ---------------------------------------------------------

    #[test]
    fn cap_status_bands_match_the_shared_threshold_primitive() {
        assert_eq!(cap_status(79.99, Some(100.0), 0.8), CapStatus::Ok);
        assert_eq!(
            cap_status(80.0, Some(100.0), 0.8),
            CapStatus::Approaching,
            "exactly-at warn threshold"
        );
        assert_eq!(cap_status(99.99, Some(100.0), 0.8), CapStatus::Approaching);
        assert_eq!(
            cap_status(100.0, Some(100.0), 0.8),
            CapStatus::Over,
            "exactly-at cap is Over"
        );
        assert_eq!(cap_status(150.0, Some(100.0), 0.8), CapStatus::Over);
    }

    #[test]
    fn cap_status_is_ok_when_uncapped_or_cap_is_non_positive() {
        assert_eq!(cap_status(1_000.0, None, 0.8), CapStatus::Ok);
        assert_eq!(cap_status(1.0, Some(0.0), 0.8), CapStatus::Ok);
        assert_eq!(cap_status(1.0, Some(-10.0), 0.8), CapStatus::Ok);
    }

    // --- headroom -----------------------------------------------------------

    #[test]
    fn headroom_saturates_at_zero_and_is_none_when_uncapped() {
        assert_eq!(headroom(30.0, Some(100.0)), Some(70.0));
        assert_eq!(headroom(100.0, Some(100.0)), Some(0.0));
        assert_eq!(headroom(120.0, Some(100.0)), Some(0.0), "never negative");
        assert_eq!(headroom(120.0, None), None);
        assert_eq!(headroom(120.0, Some(0.0)), None);
    }

    // --- trailing 7 days ----------------------------------------------------

    #[test]
    fn trailing7_sums_today_plus_the_six_closed_days_inside_the_window() {
        let history = vec![
            day("2026-08-24", 1.0), // 8 days before today — outside
            day("2026-08-25", 2.0), // 7 days before — outside (window starts 26th)
            day("2026-08-26", 4.0),
            day("2026-08-28", 8.0), // a zero-spend day (27th) is simply absent
            day("2026-08-31", 16.0),
            day("2026-09-02", 32.0), // "future" clock skew — excluded
        ];
        // today = 2026-09-01 → window 2026-08-26..=2026-09-01.
        assert_eq!(
            trailing7_usd("2026-09-01", 0.5, &history),
            4.0 + 8.0 + 16.0 + 0.5
        );
    }

    #[test]
    fn trailing7_with_no_history_is_just_today() {
        assert_eq!(trailing7_usd("2026-09-01", 3.25, &[]), 3.25);
    }

    #[test]
    fn trailing7_degrades_to_today_when_the_date_is_unparsable() {
        let history = vec![day("2026-08-31", 16.0)];
        assert_eq!(trailing7_usd("not-a-date", 2.0, &history), 2.0);
    }

    // --- build_cockpit ------------------------------------------------------

    #[test]
    fn cockpit_sums_sorts_and_counts_the_fleet() {
        let snapshot = FleetSnapshot {
            projects: vec![
                project("small", 1.0, 0.5, 1.0, None),
                project("over", 12.0, 3.0, 9.0, Some(10.0)),
                project("approaching", 8.5, 2.0, 6.0, Some(10.0)),
                project("ok", 2.0, 0.25, 2.0, Some(100.0)),
            ],
        };
        let c = build_cockpit(&snapshot, &[], 0.0, 0.8);
        assert_eq!(c.totals.spend_usd, 23.5);
        assert_eq!(c.totals.today_usd, 5.75);
        assert_eq!(c.totals.spend_7d_usd, 18.0);
        assert_eq!(c.totals.projects, 4);
        assert_eq!(c.totals.over, 1);
        assert_eq!(c.totals.approaching, 1);
        assert_eq!(c.totals.broken, 0);
        // Highest burn first.
        let ids: Vec<&str> = c.projects.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["over", "approaching", "ok", "small"]);
        // Uncapped: headroom null, status ok.
        let small = &c.projects[3];
        assert_eq!(small.headroom_usd, None);
        assert_eq!(small.status, CapStatus::Ok);
        // Uncapped hub ceiling: hub headroom null.
        assert_eq!(c.hub_ceiling_usd, 0.0);
        assert_eq!(c.totals.hub_headroom_usd, None);
    }

    #[test]
    fn cockpit_flags_broken_rows_with_zero_spend_in_the_total() {
        let snapshot = FleetSnapshot {
            projects: vec![
                project("live", 5.0, 1.0, 4.0, None),
                FleetProjectSpend::broken("broken", None),
            ],
        };
        let c = build_cockpit(&snapshot, &[], 0.0, 0.8);
        assert_eq!(c.totals.broken, 1, "the under-count is flagged, not silent");
        assert_eq!(c.totals.spend_usd, 5.0, "broken contributes zero");
        let b = c
            .projects
            .iter()
            .find(|r| r.id == "broken")
            .expect("broken row kept");
        assert!(b.broken);
        assert_eq!(b.spend_usd, 0.0);
        assert_eq!(b.status, CapStatus::Ok);
    }

    #[test]
    fn cockpit_rolls_spaces_up_and_keeps_empty_ones() {
        let mut a = project("a", 10.0, 1.0, 8.0, None);
        a.space = Some("s1".to_owned());
        let mut b = project("b", 4.0, 2.0, 3.0, None);
        b.space = Some("s1".to_owned());
        let snapshot = FleetSnapshot {
            projects: vec![a, b, project("free", 1.0, 0.0, 1.0, None)],
        };
        let spaces = vec![
            FleetSpaceInfo {
                id: "s1".to_owned(),
                name: "Alpha".to_owned(),
                budget_usd: 20.0,
            },
            FleetSpaceInfo {
                id: "s2".to_owned(),
                name: "Empty".to_owned(),
                budget_usd: 0.0,
            },
        ];
        let c = build_cockpit(&snapshot, &spaces, 0.0, 0.8);
        assert_eq!(
            c.spaces.len(),
            2,
            "configured spaces appear even at zero spend"
        );
        let s1 = &c.spaces[0];
        assert_eq!(s1.id, "s1");
        assert_eq!(s1.spend_usd, 14.0);
        assert_eq!(s1.today_usd, 3.0);
        assert_eq!(s1.spend_7d_usd, 11.0);
        assert_eq!(
            s1.status,
            CapStatus::Ok,
            "14/20 = 70% — below the 80% warn line"
        );
        let s2 = &c.spaces[1];
        assert_eq!(s2.status, CapStatus::Ok, "zero budget = uncapped space");
    }

    #[test]
    fn an_empty_fleet_builds_an_all_zero_cockpit() {
        let c = build_cockpit(&FleetSnapshot::default(), &[], 100.0, 0.8);
        assert_eq!(c.totals.projects, 0);
        assert_eq!(c.totals.spend_usd, 0.0);
        assert_eq!(c.totals.hub_headroom_usd, Some(100.0));
        assert!(c.projects.is_empty());
        assert!(c.spaces.is_empty());
    }

    // --- hub_soft_alert -----------------------------------------------------

    #[test]
    fn hub_alert_fires_once_inside_the_band_and_holds_while_it_stays() {
        assert_eq!(hub_soft_alert(79.0, 100.0, false, 0.8), HubAlert::Hold);
        assert_eq!(
            hub_soft_alert(80.0, 100.0, false, 0.8),
            HubAlert::Fire,
            "exactly at the warn line"
        );
        assert_eq!(
            hub_soft_alert(95.0, 100.0, true, 0.8),
            HubAlert::Hold,
            "deduped while in band"
        );
        assert_eq!(
            hub_soft_alert(120.0, 100.0, true, 0.8),
            HubAlert::Hold,
            "over is still in band"
        );
    }

    #[test]
    fn hub_alert_rearms_after_leaving_the_band() {
        assert_eq!(hub_soft_alert(50.0, 100.0, true, 0.8), HubAlert::ReArm);
        assert_eq!(
            hub_soft_alert(80.0, 100.0, false, 0.8),
            HubAlert::Fire,
            "…so the next crossing fires"
        );
    }

    #[test]
    fn hub_alert_never_fires_without_a_ceiling_and_clears_stale_arming() {
        assert_eq!(hub_soft_alert(1_000.0, 0.0, false, 0.8), HubAlert::Hold);
        assert_eq!(hub_soft_alert(1_000.0, 0.0, true, 0.8), HubAlert::Clear);
        assert_eq!(hub_soft_alert(1_000.0, -5.0, true, 0.8), HubAlert::Clear);
    }

    // --- above_pro_rata -----------------------------------------------------

    #[test]
    fn pro_rata_names_the_burners_at_or_above_their_share() {
        let fleet = vec![
            ("a".to_owned(), 90.0),
            ("b".to_owned(), 5.0),
            ("c".to_owned(), 0.0),
        ];
        // warn line = 80% of 100 = 80; share = 80/3 ≈ 26.67 → only "a".
        assert_eq!(
            above_pro_rata(&fleet, 100.0, 0.8),
            vec![("a".to_owned(), 90.0)]
        );
        // Exactly-at-share burn is named too (>=): with two projects the share
        // is 40, and an even 40/40 split names both — a crossed line never
        // names nobody.
        let even = vec![("a".to_owned(), 40.0), ("b".to_owned(), 40.0)];
        let named = above_pro_rata(&even, 100.0, 0.8);
        assert_eq!(named.len(), 2);
    }

    #[test]
    fn pro_rata_is_empty_for_an_empty_or_uncapped_fleet() {
        assert!(above_pro_rata(&[], 100.0, 0.8).is_empty());
        let fleet = vec![("a".to_owned(), 5.0)];
        assert!(above_pro_rata(&fleet, 0.0, 0.8).is_empty());
    }

    // --- live snapshot ------------------------------------------------------

    #[test]
    fn live_snapshot_reads_the_same_state_fields_the_project_surfaces_publish() {
        let mut state = ProjectState::default();
        state.spend.total_cost_usd = 12.5;
        state.spend_today_usd = 3.25;
        state.spend_history.push(day("2026-08-31", 4.0));
        let snap = FleetProjectSpend::live(
            "demo",
            "Demo",
            Some("s1".to_owned()),
            &state,
            "2026-09-01",
            BudgetCaps { lifetime_usd: Some(20.0), daily_usd: Some(5.0) },
        );
        assert_eq!(snap.spend_total_usd, 12.5);
        assert_eq!(snap.spend_today_usd, 3.25);
        assert_eq!(snap.spend_7d_usd, 7.25, "today + the closed day in window");
        assert_eq!(snap.lifetime_cap, Some(20.0));
        assert!(!snap.broken);
    }

    #[test]
    fn alert_today_spend_ignores_a_stale_earlier_day_counter() {
        // A project that last spent yesterday still carries yesterday's
        // counter: the alert must not call it today's burn.
        let state = ProjectState {
            spend_today_usd: 3.0,
            spend_day: "2026-08-31".to_owned(),
            ..ProjectState::default()
        };
        assert_eq!(live_today_spend(&state, "2026-09-01"), 0.0);
        // Same day: the counter is live.
        let state = ProjectState {
            spend_day: "2026-09-01".to_owned(),
            ..state
        };
        assert_eq!(live_today_spend(&state, "2026-09-01"), 3.0);
        // A project that never spent has an empty day and no burn.
        let fresh = ProjectState::default();
        assert_eq!(live_today_spend(&fresh, "2026-09-01"), 0.0);
    }

    #[test]
    fn broken_snapshot_is_zero_spend_and_flagged() {
        let snap = FleetProjectSpend::broken("broken", None);
        assert!(snap.broken);
        assert_eq!(snap.spend_total_usd, 0.0);
        assert_eq!(snap.spend_today_usd, 0.0);
        assert_eq!(snap.lifetime_cap, None);
    }
}
