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

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(models: &[&str], paths: &[&str], daily: Option<f64>) -> PolicyConfig {
        PolicyConfig {
            model_allowlist: models.iter().map(|s| (*s).to_owned()).collect(),
            forbidden_paths: paths.iter().map(|s| (*s).to_owned()).collect(),
            daily_budget_usd: daily,
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
}
