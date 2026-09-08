// Part of the `state` module split by bounded context — see state/mod.rs.
#![allow(clippy::wildcard_imports)]
//! The shard seam (CXA-C019a): `ProjectState` is persisted as ONE document,
//! but its fields belong to five bounded contexts. This module cuts that
//! boundary WITHOUT changing any behaviour, wire shape or store schema —
//! `ProjectState` keeps every field it has today and the stores keep writing
//! the exact same JSON.
//!
//! What lives here:
//! - [`ShardKind`] — the five bounded contexts (`Work` is the core+work
//!   context that carries the aggregate's identity fields).
//! - One payload struct per shard (`WorkShard`, `SocialShard`, …) whose
//!   fields mirror the `ProjectState` fields of that context verbatim.
//! - [`ShardedState`] — the whole aggregate decomposed, a typed field→shard
//!   map.
//! - [`ProjectState::into_shards`] / [`ProjectState::from_shards`] — pure
//!   projections over an already-loaded snapshot (same discipline as
//!   `state::integrity`): no IO, no clock, no engine.
//!
//! The anti-drift guard is the destructure itself: `into_shards`,
//! `from_shards` and `with_shard` pattern-match WITHOUT a `..` wildcard, and
//! the workspace denies warnings — so adding a `ProjectState` field without
//! assigning it a shard fails compilation, and a field assigned to two
//! shards fails the payload-struct definition. Shard payloads are disjoint
//! by construction; the unit tests additionally prove the serialized
//! document partitions exactly.

use serde::{Deserialize, Serialize};

use super::*;

/// Which bounded-context slice of the aggregate a shard payload carries.
/// Mirrors the context modules the state types already live in
/// (`state/{chat,docs,goals,governance,lessons,drift,ops,work}.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShardKind {
    /// Core identity + the work context (`state/work.rs`): the aggregate's
    /// identity fields, tickets, sprints, per-ticket ledgers, spend and
    /// self-tuning.
    Work,
    /// The social context (`state/chat.rs`): chat, channels, comments and
    /// agent questions.
    Social,
    /// The docs context (`state/docs.rs`): wiki pages, folders, refresh
    /// bookkeeping and the design system.
    Docs,
    /// The governance context (`state/goals.rs`, `governance.rs`,
    /// `lessons.rs`, `drift.rs`): goals, decisions, lessons, operator
    /// interventions and architecture-drift alerts.
    Governance,
    /// The ops context (`state/ops.rs`): deploys, rollbacks, incidents,
    /// engine health, queued jobs and run control.
    Ops,
}

impl ShardKind {
    /// Every shard kind — the full partition of `ProjectState`.
    pub const ALL: [ShardKind; 5] = [
        ShardKind::Work,
        ShardKind::Social,
        ShardKind::Docs,
        ShardKind::Governance,
        ShardKind::Ops,
    ];
}

/// The Work shard: core identity + the work context. The largest shard —
/// the ticket/sprint/spend/tuning bulk of the aggregate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkShard {
    pub schema_version: u32,
    pub alias: String,
    pub display_name: Option<String>,
    pub current_version: SemVer,
    pub tickets: Vec<Ticket>,
    pub activity: Vec<ActivityEntry>,
    pub sprint: Option<Sprint>,
    pub sprints: Vec<SprintRecord>,
    pub sprint_queue: Vec<PlannedSprint>,
    pub hold_reasons: std::collections::BTreeMap<String, String>,
    pub role_health: std::collections::BTreeMap<String, RoleHealth>,
    pub reviews: Vec<PrReview>,
    pub open_prs: Vec<crate::ports::outbound::PrOpen>,
    pub ticket_attachments: std::collections::BTreeMap<String, Vec<TicketAttachment>>,
    pub milestones: Vec<Milestone>,
    pub cycle_scores: Vec<CycleScore>,
    pub sprint_cycle: u64,
    pub cycle: u64,
    pub sprint_goal: String,
    pub last_digest_day: String,
    pub last_status_bucket: String,
    pub last_status_hash: u64,
    #[serde(skip_serializing_if = "StatusDigestSnapshot::is_empty")]
    pub last_status_snapshot: StatusDigestSnapshot,
    pub status_digest_quiet: u32,
    pub pr_fix_attempts: std::collections::BTreeMap<u64, u32>,
    pub pr_review_skips: std::collections::BTreeMap<u64, u32>,
    pub pr_open_holds: std::collections::BTreeMap<u64, PrOpenHold>,
    pub pr_sessions: std::collections::BTreeMap<u64, String>,
    pub ticket_sessions: std::collections::BTreeMap<String, String>,
    pub cost_holds: std::collections::BTreeMap<String, f64>,
    pub clippy_baseline: Option<u64>,
    pub debt_signals: Vec<DebtSignal>,
    pub sweeps_done: Vec<u64>,
    pub seen_merged_prs: std::collections::BTreeSet<u64>,
    pub ticket_last_merge: std::collections::BTreeMap<String, String>,
    pub seen_closed_prs: std::collections::BTreeSet<u64>,
    pub swept_tickets: std::collections::BTreeSet<String>,
    pub cost_approved: std::collections::BTreeSet<String>,
    pub tuning: Tuning,
    pub tuning_overrides: std::collections::BTreeMap<String, BrakeHold>,
    pub tuning_history: Vec<TuningAuditEntry>,
    pub ticket_evidence: std::collections::BTreeMap<String, Vec<Evidence>>,
    pub ticket_step_provenance: std::collections::BTreeMap<String, Vec<StepProvenance>>,
    pub repro_urls: std::collections::BTreeMap<String, String>,
    pub queue_recovery: bool,
    pub last_memory_hygiene_day: String,
    pub last_impediment_day: String,
    pub pr_rescues: std::collections::BTreeMap<u64, u32>,
    pub ticket_redesigns: std::collections::BTreeMap<String, u32>,
    pub drain_notice_sprint: u32,
    pub ticket_fail_attempts: std::collections::BTreeMap<String, u32>,
    pub pr_rescue_attempts: std::collections::BTreeMap<String, u32>,
    pub ticket_journal: std::collections::BTreeMap<String, Vec<String>>,
    pub ticket_failures: std::collections::BTreeMap<String, Vec<AttemptFailure>>,
    pub daily_jobs: std::collections::BTreeMap<String, String>,
    pub bug_snapshots: std::collections::BTreeMap<String, BugSnapshot>,
    pub spend: Spend,
    pub spend_today_usd: f64,
    pub spend_day: String,
    pub spend_history: Vec<SpendDay>,
    pub budget_warned_lifetime: bool,
    pub budget_warned_daily: bool,
}

impl Default for WorkShard {
    /// A default Work shard IS a fresh aggregate slice: `schema_version`
    /// starts at [`SCHEMA_VERSION`], not 0 — so `from_shards` over missing
    /// shards (a shard-native store in C019b loading a fresh project)
    /// reassembles a state the stores accept, never a `schema_version: 0`
    /// document. Values mirror [`ProjectState::default`] field for field;
    /// adding a Work field without adding it here fails compilation.
    #[allow(clippy::too_many_lines)] // one line per field, mirroring the struct above
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            alias: String::new(),
            display_name: None,
            current_version: SemVer::default(),
            tickets: Vec::new(),
            activity: Vec::new(),
            sprint: None,
            sprints: Vec::new(),
            sprint_queue: Vec::new(),
            hold_reasons: std::collections::BTreeMap::new(),
            role_health: std::collections::BTreeMap::new(),
            reviews: Vec::new(),
            open_prs: Vec::new(),
            ticket_attachments: std::collections::BTreeMap::new(),
            milestones: Vec::new(),
            cycle_scores: Vec::new(),
            sprint_cycle: 0,
            cycle: 0,
            sprint_goal: String::new(),
            last_digest_day: String::new(),
            last_status_bucket: String::new(),
            last_status_hash: 0,
            last_status_snapshot: StatusDigestSnapshot::default(),
            status_digest_quiet: 0,
            pr_fix_attempts: std::collections::BTreeMap::new(),
            pr_review_skips: std::collections::BTreeMap::new(),
            pr_open_holds: std::collections::BTreeMap::new(),
            pr_sessions: std::collections::BTreeMap::new(),
            ticket_sessions: std::collections::BTreeMap::new(),
            cost_holds: std::collections::BTreeMap::new(),
            clippy_baseline: None,
            debt_signals: Vec::new(),
            sweeps_done: Vec::new(),
            seen_merged_prs: std::collections::BTreeSet::new(),
            ticket_last_merge: std::collections::BTreeMap::new(),
            seen_closed_prs: std::collections::BTreeSet::new(),
            swept_tickets: std::collections::BTreeSet::new(),
            cost_approved: std::collections::BTreeSet::new(),
            tuning: Tuning::default(),
            tuning_overrides: std::collections::BTreeMap::new(),
            tuning_history: Vec::new(),
            ticket_evidence: std::collections::BTreeMap::new(),
            ticket_step_provenance: std::collections::BTreeMap::new(),
            repro_urls: std::collections::BTreeMap::new(),
            queue_recovery: false,
            last_memory_hygiene_day: String::new(),
            last_impediment_day: String::new(),
            pr_rescues: std::collections::BTreeMap::new(),
            ticket_redesigns: std::collections::BTreeMap::new(),
            drain_notice_sprint: 0,
            ticket_fail_attempts: std::collections::BTreeMap::new(),
            pr_rescue_attempts: std::collections::BTreeMap::new(),
            ticket_journal: std::collections::BTreeMap::new(),
            ticket_failures: std::collections::BTreeMap::new(),
            daily_jobs: std::collections::BTreeMap::new(),
            bug_snapshots: std::collections::BTreeMap::new(),
            spend: Spend::default(),
            spend_today_usd: 0.0,
            spend_day: String::new(),
            spend_history: Vec::new(),
            budget_warned_lifetime: false,
            budget_warned_daily: false,
        }
    }
}

/// The Social shard (`state/chat.rs`): human and agent conversation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SocialShard {
    pub comments: Vec<Comment>,
    pub chat: Vec<ChatMsg>,
    pub channels: Vec<Channel>,
    pub questions: Vec<AgentQuestion>,
}

/// The Docs shard (`state/docs.rs`): the wiki and the design system.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DocsShard {
    pub design_system: Option<DesignSystem>,
    pub docs: Vec<DocPage>,
    pub doc_folders: Vec<String>,
    pub doc_refresh: std::collections::BTreeMap<String, DocRefreshMark>,
}

/// The Governance shard (`state/goals.rs`, `governance.rs`, `lessons.rs`,
/// `drift.rs`): goals, decisions, lessons, operator interventions and
/// architecture-drift alerts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GovernanceShard {
    pub goals: Vec<Goal>,
    pub outcome_ledger: Vec<OutcomeLedgerEntry>,
    pub human_holds: std::collections::BTreeMap<u64, String>,
    pub lessons: Vec<String>,
    pub lesson_records: Vec<LessonRecord>,
    pub dismissed_matches: Vec<DismissedMatch>,
    pub lesson_sweep_day: String,
    pub decisions: Vec<String>,
    pub refactor_mode: bool,
    pub approval_samples: Vec<crate::use_cases::approval_memory::ApprovalSample>,
    pub auto_approved_at: std::collections::BTreeMap<String, String>,
    pub ask_again_shapes: Vec<String>,
    pub governance_interventions: Vec<InterventionRecord>,
    pub drift_alerts: Vec<DriftAlert>,
}

/// The Ops shard (`state/ops.rs`): deploys, rollbacks, incidents, queued
/// jobs and the workspace run switch.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpsShard {
    pub history: Vec<DeployRecord>,
    pub reverted_work: Vec<RevertEvent>,
    pub deploy: Option<DeployStatus>,
    pub jobs: Vec<PendingJob>,
    pub engine_incidents: Vec<EngineIncident>,
    pub ops_down: bool,
    pub workspace_run: WorkspaceRun,
    pub ops_down_streak: u32,
    pub deploy_index: u64,
    pub in_rollback: bool,
    pub last_good_deploy: Option<KnownGoodDeploy>,
    pub last_rollback: Option<RollbackStatus>,
    pub incidents: Vec<IncidentRecord>,
    pub rolled_back_commits: std::collections::BTreeSet<String>,
    pub liveness: Option<StallEpisode>,
}

/// The whole aggregate decomposed into its bounded-context shards — a typed
/// map from [`ShardKind`] to that shard's payload. Every `ProjectState`
/// field lands in exactly one payload (enforced by the exhaustive
/// destructures below and unit-tested as an exact partition).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShardedState {
    pub work: WorkShard,
    pub social: SocialShard,
    pub docs: DocsShard,
    pub governance: GovernanceShard,
    pub ops: OpsShard,
}

impl ShardedState {
    /// Remove and return one shard's payload as a [`StateShard`], leaving the
    /// default payload behind. Used by [`ProjectState::shard`].
    #[must_use]
    pub fn take(&mut self, kind: ShardKind) -> StateShard {
        match kind {
            ShardKind::Work => StateShard::new(ShardData::Work(std::mem::take(&mut self.work))),
            ShardKind::Social => {
                StateShard::new(ShardData::Social(std::mem::take(&mut self.social)))
            }
            ShardKind::Docs => StateShard::new(ShardData::Docs(std::mem::take(&mut self.docs))),
            ShardKind::Governance => {
                StateShard::new(ShardData::Governance(std::mem::take(&mut self.governance)))
            }
            ShardKind::Ops => StateShard::new(ShardData::Ops(std::mem::take(&mut self.ops))),
        }
    }
}

/// One shard's payload. The enum keeps the port methods object-safe
/// (`dyn StateStorePort` is live in the presentation layer), and
/// [`ShardData::kind`] is the single source of the kind label.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // payloads mirror the aggregate's own field groups by design; boxing would defeat the plain decompose/compose
pub enum ShardData {
    Work(WorkShard),
    Social(SocialShard),
    Docs(DocsShard),
    Governance(GovernanceShard),
    Ops(OpsShard),
}

impl ShardData {
    /// The shard kind this payload belongs to.
    #[must_use]
    pub fn kind(&self) -> ShardKind {
        match self {
            ShardData::Work(_) => ShardKind::Work,
            ShardData::Social(_) => ShardKind::Social,
            ShardData::Docs(_) => ShardKind::Docs,
            ShardData::Governance(_) => ShardKind::Governance,
            ShardData::Ops(_) => ShardKind::Ops,
        }
    }
}

/// A shard payload plus the kind it belongs to — the unit the shard-scoped
/// port methods ([`crate::ports::outbound::StateStorePort::load_shard`] /
/// `save_shard`) move across the boundary. Build one with
/// [`StateShard::new`], which derives `kind` from the payload, so the label
/// and the payload agree by construction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateShard {
    pub kind: ShardKind,
    pub data: ShardData,
}

impl StateShard {
    /// Wrap a payload, deriving `kind` from it.
    #[must_use]
    pub fn new(data: ShardData) -> Self {
        Self {
            kind: data.kind(),
            data,
        }
    }
}

impl ProjectState {
    /// Decompose the aggregate into its bounded-context shards.
    ///
    /// Pure projection over an already-loaded snapshot. The destructure has
    /// NO `..` wildcard: a future `ProjectState` field that is not named
    /// here fails compilation (missing field in pattern), and a named field
    /// that no shard claims is a denied unused-variable warning — every
    /// field must land in exactly one shard to compile.
    #[must_use]
    #[allow(clippy::too_many_lines)] // the exhaustive destructure IS the anti-drift guard
    pub fn into_shards(self) -> ShardedState {
        let ProjectState {
            schema_version,
            alias,
            display_name,
            current_version,
            tickets,
            history,
            reverted_work,
            activity,
            spend,
            sprint,
            sprints,
            sprint_queue,
            hold_reasons,
            role_health,
            deploy,
            comments,
            reviews,
            open_prs,
            human_holds,
            ticket_attachments,
            chat,
            channels,
            design_system,
            milestones,
            goals,
            outcome_ledger,
            docs,
            doc_folders,
            doc_refresh,
            cycle_scores,
            lessons,
            lesson_records,
            dismissed_matches,
            lesson_sweep_day,
            decisions,
            refactor_mode,
            sprint_cycle,
            cycle,
            sprint_goal,
            last_digest_day,
            last_status_bucket,
            last_status_hash,
            last_status_snapshot,
            status_digest_quiet,
            pr_fix_attempts,
            pr_review_skips,
            pr_open_holds,
            pr_sessions,
            ticket_sessions,
            cost_holds,
            clippy_baseline,
            debt_signals,
            sweeps_done,
            seen_merged_prs,
            ticket_last_merge,
            seen_closed_prs,
            swept_tickets,
            cost_approved,
            tuning,
            tuning_overrides,
            tuning_history,
            ticket_evidence,
            ticket_step_provenance,
            repro_urls,
            queue_recovery,
            last_memory_hygiene_day,
            jobs,
            last_impediment_day,
            pr_rescues,
            ticket_redesigns,
            drain_notice_sprint,
            ticket_fail_attempts,
            pr_rescue_attempts,
            approval_samples,
            auto_approved_at,
            ask_again_shapes,
            ticket_journal,
            ticket_failures,
            questions,
            engine_incidents,
            daily_jobs,
            ops_down,
            workspace_run,
            ops_down_streak,
            spend_today_usd,
            spend_day,
            spend_history,
            budget_warned_lifetime,
            budget_warned_daily,
            deploy_index,
            in_rollback,
            last_good_deploy,
            last_rollback,
            incidents,
            rolled_back_commits,
            bug_snapshots,
            governance_interventions,
            drift_alerts,
            liveness,
        } = self;
        ShardedState {
            work: WorkShard {
                schema_version,
                alias,
                display_name,
                current_version,
                tickets,
                activity,
                sprint,
                sprints,
                sprint_queue,
                hold_reasons,
                role_health,
                reviews,
                open_prs,
                ticket_attachments,
                milestones,
                cycle_scores,
                sprint_cycle,
                cycle,
                sprint_goal,
                last_digest_day,
                last_status_bucket,
                last_status_hash,
                last_status_snapshot,
                status_digest_quiet,
                pr_fix_attempts,
                pr_review_skips,
                pr_open_holds,
                pr_sessions,
                ticket_sessions,
                cost_holds,
                clippy_baseline,
                debt_signals,
                sweeps_done,
                seen_merged_prs,
                ticket_last_merge,
                seen_closed_prs,
                swept_tickets,
                cost_approved,
                tuning,
                tuning_overrides,
                tuning_history,
                ticket_evidence,
                ticket_step_provenance,
                repro_urls,
                queue_recovery,
                last_memory_hygiene_day,
                last_impediment_day,
                pr_rescues,
                ticket_redesigns,
                drain_notice_sprint,
                ticket_fail_attempts,
                pr_rescue_attempts,
                ticket_journal,
                ticket_failures,
                daily_jobs,
                bug_snapshots,
                spend,
                spend_today_usd,
                spend_day,
                spend_history,
                budget_warned_lifetime,
                budget_warned_daily,
            },
            social: SocialShard {
                comments,
                chat,
                channels,
                questions,
            },
            docs: DocsShard {
                design_system,
                docs,
                doc_folders,
                doc_refresh,
            },
            governance: GovernanceShard {
                goals,
                outcome_ledger,
                human_holds,
                lessons,
                lesson_records,
                dismissed_matches,
                lesson_sweep_day,
                decisions,
                refactor_mode,
                approval_samples,
                auto_approved_at,
                ask_again_shapes,
                governance_interventions,
                drift_alerts,
            },
            ops: OpsShard {
                history,
                reverted_work,
                deploy,
                jobs,
                engine_incidents,
                ops_down,
                workspace_run,
                ops_down_streak,
                deploy_index,
                in_rollback,
                last_good_deploy,
                last_rollback,
                incidents,
                rolled_back_commits,
                liveness,
            },
        }
    }

    /// Reassemble the aggregate from its shards. The inverse of
    /// [`ProjectState::into_shards`] — also a pure projection with the same
    /// no-wildcard exhaustive destructures, so the two projections and the
    /// payload structs must stay in lockstep or compilation fails.
    #[must_use]
    #[allow(clippy::too_many_lines)] // the exhaustive destructure IS the anti-drift guard
    pub fn from_shards(shards: ShardedState) -> Self {
        let ShardedState {
            work,
            social,
            docs: docs_shard,
            governance: governance_shard,
            ops: ops_shard,
        } = shards;
        let WorkShard {
            schema_version,
            alias,
            display_name,
            current_version,
            tickets,
            activity,
            sprint,
            sprints,
            sprint_queue,
            hold_reasons,
            role_health,
            reviews,
            open_prs,
            ticket_attachments,
            milestones,
            cycle_scores,
            sprint_cycle,
            cycle,
            sprint_goal,
            last_digest_day,
            last_status_bucket,
            last_status_hash,
            last_status_snapshot,
            status_digest_quiet,
            pr_fix_attempts,
            pr_review_skips,
            pr_open_holds,
            pr_sessions,
            ticket_sessions,
            cost_holds,
            clippy_baseline,
            debt_signals,
            sweeps_done,
            seen_merged_prs,
            ticket_last_merge,
            seen_closed_prs,
            swept_tickets,
            cost_approved,
            tuning,
            tuning_overrides,
            tuning_history,
            ticket_evidence,
            ticket_step_provenance,
            repro_urls,
            queue_recovery,
            last_memory_hygiene_day,
            last_impediment_day,
            pr_rescues,
            ticket_redesigns,
            drain_notice_sprint,
            ticket_fail_attempts,
            pr_rescue_attempts,
            ticket_journal,
            ticket_failures,
            daily_jobs,
            bug_snapshots,
            spend,
            spend_today_usd,
            spend_day,
            spend_history,
            budget_warned_lifetime,
            budget_warned_daily,
        } = work;
        let SocialShard {
            comments,
            chat,
            channels,
            questions,
        } = social;
        let DocsShard {
            design_system,
            docs,
            doc_folders,
            doc_refresh,
        } = docs_shard;
        let GovernanceShard {
            goals,
            outcome_ledger,
            human_holds,
            lessons,
            lesson_records,
            dismissed_matches,
            lesson_sweep_day,
            decisions,
            refactor_mode,
            approval_samples,
            auto_approved_at,
            ask_again_shapes,
            governance_interventions,
            drift_alerts,
        } = governance_shard;
        let OpsShard {
            history,
            reverted_work,
            deploy,
            jobs,
            engine_incidents,
            ops_down,
            workspace_run,
            ops_down_streak,
            deploy_index,
            in_rollback,
            last_good_deploy,
            last_rollback,
            incidents,
            rolled_back_commits,
            liveness,
        } = ops_shard;
        Self {
            schema_version,
            alias,
            display_name,
            current_version,
            tickets,
            history,
            reverted_work,
            activity,
            spend,
            sprint,
            sprints,
            sprint_queue,
            hold_reasons,
            role_health,
            deploy,
            comments,
            reviews,
            open_prs,
            human_holds,
            ticket_attachments,
            chat,
            channels,
            design_system,
            milestones,
            goals,
            outcome_ledger,
            docs,
            doc_folders,
            doc_refresh,
            cycle_scores,
            lessons,
            lesson_records,
            dismissed_matches,
            lesson_sweep_day,
            decisions,
            refactor_mode,
            sprint_cycle,
            cycle,
            sprint_goal,
            last_digest_day,
            last_status_bucket,
            last_status_hash,
            last_status_snapshot,
            status_digest_quiet,
            pr_fix_attempts,
            pr_review_skips,
            pr_open_holds,
            pr_sessions,
            ticket_sessions,
            cost_holds,
            clippy_baseline,
            debt_signals,
            sweeps_done,
            seen_merged_prs,
            ticket_last_merge,
            seen_closed_prs,
            swept_tickets,
            cost_approved,
            tuning,
            tuning_overrides,
            tuning_history,
            ticket_evidence,
            ticket_step_provenance,
            repro_urls,
            queue_recovery,
            last_memory_hygiene_day,
            jobs,
            last_impediment_day,
            pr_rescues,
            ticket_redesigns,
            drain_notice_sprint,
            ticket_fail_attempts,
            pr_rescue_attempts,
            approval_samples,
            auto_approved_at,
            ask_again_shapes,
            ticket_journal,
            ticket_failures,
            questions,
            engine_incidents,
            daily_jobs,
            ops_down,
            workspace_run,
            ops_down_streak,
            spend_today_usd,
            spend_day,
            spend_history,
            budget_warned_lifetime,
            budget_warned_daily,
            deploy_index,
            in_rollback,
            last_good_deploy,
            last_rollback,
            incidents,
            rolled_back_commits,
            bug_snapshots,
            governance_interventions,
            drift_alerts,
            liveness,
        }
    }

    /// Project ONE shard's fields out of the aggregate (the rest are dropped
    /// from the returned payload). Pure; clones the aggregate because the
    /// projection borrows nothing. This is the shape the port's default
    /// `load_shard` serves; adapters with real per-shard storage override it
    /// so the whole-document cost never happens.
    #[must_use]
    pub fn shard(&self, kind: ShardKind) -> StateShard {
        self.clone().into_shards().take(kind)
    }

    /// Merge ONE shard's payload into the aggregate, leaving every other
    /// shard's fields exactly as they are. The payload is authoritative —
    /// `kind` is carried as a label ([`StateShard::new`] derives it from the
    /// payload, so the two agree by construction).
    pub fn with_shard(&mut self, shard: StateShard) {
        let StateShard { kind, data } = shard;
        debug_assert_eq!(kind, data.kind(), "StateShard.kind must label its payload");
        let mut all = std::mem::take(self).into_shards();
        match data {
            ShardData::Work(work) => all.work = work,
            ShardData::Social(social) => all.social = social,
            ShardData::Docs(docs) => all.docs = docs,
            ShardData::Governance(governance) => all.governance = governance,
            ShardData::Ops(ops) => all.ops = ops,
        }
        *self = Self::from_shards(all);
    }
}

#[cfg(test)]
mod shard_tests {
    use super::*;
    use coxagent_domain::{
        Complexity, DebtSignalKind, Goal, GoalId, InterventionKind, Priority, Ticket, TicketId,
        TicketType,
    };

    fn ticket_id(id: &str) -> TicketId {
        TicketId::new(id).expect("ticket id")
    }

    fn feature(id: &str) -> Ticket {
        Ticket::new(
            ticket_id(id),
            TicketType::Feature,
            "shard fixture",
            "populated for the shard round-trip",
            Priority::High,
            Complexity::Small,
            false,
        )
        .expect("ticket")
    }

    /// A `ProjectState` with EVERY field non-default, so the shard
    /// round-trip exercises each field's move, not just the empty shapes.
    /// (`schema_version` alone stays at its persisted value — it is the one
    /// field whose default IS the only legal value.) The length IS the
    /// coverage: one visible assignment per field, in the struct's
    /// declaration order, so a new field missing from this fixture reads as
    /// a gap.
    #[allow(clippy::too_many_lines, clippy::field_reassign_with_default)]
    fn populated() -> ProjectState {
        let mut s = ProjectState::default();
        s.alias = "CXA".to_owned();
        s.display_name = Some("CoXAgent".to_owned());
        s.current_version = SemVer::new(2, 35, 0);
        s.tickets = vec![feature("CXA-F293")];
        s.history = vec![DeployRecord {
            version: SemVer::new(1, 2, 3),
            ticket: ticket_id("CXA-F293"),
            title: "shipped".to_owned(),
            at: "2026-09-06T00:00:00Z".to_owned(),
        }];
        s.reverted_work = vec![RevertEvent {
            sha: "abc123".to_owned(),
            subject: "revert: shipped".to_owned(),
            ticket: "CXA-F293".to_owned(),
            role: "DEV-FEATURE".to_owned(),
            reverted_at: "2026-09-06T00:00:00Z".to_owned(),
            detected_at: "2026-09-06T00:00:00Z".to_owned(),
            decision: RevertDecision::default(),
            decided_at: Some("2026-09-06T01:00:00Z".to_owned()),
            decided_by: Some("operator".to_owned()),
        }];
        s.activity = vec![ActivityEntry {
            at: "2026-09-06T00:00:00Z".to_owned(),
            agent: "SM".to_owned(),
            action: "announced".to_owned(),
            ticket: Some("CXA-F293".to_owned()),
        }];
        s.spend = Spend {
            total_cost_usd: 1.5,
            ..Spend::default()
        };
        s.sprint = Some(Sprint {
            number: 1,
            goal: "shard the state".to_owned(),
            started_cycle: 41,
            length_cycles: 10,
            committed: vec![ticket_id("CXA-F293")],
            started_at: "2026-09-06T00:00:00Z".to_owned(),
            bug_burn_floor: Some(Priority::High),
        });
        s.sprints = vec![SprintRecord {
            number: 1,
            goal: "shard the state".to_owned(),
            committed: 1,
            done: 0,
            at: "2026-09-06T00:00:00Z".to_owned(),
        }];
        s.sprint_queue = vec![PlannedSprint {
            id: 1,
            goal: "physical sharding".to_owned(),
            tickets: vec![ticket_id("CXA-F294")],
            created_at: "2026-09-06T00:00:00Z".to_owned(),
            by: "po".to_owned(),
        }];
        s.hold_reasons
            .insert("CXA-F293".to_owned(), "on hold".to_owned());
        s.role_health
            .insert("DEV".to_owned(), RoleHealth::default());
        s.reviews = vec![PrReview {
            number: 1,
            decision: "approve".to_owned(),
            summary: "clean seam".to_owned(),
            at: "2026-09-06T00:00:00Z".to_owned(),
            head_sha: "abc123".to_owned(),
            latency_secs: Some(5),
        }];
        s.open_prs = vec![crate::ports::outbound::PrOpen {
            number: 1,
            title: "shard seam".to_owned(),
            head: "abc123".to_owned(),
            base: "main".to_owned(),
            url: "https://example.test/pr/1".to_owned(),
            author: "DEV-FEATURE".to_owned(),
            ci: "green".to_owned(),
            mergeable: true,
            created: "2026-09-06T00:00:00Z".to_owned(),
        }];
        s.human_holds.insert(1, "needs human eyes".to_owned());
        s.ticket_attachments.insert(
            "CXA-F293".to_owned(),
            vec![TicketAttachment {
                name: "seam.svg".to_owned(),
                key: "blobs/seam.svg".to_owned(),
                content_type: "image/svg+xml".to_owned(),
                by: "PD".to_owned(),
                at: "2026-09-06T00:00:00Z".to_owned(),
            }],
        );
        s.chat = vec![ChatMsg {
            id: "m1".to_owned(),
            at: "2026-09-06T00:00:00Z".to_owned(),
            user: "operator".to_owned(),
            body: "seam looks right".to_owned(),
            edited: None,
            channel: GENERAL_CHANNEL.to_owned(),
            attachments: Vec::new(),
            reactions: Vec::new(),
            thread_id: None,
            reply_count: 0,
            deleted: false,
        }];
        s.channels = vec![Channel {
            id: "design-review".to_owned(),
            name: "Design Review".to_owned(),
            owner: "operator".to_owned(),
            members: vec!["operator".to_owned()],
            inviters: Vec::new(),
            created_at: "2026-09-06T00:00:00Z".to_owned(),
            kind: "private".to_owned(),
            project: String::new(),
            topic: "seam reviews".to_owned(),
            parent: String::new(),
            open_invite: true,
        }];
        s.design_system = Some(DesignSystem::default());
        s.milestones = vec![Milestone {
            name: "sharded state".to_owned(),
            goal: "bounded-context persistence".to_owned(),
            target_version: "2.36.0".to_owned(),
            goal_complete: true,
            fulfilled: false,
        }];
        s.goals =
            vec![Goal::new(GoalId::new("G001").expect("goal id"), "ship the seam").expect("goal")];
        s.outcome_ledger = vec![OutcomeLedgerEntry {
            ticket: ticket_id("CXA-F293"),
            goal: Some(GoalId::new("G001").expect("goal id")),
            capture_commit: Some("abc123".to_owned()),
            verified_at: "2026-09-06T00:00:00Z".to_owned(),
        }];
        s.docs = vec![DocPage {
            id: "d1".to_owned(),
            folder: "Technical/Architecture".to_owned(),
            category: "technical".to_owned(),
            title: "State shards".to_owned(),
            body: "Five bounded contexts.".to_owned(),
            updated_at: "2026-09-06T00:00:00Z".to_owned(),
            updated_by: "SA".to_owned(),
        }];
        s.doc_folders = vec!["Technical/Architecture".to_owned()];
        s.doc_refresh
            .insert("d1".to_owned(), DocRefreshMark::default());
        s.cycle_scores = vec![CycleScore {
            cycle: 42,
            shipped: 1,
            ..CycleScore::default()
        }];
        s.lessons = vec!["decompose before you shard".to_owned()];
        s.lesson_records = vec![LessonRecord {
            text: "decompose before you shard".to_owned(),
            at: "2026-09-06T00:00:00Z".to_owned(),
            cycle: 42,
            re_recordings: 0,
            recurrences: Vec::new(),
            escalated: None,
            id: Some("seed-1".to_owned()),
            source: Some("shipped".to_owned()),
        }];
        s.dismissed_matches = vec![DismissedMatch {
            lesson: "decompose before you shard".to_owned(),
            incident_at: "2026-09-06T00:00:00Z".to_owned(),
            incident_reason: "deploy failed".to_owned(),
            dismissed_at: "2026-09-06T01:00:00Z".to_owned(),
            dismissed_by: "operator".to_owned(),
        }];
        s.lesson_sweep_day = "2026-09-06".to_owned();
        s.decisions = vec!["shards before columns".to_owned()];
        s.refactor_mode = true;
        s.sprint_cycle = 7;
        s.cycle = 42;
        s.sprint_goal = "shard the state".to_owned();
        s.last_digest_day = "2026-09-06".to_owned();
        s.pr_fix_attempts.insert(1, 1);
        s.pr_review_skips.insert(1, 2);
        s.pr_open_holds.insert(
            1,
            PrOpenHold {
                reason: "settled but diff not on main".to_owned(),
                rounds: 2,
                head_sha: "holdsha".to_owned(),
                first_at: "2026-09-06T00:00:00Z".to_owned(),
                last_at: "2026-09-06T01:00:00Z".to_owned(),
            },
        );
        s.pr_sessions.insert(1, "pr-session".to_owned());
        s.ticket_sessions.insert(
            "CXA-F293/dev_feature".to_owned(),
            "ticket-session".to_owned(),
        );
        s.cost_holds.insert("CXA-F293".to_owned(), 9.5);
        s.clippy_baseline = Some(3);
        s.debt_signals = vec![DebtSignal::new(DebtSignalKind::LintRegression, 3)];
        s.sweeps_done = vec![40];
        s.seen_merged_prs.insert(1);
        s.ticket_last_merge
            .insert("CXA-F293".to_owned(), "2026-09-06T00:00:00Z".to_owned());
        s.seen_closed_prs.insert(2);
        s.swept_tickets.insert("CXA-F293".to_owned());
        s.cost_approved.insert("CXA-F293".to_owned());
        s.tuning = Tuning {
            bugs_first: true,
            ..Tuning::default()
        };
        s.tuning_overrides.insert(
            "bugs_first".to_owned(),
            BrakeHold {
                pinned_value: Some(false),
                reason: "operator override".to_owned(),
                actor: "operator".to_owned(),
                at: "2026-09-06T00:00:00Z".to_owned(),
                expires_at: "2026-09-07T00:00:00Z".to_owned(),
            },
        );
        s.tuning_history = vec![TuningAuditEntry {
            at: "2026-09-06T00:00:00Z".to_owned(),
            actor: "SM".to_owned(),
            source: "self_tune".to_owned(),
            brake: "bugs_first".to_owned(),
            from: false,
            to: true,
            reason: "churn hot".to_owned(),
            until: None,
        }];
        s.ticket_evidence.insert(
            "CXA-F293".to_owned(),
            vec![Evidence {
                kind: "api".to_owned(),
                label: "live request".to_owned(),
                detail: "HTTP 200".to_owned(),
                at: "2026-09-06T00:00:00Z".to_owned(),
                source_gates: vec!["verify".to_owned()],
                actor: "reviewer".to_owned(),
            }],
        );
        s.ticket_step_provenance
            .insert("CXA-F293".to_owned(), vec![StepProvenance::default()]);
        s.repro_urls
            .insert("CXA-F293".to_owned(), "http://127.0.0.1:8101/".to_owned());
        s.queue_recovery = true;
        s.last_memory_hygiene_day = "2026-09-06".to_owned();
        s.jobs = vec![PendingJob {
            id: "j1".to_owned(),
            kind: "force_merge".to_owned(),
            args: serde_json::json!({"pr": 1}),
            queued_at: "2026-09-06T00:00:00Z".to_owned(),
            queued_by: "operator".to_owned(),
        }];
        s.last_impediment_day = "2026-09-06".to_owned();
        s.pr_rescues.insert(1, 1);
        s.ticket_redesigns.insert("CXA-F293".to_owned(), 1);
        s.drain_notice_sprint = 3;
        s.ticket_fail_attempts.insert("CXA-F293".to_owned(), 1);
        s.pr_rescue_attempts.insert("1".to_owned(), 1);
        s.approval_samples = vec![crate::use_cases::approval_memory::ApprovalSample {
            shape: "feature/small".to_owned(),
            decision: "approve".to_owned(),
            by: "operator".to_owned(),
            reason: "trusted shape".to_owned(),
            at: "2026-09-06T00:00:00Z".to_owned(),
        }];
        s.auto_approved_at
            .insert("CXA-F293".to_owned(), "2026-09-06T00:00:00Z".to_owned());
        s.ask_again_shapes = vec!["feature/large".to_owned()];
        s.ticket_journal.insert(
            "CXA-F293".to_owned(),
            vec!["attempt 1: wrote the seam".to_owned()],
        );
        s.ticket_failures.insert(
            "CXA-F293".to_owned(),
            vec![AttemptFailure {
                attempt: 1,
                layer: FailureLayer::Gate,
                gate: "clippy".to_owned(),
                detail: "one lint".to_owned(),
                files: vec!["crates/application/src/state/shards.rs".to_owned()],
            }],
        );
        s.questions = vec![AgentQuestion {
            id: "CXA-F293#1".to_owned(),
            ticket: "CXA-F293".to_owned(),
            from: "DEV-FEATURE".to_owned(),
            to: "SA".to_owned(),
            body: "which shard owns schema_version?".to_owned(),
            answer: "work".to_owned(),
            asked_at: "2026-09-06T00:00:00Z".to_owned(),
            answered_at: "2026-09-06T00:01:00Z".to_owned(),
            forwarded: false,
            escalated: false,
            deferred: false,
        }];
        s.engine_incidents = vec![EngineIncident {
            engine: "claude".to_owned(),
            reason: "rate limited".to_owned(),
            role: "DEV-BUG".to_owned(),
            since: "2026-09-06T00:00:00Z".to_owned(),
            hits: 2,
        }];
        s.daily_jobs
            .insert("standup".to_owned(), "2026-09-06".to_owned());
        s.ops_down = true;
        s.workspace_run = WorkspaceRun {
            running: false,
            by: "operator".to_owned(),
            at: "2026-09-06T00:00:00Z".to_owned(),
            reason: "drain before sharding".to_owned(),
        };
        s.ops_down_streak = 2;
        s.spend_today_usd = 1.25;
        s.spend_day = "2026-09-06".to_owned();
        s.spend_history = vec![SpendDay {
            day: "2026-09-05".to_owned(),
            usd: 4.0,
        }];
        s.budget_warned_lifetime = true;
        s.budget_warned_daily = true;
        s.deploy_index = 9;
        s.in_rollback = true;
        s.last_good_deploy = Some(KnownGoodDeploy {
            sha: "good123".to_owned(),
            at: "2026-09-06T00:00:00Z".to_owned(),
            deploy_index: 9,
            summary: "green".to_owned(),
        });
        s.last_rollback = Some(RollbackStatus {
            at: "2026-09-06T00:00:00Z".to_owned(),
            reason: "deploy failed".to_owned(),
            to_sha: "good123".to_owned(),
            ok: true,
            summary: "rolled back".to_owned(),
            stale: false,
            migration_blocked: false,
            failure_bundle: None,
        });
        s.incidents = vec![IncidentRecord {
            at: "2026-09-06T00:00:00Z".to_owned(),
            reason: "deploy failed".to_owned(),
            failed_sha: "bad456".to_owned(),
            to_sha: "good123".to_owned(),
            ok: true,
            summary: "health probe failed".to_owned(),
            root_cause_ticket: Some("CXA-B001".to_owned()),
            lesson: Some("pin the probe".to_owned()),
        }];
        s.rolled_back_commits.insert("bad456".to_owned());
        s.bug_snapshots.insert(
            "2026-09-06".to_owned(),
            BugSnapshot {
                open: 1,
                fixed: 2,
                verified: 3,
            },
        );
        s.governance_interventions = vec![InterventionRecord {
            kind: InterventionKind::ReadyApprove,
            ticket: "CXA-F293".to_owned(),
            area: Some(TicketType::Feature),
            by: "operator".to_owned(),
            at: "2026-09-06T00:00:00Z".to_owned(),
        }];
        s.drift_alerts = vec![DriftAlert {
            area: "server".to_owned(),
            message: "handler grew business logic".to_owned(),
            ticket: "CXA-B002".to_owned(),
            at: "2026-09-06T00:00:00Z".to_owned(),
        }];
        s.liveness = Some(StallEpisode {
            since: "2026-09-06T00:00:00Z".to_owned(),
            worker: "operator@host".to_owned(),
            last_activity_at: "2026-09-06T00:00:00Z".to_owned(),
            last_alert_at: "2026-09-06T00:05:00Z".to_owned(),
            escalations: 1,
        });
        s
    }

    #[test]
    fn the_empty_shard_map_reassembles_to_a_fresh_aggregate() {
        // Identity elements of the decompose/compose pair, both directions.
        // The schema_version half is the guard that matters for C019b: a
        // shard-native store reassembling MISSING shards gets a fresh
        // aggregate (schema_version at SCHEMA_VERSION), never a
        // schema_version: 0 document the stores would refuse.
        assert_eq!(
            ProjectState::default().into_shards(),
            ShardedState::default()
        );
        assert_eq!(
            ProjectState::from_shards(ShardedState::default()),
            ProjectState::default()
        );
    }

    #[test]
    fn default_state_round_trips_through_the_shards() {
        let state = ProjectState::default();
        assert_eq!(
            ProjectState::from_shards(state.clone().into_shards()),
            state
        );
    }

    #[test]
    fn populated_state_round_trips_through_the_shards() {
        let state = populated();
        assert_eq!(
            ProjectState::from_shards(state.clone().into_shards()),
            state
        );
    }

    #[test]
    fn recomposed_state_serializes_byte_identically_to_the_original() {
        // Wire identity: the decomposition exists only in memory, so the
        // persisted document before and after a decompose->compose cycle
        // must be the exact same JSON.
        let state = populated();
        let before = serde_json::to_value(&state).expect("serialize");
        let after = serde_json::to_value(ProjectState::from_shards(state.into_shards()))
            .expect("serialize");
        assert_eq!(after, before);
    }

    #[test]
    fn shard_payloads_partition_the_serialized_document() {
        // Disjointness: no field may be serialized into two shards; coverage:
        // together the shards must serialize to exactly the full document's
        // key set. A field claimed by two shards or by neither fails here.
        let state = populated();
        let full_keys: std::collections::BTreeSet<String> = serde_json::to_value(&state)
            .expect("serialize full state")
            .as_object()
            .expect("object")
            .keys()
            .cloned()
            .collect();
        let shards = state.into_shards();
        let payloads = [
            serde_json::to_value(&shards.work).expect("work"),
            serde_json::to_value(&shards.social).expect("social"),
            serde_json::to_value(&shards.docs).expect("docs"),
            serde_json::to_value(&shards.governance).expect("governance"),
            serde_json::to_value(&shards.ops).expect("ops"),
        ];
        let mut seen = std::collections::BTreeSet::new();
        for payload in payloads {
            let object = payload.as_object().expect("shard payload object");
            for key in object.keys() {
                assert!(
                    seen.insert(key.clone()),
                    "field {key} serialized into two shards"
                );
            }
        }
        assert_eq!(
            seen, full_keys,
            "shard payloads must exactly partition the serialized document"
        );
    }

    #[test]
    fn shard_data_survives_its_own_serde_round_trip() {
        let state = populated();
        let mut shards = state.into_shards();
        for kind in ShardKind::ALL {
            let shard = shards.take(kind);
            let json = serde_json::to_value(&shard).expect("serialize shard");
            let back: StateShard = serde_json::from_value(json).expect("deserialize shard");
            assert_eq!(back, shard, "shard {kind:?} must survive serde losslessly");
            assert_eq!(back.kind, kind);
            assert_eq!(back.data.kind(), kind);
        }
    }

    #[test]
    fn merging_one_shard_into_a_default_state_leaves_other_shards_untouched() {
        let source = populated();
        for kind in ShardKind::ALL {
            let mut merged = ProjectState::default();
            merged.with_shard(source.shard(kind));
            for other in ShardKind::ALL {
                let expected = if other == kind {
                    source.shard(other)
                } else {
                    ProjectState::default().shard(other)
                };
                assert_eq!(
                    merged.shard(other),
                    expected,
                    "shard {other:?} after merging {kind:?}"
                );
            }
        }
    }

    #[test]
    fn take_leaves_the_other_payloads_intact() {
        let mut shards = populated().into_shards();
        let before = shards.clone();
        let _ = shards.take(ShardKind::Docs);
        assert_eq!(shards.work, before.work);
        assert_eq!(shards.social, before.social);
        assert_eq!(shards.governance, before.governance);
        assert_eq!(shards.ops, before.ops);
        assert_eq!(
            shards.docs,
            DocsShard::default(),
            "taken shard is replaced by default"
        );
    }
}
