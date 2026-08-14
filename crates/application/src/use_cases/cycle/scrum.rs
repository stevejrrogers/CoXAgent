// Part of the cycle module split by concern — see cycle/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Scrum ceremonies the cycle runs between agent passes: the standing scrum
//! topic, next-feature clarification, and the per-cycle activity record.

use super::*;

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// Whether the board has any recent team activity to hold a standup over —
    /// the gate that stops empty, token-wasting standups on a quiet board.
    pub(super) async fn has_recent_activity(&self) -> bool {
        self.store
            .load()
            .await
            .is_ok_and(|s| !s.activity.is_empty())
    }

    /// Pick the most pressing thing worth a team discussion this cycle, or `None`
    /// when there's nothing to talk about (so the team isn't noisy for no reason).
    /// Returns `(category, text)` — the category keys a once-per-day claim so the
    /// SAME kind of discussion isn't re-posted every cycle (the "47 open bugs"
    /// spam), while a genuinely different topic can still fire the same day.
    pub(super) fn scrum_topic(
        state: &crate::state::ProjectState,
        report: &CycleReport,
        cycle: u64,
        lang: crate::config::Language,
    ) -> Option<(&'static str, String)> {
        use coxagent_domain::{Status, TicketType};
        let vi = lang.is_vi();
        // A failed deploy is the loudest signal — discuss root cause + prevention.
        if report.errors.iter().any(|e| e.contains("DEPLOY")) || !report.bugs_filed.is_empty() {
            return Some((
                "deploy_fail",
                if vi {
                    "Lần deploy hoặc chạy test gần nhất phát sinh lỗi. Nguyên nhân gốc có thể là gì, \
                     và ta nên thay đổi gì để nó không tái diễn?"
                } else {
                    "The last deploy or test run surfaced failures. What's the likely root cause, \
                     and what should we change to stop it recurring?"
                }
                .to_owned(),
            ));
        }
        let open_bugs = state
            .tickets
            .iter()
            .filter(|t| t.ticket_type() == TicketType::Bug && t.status() == Status::Open)
            .count();
        if open_bugs >= 3 {
            return Some(("bug_backlog", if vi {
                format!(
                    "Đang có {open_bugs} bug mở. Nên tạm dừng tính năng mới để dọn hết bug trước, \
                     hay tiếp tục ship? Quyết định đi, và nếu cần thì tạo ticket theo dõi."
                )
            } else {
                format!(
                    "We have {open_bugs} open bugs. Should we pause new features and burn down the \
                     bug backlog first, or keep shipping? Decide and, if useful, create a tracking ticket."
                )
            }));
        }
        // A stalled in-progress ticket is worth flagging as a possible blocker.
        if let Some(t) = state
            .tickets
            .iter()
            .find(|t| t.status() == Status::InProgress)
        {
            if cycle % 4 == 0 {
                return Some(("stalled", if vi {
                    format!(
                        "{} đã ở trạng thái đang làm khá lâu. Có bị block hay quá lớn không? \
                         Nên tách nhỏ hay gỡ block cho nó?",
                        t.id()
                    )
                } else {
                    format!(
                        "{} has been in progress for a while. Is it blocked or too big? \
                         Should we split it or unblock it?",
                        t.id()
                    )
                }));
            }
        }
        // Otherwise a light periodic check-in keeps the sprint honest.
        if cycle % 6 == 0 {
            return Some((
                "checkin",
                if vi {
                    "Điểm tin sprint: có đang đúng hướng với mục tiêu sprint không? Có rủi ro, phình \
                     phạm vi, hay blocker nào cần nêu? Chốt một bước tiếp theo cụ thể."
                } else {
                    "Sprint check-in: are we on track for the sprint goal? Any risks, scope creep, \
                     or blockers to raise? Decide on one concrete next step."
                }
                .to_owned(),
            ));
        }
        None
    }

    /// Clarification loop: if the next ready feature has no acceptance criteria,
    /// DEV-FEATURE "raises it" on the ticket thread and the BA jumps in to pin
    /// down the definition of done — so nobody builds against a fuzzy spec.
    /// Best-effort, at most one clarification per cycle.
    pub(super) async fn clarify_next_feature(&self) {
        use coxagent_domain::ticket::{Status, TicketType};
        let Ok(state) = self.store.load().await else {
            return;
        };
        let Some((id, title)) = state
            .tickets
            .iter()
            .find(|t| {
                t.ticket_type() == TicketType::Feature
                    && t.status() == Status::Ready
                    && t.acceptance_criteria().is_empty()
            })
            .map(|t| (t.id().clone(), t.title().to_owned()))
        else {
            return;
        };

        // DEV flags it — in a human tone — on the ticket thread.
        if let Ok(mut s) = self.store.load().await {
            s.post_comment(
                "DEV-FEATURE",
                &format!(
                    "Hold on — {id} has no acceptance criteria. I'm not going to guess what \
                     \"done\" means and risk building the wrong thing. BA, can you pin it down?"
                ),
                Some(id.to_string()),
            );
            let _ = self.store.save(&s).await;
        }

        // BA answers by generating concrete criteria.
        let request = AgentRequest {
            role: coxagent_domain::Role::Ba,
            system_prompt: crate::prompts::system_prompt(crate::prompts::BA),
            task_prompt: format!(
                "A developer flagged that ticket {id} (\"{title}\") has no acceptance criteria \
                 and won't start without them. Write 2-5 concrete, testable acceptance criteria \
                 (user-visible behaviour, not implementation). Respond with ONLY a JSON array of \
                 strings."
            ),
            work_dir: self.work_dir.clone(),
            timeout: std::time::Duration::from_secs(300),
            escalation_level: 0,
            label: Some(id.to_string()),
        };
        let Ok(outcome) = self.engine.run(request).await else {
            return;
        };
        if !outcome.succeeded() {
            return;
        }
        let Ok(criteria) = crate::parsing::parse_string_list(&outcome.stdout) else {
            return;
        };
        let criteria: Vec<String> = criteria.into_iter().take(5).collect();
        if criteria.is_empty() {
            return;
        }
        if let Ok(mut s) = self.store.load().await {
            if let Some(t) = s.ticket_mut(&id) {
                t.set_acceptance_criteria(criteria.clone());
            }
            s.post_comment(
                "BA",
                &format!(
                    "Good catch — my bad for leaving {id} fuzzy. Definition of done: {}. \
                     Updated the ticket, you're clear to build.",
                    criteria.join("; ")
                ),
                Some(id.to_string()),
            );
            s.log_activity("BA", "clarified acceptance criteria", Some(id.to_string()));
            let _ = self.store.save(&s).await;
        }
    }

    /// Append a human-readable activity trail plus drain the spend meter into
    /// state. Returns whether accumulated spend has crossed the budget cap.
    /// Best-effort: a failure here never fails a cycle.
    /// Fold the spend meter's since-last-cycle deltas into persistent state and
    /// reset it, returning this cycle's cost. Kept separate so `record_activity`
    /// stays a readable list of what happened, not a ledger.
    fn drain_meter(&self, state: &mut crate::state::ProjectState) -> (f64, u64) {
        let mut cycle_cost = 0.0;
        let mut cycle_runs = 0u64;
        if let Some(meter) = &self.meter {
            if let Ok(mut m) = meter.lock() {
                cycle_cost = m.total_cost_usd;
                cycle_runs = m.runs;
                state.spend.total_cost_usd += m.total_cost_usd;
                state.spend.input_tokens += m.input_tokens;
                state.spend.output_tokens += m.output_tokens;
                state.spend.runs += m.runs;
                for (role, cost) in std::mem::take(&mut m.by_role) {
                    *state.spend.by_role.entry(role).or_default() += cost;
                }
                for (role, n) in std::mem::take(&mut m.runs_by_role) {
                    *state.spend.runs_by_role.entry(role).or_default() += n;
                }
                for (role, cost) in std::mem::take(&mut m.metered_cost_by_role) {
                    *state.spend.metered_cost_by_role.entry(role).or_default() += cost;
                }
                // Live engine per role (last-wins), plus the operator that ran it
                // — so each agent card can name its real engine and its user.
                for (role, eng) in std::mem::take(&mut m.engine_by_role) {
                    if !self.worker.is_empty() {
                        state
                            .spend
                            .operator_by_role
                            .insert(role.clone(), self.worker.clone());
                    }
                    state.spend.engine_by_role.insert(role, eng);
                }
                // Attribute this cycle's spend to the operator that ran it, so
                // each user's token usage is measurable in a shared project.
                if !self.worker.is_empty() {
                    let op = state
                        .spend
                        .by_operator
                        .entry(self.worker.clone())
                        .or_default();
                    op.cost_usd += m.total_cost_usd;
                    op.input_tokens += m.input_tokens;
                    op.output_tokens += m.output_tokens;
                    op.runs += m.runs;
                }
                *m = Spend::default();
            }
        }
        (cycle_cost, cycle_runs)
    }

    /// Deterministic per-cycle scorecard — zero tokens, graded from what the
    /// cycle actually recorded. "Was the cycle worth its cost?" as chartable
    /// data instead of a feeling. Bounded history, newest last.
    fn record_cycle_score(
        state: &mut crate::state::ProjectState,
        report: &CycleReport,
        cost_usd: f64,
        runs: u64,
    ) {
        use crate::state::CycleScore;
        let shipped =
            u64::from(report.feature_done.is_some()) + u64::from(report.bug_fixed.is_some());
        let useful = shipped
            + report.ba_created.len() as u64
            + u64::from(report.sa_readied.is_some())
            + u64::from(report.pd_designed.is_some())
            + u64::from(report.documented.is_some())
            + report.bugs_filed.len() as u64;
        let errors = report
            .errors
            .iter()
            .filter(|e| !e.contains("paused by self-tuning") && !e.contains("skipped"))
            .count() as u64;
        let incidents = state.engine_incidents.len() as u64;
        let grade = CycleScore::grade_of(shipped, runs, useful, incidents, errors);
        // The cycle counter is per-RUNNER (local, starts at 1 in the app loop).
        // When the leader lease hands over — another runner takes the wheel, or
        // the same runner restarts mid-run — its counter resets, so 'cycle 1'
        // gets scored again and again. Each of those is a DIFFERENT runner's
        // fresh-work cycle, but collapsing them all under the same number floods
        // the bounded history with duplicate-number noise and evicts the real
        // scores. Keep the number spending-monotonic: only accept a cycle that
        // advances past the largest already scored. The runner's local counter
        // is still used everywhere else (scrum/sprint/debt cadence); only the
        // scored *history key* is deduped so the chart reflects project cycles,
        // not leader churn.
        let scored_max = state.cycle_scores.iter().map(|c| c.cycle).max();
        if !should_record_cycle(scored_max, report.cycle) {
            return;
        }
        state.cycle_scores.push(CycleScore {
            cycle: report.cycle,
            at: crate::state::now_rfc3339(),
            runs,
            useful,
            cost_usd,
            shipped,
            incidents,
            errors,
            grade,
        });
        let overflow = state.cycle_scores.len().saturating_sub(100);
        if overflow > 0 {
            state.cycle_scores.drain(0..overflow);
        }
    }

    pub(super) async fn record_activity(&self, report: &CycleReport, leader: bool) -> bool {
        let Ok(mut state) = self.store.load().await else {
            return false;
        };
        for id in &report.ba_created {
            state.log_activity("BA", "proposed feature", Some(id.to_string()));
        }
        if let Some(id) = &report.sa_readied {
            state.log_activity("SA", "designed (technical)", Some(id.to_string()));
        }
        if report.design_system_created {
            state.log_activity("PD", "established design system", None);
        }
        if let Some(id) = &report.pd_designed {
            state.log_activity("PD", "designed UX & readied", Some(id.to_string()));
        }
        if let Some(id) = &report.bug_fixed {
            state.log_activity("DEV-BUG", "fixed bug", Some(id.to_string()));
        }
        if let Some(id) = &report.feature_done {
            state.log_activity("DEV-FEATURE", "implemented feature", Some(id.to_string()));
        }
        if let Some(id) = &report.documented {
            state.log_activity("DOCS", "documented", Some(id.to_string()));
        }
        for id in &report.bugs_filed {
            state.log_activity("TEST", "filed bug", Some(id.to_string()));
        }

        // Drain the spend meter (deltas since last cycle) into persistent state.
        let (cycle_cost, cycle_runs) = self.drain_meter(&mut state);
        // LEADER-ONLY: every runner passes through here, and the workers cycle
        // every few seconds — letting them all score flooded the history with
        // duplicate/no-op rows within minutes of the feature shipping.
        if leader {
            Self::record_cycle_score(&mut state, report, cycle_cost, cycle_runs);
        }
        let spent_today = state.add_daily_spend(cycle_cost);

        // Effective caps: the live cell (adjustable without restart) when present,
        // otherwise the caps from the loaded config.
        let (lifetime_cap, daily_cap) = self.budget.as_ref().map_or_else(
            || {
                (
                    self.config.workflow.budget_usd,
                    self.config.policy.daily_budget_usd,
                )
            },
            |b| {
                b.lock().map_or(
                    (
                        self.config.workflow.budget_usd,
                        self.config.policy.daily_budget_usd,
                    ),
                    |caps| (caps.lifetime_usd, caps.daily_usd),
                )
            },
        );
        // Pause on either the lifetime cap or the per-day cap.
        let over_lifetime =
            lifetime_cap.is_some_and(|cap| cap > 0.0 && state.spend.total_cost_usd >= cap);
        let over_daily = daily_cap.is_some_and(|cap| cap > 0.0 && spent_today >= cap);
        if over_daily {
            state.log_activity("POLICY", "daily budget cap reached", None);
        }

        let warn_pct = self.config.policy.budget_warn_pct;
        let (new_lifetime_warning, new_daily_warning) = apply_budget_warnings(
            &mut state,
            warn_pct,
            lifetime_cap,
            daily_cap,
            spent_today,
            over_lifetime,
            over_daily,
        );

        let _ = self.store.save(&state).await;
        self.notify_budget_warnings(new_lifetime_warning, new_daily_warning, warn_pct)
            .await;

        over_lifetime || over_daily
    }
}

/// Pure gate deciding whether a freshly-scored cycle advances the bounded
/// history. The scored *key* (`report.cycle`) must be strictly greater than
/// every cycle already in the buffer; a duplicate or stale number (a leader
/// handover / runner restart renumbering from 1) is dropped so it can't evict
/// a real score. Split out of `record_cycle_score` so the dedupe policy is
/// testable without a store/engine.
fn should_record_cycle(scored_max: Option<u64>, new_cycle: u64) -> bool {
    // MSRV 1.80 predates Option::is_none_or.
    scored_max.map_or(true, |m| new_cycle > m)
}

#[cfg(test)]
mod tests {
    use super::should_record_cycle;

    #[test]
    fn accepts_first_cycle_when_buffer_empty() {
        assert!(should_record_cycle(None, 1));
    }

    #[test]
    fn accepts_monotonic_advancing_cycles() {
        assert!(should_record_cycle(Some(1), 2));
        assert!(should_record_cycle(Some(41), 42));
    }

    #[test]
    fn rejects_duplicate_and_stale_cycles() {
        // Duplicate of the current max — a leader handover renames a fresh
        // run's work back to the same number.
        assert!(!should_record_cycle(Some(3), 3));
        // Stale re-run of an early number after a restart.
        assert!(!should_record_cycle(Some(42), 1));
        assert!(!should_record_cycle(Some(42), 41));
    }
}
