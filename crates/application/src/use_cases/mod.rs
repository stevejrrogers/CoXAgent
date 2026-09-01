//! Use cases — application services orchestrating domain + ports.

pub mod add_ticket;
pub mod analyze_attachment;
pub mod approval_memory;
pub mod approval_risk;
pub mod backup;
pub mod ceremony;
pub mod conformance_check;
pub mod coverage;
pub mod cycle;
pub mod generate_docs;
pub mod global_search;
pub mod json_repair;
pub mod manual_rollback;
pub mod merge_policy;
pub mod merge_sweep;
pub mod question_batching;
pub mod readiness_preflight;
pub mod recover;
pub mod refine_ticket;
pub mod release_assembly;
pub mod repro_url;
pub mod restore;
pub mod run_ba;
pub mod run_chat_reply;
pub mod run_design_system;
pub mod run_dev;
pub mod run_discussion;
pub mod run_docs;
pub mod run_grooming;
pub mod run_milestones;
pub mod run_pd;
pub mod run_planning;
pub mod run_releases;
pub mod run_reviews;
pub mod run_sa;
pub mod run_standup;
pub mod run_test;
pub mod runner;
pub mod scan_deps;

pub use add_ticket::{AddTicketInput, AddTicketUseCase};
pub use analyze_attachment::{AnalyzeAttachmentUseCase, ReadableAttachment};
pub use backup::{
    is_excluded, path_stamp, rel_from, secret_statement, BackupOutcome, BackupRequest,
    BackupWorkspaceUseCase,
};
pub use conformance_check::RunConformanceUseCase;
pub use coverage::{match_score, matches_criterion, record_verdicts};
pub use cycle::{CycleReport, RunCycleUseCase};
pub use generate_docs::GenerateDocsUseCase;
pub use global_search::{
    global_search, global_search_many, GroupedSearch, SearchGroup, SearchHit, SearchKind,
    MAX_PER_KIND, MAX_QUERY_LEN, MAX_TOTAL, MIN_QUERY_LEN,
};
pub use json_repair::repair_json;
pub use manual_rollback::{known_good_target, ManualRollbackInput, RollbackOutcome};
pub use merge_policy::{
    competing_pr, escalation_route, needs_human_eyes, route_from_failures, EscalationRoute,
    MAX_TICKET_RESCUES,
};
pub use merge_sweep::{merge_sweep, SweepOutcome};
pub use question_batching::{flush_batches, select_deferred, should_defer};
pub use readiness_preflight::{
    run_preflight, PreflightItem, PreflightProbe, PreflightReport, PreflightSnapshot,
};
pub use recover::RecoverUseCase;
pub use refine_ticket::{FeasLane, Feasibility, RefineTicketUseCase, RefinedTicket, TeamNote};
pub use release_assembly::{
    assemble, cut_manifest, extract_ticket_refs, filter_verified_subjects, is_verified_complete,
    rc_members, verified_complete_ids, BlockedCandidate, CutManifest, RcAssembly, RcBundle,
    SubjectManifest,
};
pub use repro_url::{
    resolve_repro_url, ReproSource, ReproUrl, ReproUrlSnapshot, ResolveReproUrlUseCase,
};
pub use restore::{RestoreOutcome, RestoreRequest, RestoreWorkspaceUseCase};
pub use run_ba::RunBaUseCase;
pub use run_chat_reply::RunChatReplyUseCase;
pub use run_design_system::RunDesignSystemUseCase;
pub use run_dev::{DevMode, RunDevUseCase};
pub use run_discussion::{DiscussionOutcome, RunDiscussionUseCase};
pub use run_docs::RunDocsUseCase;
pub use run_grooming::RunGroomingUseCase;
pub use run_milestones::RunMilestonesUseCase;
pub use run_pd::RunPdUseCase;
pub use run_planning::RunPlanningUseCase;
pub use run_releases::RunReleasesUseCase;
pub use run_reviews::{RunArchitectureAuditUseCase, RunDocsAuditUseCase};
pub use run_sa::RunSaUseCase;
pub use run_standup::RunStandupUseCase;
pub use run_test::RunTestUseCase;
pub use runner::{run_forever, RunnerHandle, RunnerSnapshot};
pub use scan_deps::{FindingReport, ScanDependenciesUseCase, ScanDepsInput, ScanDepsOutcome};

#[cfg(test)]
mod mod_guard_tests;
#[cfg(test)]
mod release_assembly_tests;
#[cfg(test)]
mod run_releases_tdd_tests;
#[cfg(test)]
mod run_releases_tests;
#[cfg(test)]
mod run_test_tdd_tests;
