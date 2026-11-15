//! Pure policy evaluation — governance decisions made from config + state, with
//! no IO, so they are testable in isolation and enforced identically wherever
//! called. Turns "human gates" into deterministic, code-checked configuration.

use crate::config::PolicyConfig;

/// Whether `model` is permitted. An empty allowlist permits everything.
#[must_use]
pub fn model_allowed(policy: &PolicyConfig, model: &str) -> bool {
    policy.model_allowlist.is_empty() || policy.model_allowlist.iter().any(|m| m == model)
}

/// The forbidden path prefixes that `changed` files touch (empty = clean).
#[must_use]
pub fn forbidden_hits<'a>(policy: &'a PolicyConfig, changed: &'a [String]) -> Vec<&'a str> {
    changed
        .iter()
        .filter(|p| {
            policy
                .forbidden_paths
                .iter()
                .any(|f| !f.is_empty() && p.starts_with(f.as_str()))
        })
        .map(String::as_str)
        .collect()
}

/// Whether `spent_today` has reached the per-day cap (if one is configured).
#[must_use]
pub fn over_daily_budget(policy: &PolicyConfig, spent_today: f64) -> bool {
    policy
        .daily_budget_usd
        .is_some_and(|cap| cap > 0.0 && spent_today >= cap)
}

/// Whether `spend` is within the warning band of `cap` — at or past `pct` of
/// it, INCLUSIVELY up to and including the cap itself. `false` when no cap is
/// configured or the cap is non-positive.
///
/// WHY (CXA-F448 billing bug 1 — cap-boundary off-by-one): the old rule ended
/// the band strictly below the cap (`spend < cap`), so a cycle landing EXACTLY
/// on the warn threshold's upper bound reported "no warning" while sitting on
/// the line; downstream pause logic (`over_daily_budget`, scrum.rs caps) fires
/// only at `>= cap`, leaving the exact-threshold boundary silently unwarned.
/// A warning is now uniformly inclusive: `spend >= cap * pct`. Pausing remains
/// the caller's stricter `>= cap` check — the two coincide for one beat at
/// exact equality, which is correct (pause beats warn), not a double-fire.
/// Blast radius audited: `fleet::cap_status` checks `>= cap` FIRST so
/// `CapStatus::Over` still wins at the boundary (unchanged rendering);
/// `presentation/server/manage.rs` warns and then continues into its own hard
/// stop at `spend >= budget` — at exact equality the warning now accompanies
/// the stop instead of being swallowed one tick earlier.
#[must_use]
pub fn approaching_cap(spend: f64, cap: Option<f64>, pct: f64) -> bool {
    cap.is_some_and(|cap| cap > 0.0 && spend >= cap * pct)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(models: &[&str], paths: &[&str], daily: Option<f64>) -> PolicyConfig {
        PolicyConfig {
            model_allowlist: models.iter().map(|s| (*s).to_owned()).collect(),
            forbidden_paths: paths.iter().map(|s| (*s).to_owned()).collect(),
            daily_budget_usd: daily,
            ..PolicyConfig::default()
        }
    }

    #[test]
    fn empty_allowlist_permits_all() {
        assert!(model_allowed(&policy(&[], &[], None), "anything"));
    }

    #[test]
    fn allowlist_gates_models() {
        let p = policy(&["sonnet", "opus"], &[], None);
        assert!(model_allowed(&p, "sonnet"));
        assert!(!model_allowed(&p, "gpt-4"));
    }

    #[test]
    fn forbidden_paths_flag_prefix_matches() {
        let p = policy(&[], &["infra/", ".github/"], None);
        let changed = vec![
            "src/main.rs".to_owned(),
            "infra/deploy.tf".to_owned(),
            ".github/workflows/ci.yml".to_owned(),
        ];
        let hits = forbidden_hits(&p, &changed);
        assert_eq!(hits, vec!["infra/deploy.tf", ".github/workflows/ci.yml"]);
    }

    #[test]
    fn daily_budget_trips_at_cap() {
        let p = policy(&[], &[], Some(10.0));
        assert!(!over_daily_budget(&p, 9.99));
        assert!(over_daily_budget(&p, 10.0));
        assert!(!over_daily_budget(&policy(&[], &[], None), 1000.0));
    }

    #[test]
    fn approaching_cap_is_false_when_no_cap_is_configured() {
        assert!(!approaching_cap(1_000.0, None, 0.8));
    }

    // CXA-F448 billing bug 1 regression: the boundary cases below were
    // silently unwarned before the inclusive-threshold fix (red without it).
    #[test]
    fn f448_approaching_cap_warns_at_exact_threshold_and_above_up_to_cap() {
        // Table: (spend, cap, pct) -> expected, incl. exact-equality edges.
        let cases = [
            (79.99, Some(100.0), 0.8, false), // just below band
            (80.00, Some(100.0), 0.8, true),  // EXACT warn threshold
            (80.01, Some(100.0), 0.8, true),
            (99.99, Some(100.0), 0.8, true),
            (100.00, Some(100.0), 0.8, true), // EXACTLY at the cap: still a warning beat
            (100.01, Some(100.0), 0.8, true), // one cent over
            (150.00, Some(100.0), 0.8, true), // full overrun
            (25.00, Some(100.0), 0.25, true), // fractional pct, exact multiply
            (24.999, Some(100.0), 0.25, false),
        ];
        for (spend, cap, pct, want) in cases {
            assert_eq!(
                approaching_cap(spend, cap, pct),
                want,
                "spend={spend} cap={cap:?} pct={pct}"
            );
        }
    }

    #[test]
    fn approaching_cap_is_false_below_the_threshold() {
        assert!(!approaching_cap(79.99, Some(100.0), 0.8));
    }

    #[test]
    fn approaching_cap_is_true_at_exactly_the_threshold() {
        assert!(approaching_cap(80.0, Some(100.0), 0.8));
    }

    // CXA-F448 billing bug 1: the band is INCLUSIVE up to and including the
    // cap — a cycle sitting exactly on (or past) the cap still counts as "at
    // the line" so the warning accompanies, never precedes silently, the stop.
    #[test]
    fn approaching_cap_stays_true_once_spend_reaches_the_cap() {
        assert!(approaching_cap(100.0, Some(100.0), 0.8));
        assert!(approaching_cap(150.0, Some(100.0), 0.8));
    }

    #[test]
    fn approaching_cap_is_false_when_cap_is_non_positive() {
        assert!(!approaching_cap(1.0, Some(0.0), 0.8));
        assert!(!approaching_cap(1.0, Some(-10.0), 0.8));
    }
}
