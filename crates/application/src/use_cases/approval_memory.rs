//! What this team's humans actually approve — learned from their decisions,
//! not from a config file.
//!
//! Pure over a slice of recorded samples: the caller reads them from state,
//! the rules fall out here. See docs/ADAPTIVE_APPROVAL.md.

use serde::{Deserialize, Serialize};

/// One human decision on one ticket shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalSample {
    /// [`super::approval_risk::shape_key`] of the ticket decided on.
    pub shape: String,
    /// `"approve"`, `"reject"`, or `"undo"` (a reversed auto-approval).
    pub decision: String,
    /// Who decided — rules are per-person, and named when announced.
    pub by: String,
    /// Why, when the human said (reject reasons are the useful ones).
    #[serde(default)]
    pub reason: String,
    pub at: String,
}

/// What the learner concluded about one shape.
///
/// Serialized snake_case so the policy panel (CXA-F303) can show the rule
/// exactly as the gate holds it: `"keep_asking"`, or an externally-tagged
/// `{"auto_approve":{..}}` / `{"preflight_fix":{..}}` object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    /// Enough consistent approvals: stop asking, announce instead.
    AutoApprove { by: String, samples: usize },
    /// Consistently rejected for the same reason: fix it BEFORE a human sees
    /// it — the reason is the pre-flight check to apply.
    PreflightFix { reason: String, samples: usize },
    /// Mixed, undone, or too few samples: keep asking.
    KeepAsking,
}

/// Decide what to do with `shape` given every sample recorded for it.
///
/// One `undo` outweighs the approvals before it: a person who bothers to
/// reverse an auto-approval is giving the strongest signal available.
#[must_use]
pub fn rule_for(shape: &str, samples: &[ApprovalSample], learn_after: usize) -> Rule {
    let mine: Vec<&ApprovalSample> = samples.iter().filter(|s| s.shape == shape).collect();
    if mine.iter().any(|s| s.decision == "undo") {
        return Rule::KeepAsking;
    }
    let approvals: Vec<&&ApprovalSample> =
        mine.iter().filter(|s| s.decision == "approve").collect();
    let rejects: Vec<&&ApprovalSample> = mine.iter().filter(|s| s.decision == "reject").collect();

    // Two rejections in a row retire any confidence in the shape.
    if rejects.len() >= 2 {
        // The repeated reason IS the fix to apply before asking again.
        let reason = rejects
            .iter()
            .rev()
            .find(|s| !s.reason.trim().is_empty())
            .map(|s| s.reason.clone())
            .unwrap_or_default();
        if !reason.is_empty() {
            return Rule::PreflightFix {
                reason,
                samples: rejects.len(),
            };
        }
        return Rule::KeepAsking;
    }
    if rejects.is_empty() && approvals.len() >= learn_after.max(1) {
        // Attribute to the person who decided most of them.
        let by = approvals
            .last()
            .map_or_else(|| "the team".to_owned(), |s| s.by.clone());
        return Rule::AutoApprove {
            by,
            samples: approvals.len(),
        };
    }
    Rule::KeepAsking
}

/// The announcement a human reads the first time a rule takes effect.
#[must_use]
pub fn announce(shape: &str, rule: &Rule) -> Option<String> {
    match rule {
        Rule::AutoApprove { by, samples } => Some(format!(
            "🤖 Learned: @{by} approved {samples}/{samples} `{shape}` tickets — I will \
             auto-approve that shape from now on and post a notice instead of asking. \
             Reply `ask again: {shape}` to reverse."
        )),
        Rule::PreflightFix { reason, samples } => Some(format!(
            "🤖 Learned: `{shape}` tickets were rejected {samples}× for the same reason \
             (\"{reason}\") — the BA will fix that BEFORE the ticket reaches anyone's inbox."
        )),
        Rule::KeepAsking => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(shape: &str, decision: &str, by: &str, reason: &str) -> ApprovalSample {
        ApprovalSample {
            shape: shape.to_owned(),
            decision: decision.to_owned(),
            by: by.to_owned(),
            reason: reason.to_owned(),
            at: "2026-08-02T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn consistent_approvals_become_an_auto_rule() {
        let samples: Vec<ApprovalSample> = (0..8)
            .map(|_| s("test/small", "approve", "luffy", ""))
            .collect();
        assert_eq!(
            rule_for("test/small", &samples, 8),
            Rule::AutoApprove {
                by: "luffy".to_owned(),
                samples: 8
            }
        );
        assert!(announce("test/small", &rule_for("test/small", &samples, 8))
            .is_some_and(|m| m.contains("luffy")));
    }

    #[test]
    fn one_undo_outweighs_a_pile_of_approvals() {
        let mut samples: Vec<ApprovalSample> = (0..8)
            .map(|_| s("test/small", "approve", "luffy", ""))
            .collect();
        samples.push(s("test/small", "undo", "luffy", ""));
        assert_eq!(rule_for("test/small", &samples, 8), Rule::KeepAsking);
    }

    #[test]
    fn repeated_rejections_become_a_preflight_check() {
        let samples = vec![
            s("feature/small", "reject", "luffy", "no acceptance criteria"),
            s("feature/small", "reject", "luffy", "no acceptance criteria"),
        ];
        assert_eq!(
            rule_for("feature/small", &samples, 8),
            Rule::PreflightFix {
                reason: "no acceptance criteria".to_owned(),
                samples: 2
            }
        );
    }

    #[test]
    fn too_few_or_mixed_samples_keep_asking() {
        let few: Vec<ApprovalSample> = (0..3)
            .map(|_| s("test/small", "approve", "luffy", ""))
            .collect();
        assert_eq!(rule_for("test/small", &few, 8), Rule::KeepAsking);
        let mixed = vec![
            s("test/small", "approve", "luffy", ""),
            s("test/small", "reject", "luffy", "wrong scope"),
        ];
        assert_eq!(rule_for("test/small", &mixed, 1), Rule::KeepAsking);
    }

    #[test]
    fn other_shapes_do_not_leak_into_the_decision() {
        let samples: Vec<ApprovalSample> = (0..8)
            .map(|_| s("docs/small", "approve", "luffy", ""))
            .collect();
        assert_eq!(rule_for("test/small", &samples, 8), Rule::KeepAsking);
    }
}
