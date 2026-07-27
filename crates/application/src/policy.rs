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
/// it but not yet at the cap itself. `false` when no cap is configured, the
/// cap is non-positive, or spend has already reached/passed the cap (that's
/// [`over_daily_budget`]'s job, not a warning — the two never double-fire for
/// the same crossing). Single source of truth for "near the line" so callers
/// (the cycle loop, the hub-level budget watchdog) don't each re-derive the
/// arithmetic.
#[must_use]
pub fn approaching_cap(spend: f64, cap: Option<f64>, pct: f64) -> bool {
    cap.is_some_and(|cap| cap > 0.0 && spend >= cap * pct && spend < cap)
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

    #[test]
    fn approaching_cap_is_false_below_the_threshold() {
        assert!(!approaching_cap(79.99, Some(100.0), 0.8));
    }

    #[test]
    fn approaching_cap_is_true_at_exactly_the_threshold() {
        assert!(approaching_cap(80.0, Some(100.0), 0.8));
    }

    #[test]
    fn approaching_cap_is_false_once_spend_reaches_the_cap() {
        assert!(!approaching_cap(100.0, Some(100.0), 0.8));
        assert!(!approaching_cap(150.0, Some(100.0), 0.8));
    }

    #[test]
    fn approaching_cap_is_false_when_cap_is_non_positive() {
        assert!(!approaching_cap(1.0, Some(0.0), 0.8));
        assert!(!approaching_cap(1.0, Some(-10.0), 0.8));
    }
}
