# Repo map — 118 files, 2721 symbols
Query it (if `coxagent` is on PATH): `coxagent codegraph search|impact|callers <name>`.
Languages: javascript 4, rust 113, swift 1

## crates/app/src/bin/service.rs (rust)
  fn main
## crates/app/src/lib.rs (rust)
  fn append_registry, fn assign_host_port, fn build_audit, fn build_auth, fn build_doc_store, fn build_engine, fn build_failover, fn build_notifier, fn build_project, fn build_storage, fn build_syschat_store, type BuiltEngine
## crates/app/src/main.rs (rust)
  fn main
## crates/app/src/onboard.rs (rust)
  fn brownfield, fn build_repo_map, fn comprehension_context, fn context_template, fn detect_stack, fn ensure_git_repo, fn greenfield, fn has_compose, const MANIFESTS
## crates/app/src/shutdown.rs (rust)
  fn Shutdown::is_triggered, fn Shutdown::listen, struct Shutdown, fn Shutdown::sleep_or_shutdown, fn spawn_listener, fn spawn_listener
## crates/application/src/auth.rs (rust)
  fn AuthRole::all, fn AuthRole::as_str, fn attach_device, trait AuthPort, enum AuthRole, struct AuthUser, fn AuthRole::can_create_channel, fn AuthRole::can_manage, fn AuthRole::can_review, fn AuthRole::can_write, fn AuthRole::from_str_lenient, fn AuthRole::is_lead
## crates/application/src/codegraph.rs (rust)
  fn c_cpp_ruby_php_symbols_and_calls, struct Call, fn call_graph_links_callers_and_callees, fn CodeGraph::callees, fn CodeGraph::callers, struct CodeGraph, fn CodeGraph::dependents, fn extract_import, fn extract_symbol, struct FileNode, fn CodeGraph::index, fn index_walks_and_maps
## crates/application/src/config.rs (rust)
  fn EngineKind::all, fn EngineKind::as_binary, struct BudgetCaps, struct Config, fn config_round_trips_through_json, fn WorkflowConfig::default, fn GitConfig::default, fn Config::default, fn default_branch, fn default_branch_prefix, fn default_concurrency, fn default_max_open_prs
## crates/application/src/conformance.rs (rust)
  fn Violation::bug_title, fn check, fn conformant_rust_server_passes, fn flags_forbidden_extension_and_missing_marker, fn list_files, fn missing_area_is_skipped, const SKIP_DIRS, struct StackRule, struct Violation, fn walk, fn write
## crates/application/src/error.rs (rust)
  enum AppError, enum PortError
## crates/application/src/lib.rs (rust)
## crates/application/src/metrics.rs (rust)
  fn compute, fn counts_shipped_features_and_open_bugs, struct DayCount, fn digest_markdown, fn empty_state_is_all_zeroes, fn is_feature_id, struct Metrics, fn status_key, fn ticket, fn ty_key
## crates/application/src/parsing.rs (rust)
  fn empty_array_is_ok, fn jaccard, fn jaccard_flags_paraphrased_titles, fn no_array_errors, fn normalize_title, fn parse_items, fn parse_string_list, fn parses_array_amid_prose, struct ProposedItem, const STOP, fn title_tokens, fn title_tokens_drops_stopwords
## crates/application/src/policy.rs (rust)
  fn allowlist_gates_models, fn daily_budget_trips_at_cap, fn empty_allowlist_permits_all, fn forbidden_hits, fn forbidden_paths_flag_prefix_matches, fn model_allowed, fn over_daily_budget, fn policy
## crates/application/src/ports/inbound/mod.rs (rust)
## crates/application/src/ports/mod.rs (rust)
## crates/application/src/ports/outbound/audit.rs (rust)
  trait AuditPort, struct AuditRecord
## crates/application/src/ports/outbound/deploy.rs (rust)
  trait DeployPort, struct DeployReport, fn down, fn ensure_daemon, fn health, fn run_tests
## crates/application/src/ports/outbound/doc_store.rs (rust)
  trait DocStorePort
## crates/application/src/ports/outbound/engine.rs (rust)
  trait AgentEnginePort, struct AgentOutcome, struct AgentRequest, fn AgentOutcome::succeeded, struct Usage
## crates/application/src/ports/outbound/forge.rs (rust)
  fn comment_pr, trait ForgePort, fn pr_feedback, struct PrFeedback, struct PullRequest
## crates/application/src/ports/outbound/git.rs (rust)
  struct GitAuthor, trait GitPort, enum SyncBase
## crates/application/src/ports/outbound/kv_doc.rs (rust)
  trait KvDocPort
## crates/application/src/ports/outbound/mod.rs (rust)
## crates/application/src/ports/outbound/notify.rs (rust)
  struct ChatNotifier, struct FanoutNotifier, fn kind_icon, fn ChatNotifier<S>::new, trait NotifierPort, fn NullNotifier::notify, fn ChatNotifier<S>::notify, fn FanoutNotifier::notify, struct NotifyEvent, struct NullNotifier
## crates/application/src/ports/outbound/state_store.rs (rust)
  fn acquire_leader, fn acquire_operator, fn claim_stage, fn claim_ticket, fn get_desired, fn heartbeat_worker, fn mutate_state, fn release_stage, fn set_desired, trait StateStorePort, struct WorkerEntry, fn workers
## crates/application/src/ports/outbound/storage.rs (rust)
  trait StoragePort
## crates/application/src/prompts.rs (rust)
  const BA, const BASE, fn deploy_constraint_names_the_assigned_port, fn deploy_constraints, fn design_constraints, const DESIGN_SYSTEM, const DEV, const DOCS, fn empty_design_system_renders_nothing, const ENGINEERING_STANDARDS, fn focus_block, const PD
## crates/application/src/selection.rs (rust)
  fn candidates, fn deps_satisfied, fn design_candidates, fn documentable_candidates, fn next_documentable, fn next_feature_needing_design, fn next_feature_needing_ux, fn next_open_bug, fn next_ready_feature, fn no_ready_feature_returns_none, fn open_bug_candidates, fn picks_highest_priority_ready_feature
## crates/application/src/sprint.rs (rust)
  fn advance, fn does_not_reopen_mid_sprint, fn done_count, fn feature, fn goal_from, fn open_backlog, fn opens_first_sprint_and_commits_backlog, fn rolls_over_after_length
## crates/application/src/state.rs (rust)
  struct ActivityEntry, fn acyclic_graph_validates, fn ProjectState::add_daily_spend, fn ProjectState::add_decision, fn ProjectState::add_doc_folder, fn ProjectState::add_lesson, const AGENTS_CHANNEL, fn agents_channel_record, struct Attachment, fn Channel::can_invite, fn Channel::can_view, fn chan_kind_private
## crates/application/src/system_chat.rs (rust)
  fn SystemChat::can_view, fn SystemChat::channels_for, fn channels_for_lists_general_and_projects, struct ChatContext, fn SystemChat::create_channel, fn SystemChat::create_webhook, fn ctx, fn SystemChat::delegate, fn dms_are_participants_only_even_for_admins, fn SystemChat::general, fn general_visible_to_all_project_channel_scoped, fn SystemChat::get_topic
## crates/application/src/tokens.rs (rust)
  fn clip_middle, fn clip_middle_noop_when_small, fn clip_middle_preserves_head_and_tail, fn compress, fn compress_diff, fn compress_diff_drops_index_lines_and_dedupes, fn dedupe_collapses_repeats, fn dedupe_keeps_distinct_lines, fn dedupe_lines, fn proxy_compress, fn proxy_compress_passes_small_and_shrinks_large, const TERSE
## crates/application/src/ts.rs (rust)
  fn c_fn_name, fn call_target, fn calls, fn collect_calls, fn collect_refs, fn collect_symbols, fn descend_name, fn fn_identity, const IDENT_KINDS, fn language_for, fn last_ident, fn name_field
## crates/application/src/use_cases/add_ticket.rs (rust)
  struct AddTicketInput, struct AddTicketUseCase, fn AddTicketUseCase<S>::execute, fn input, fn MemStore::load, struct MemStore, fn mint_id, fn mints_sequential_ids_per_type, fn AddTicketUseCase<S>::new, fn poison, fn rejects_blank_title, fn MemStore::save
## crates/application/src/use_cases/analyze_attachment.rs (rust)
  struct AnalyzeAttachmentUseCase, fn compose_task, fn detects_questions, fn AnalyzeAttachmentUseCase<S, E>::execute, fn ReadableAttachment::is_image, fn looks_like_question, fn AnalyzeAttachmentUseCase<S, E>::new, fn plain_captions_are_not_questions, fn AnalyzeAttachmentUseCase<S, E>::post, const QUESTION_LEADS, struct ReadableAttachment, const SYSTEM_PROMPT
## crates/application/src/use_cases/ceremony.rs (rust)
  fn allowed, fn canonical_speaker, struct CeremonyTurn, fn drops_unknown_speakers_and_empty, fn folds_wrapped_continuation_lines, fn parse_transcript, fn parses_multi_speaker_transcript, fn run_transcript, fn run_transcript_raw, fn tolerates_bullet_prefixed_speaker
## crates/application/src/use_cases/conformance_check.rs (rust)
  fn RunConformanceUseCase<S>::execute, fn files_bug_for_typescript_server_drift, fn MemStore::load, struct MemStore, fn RunConformanceUseCase<S>::new, fn no_rules_is_noop, struct RunConformanceUseCase, fn rust_server_rule, fn MemStore::save
## crates/application/src/use_cases/cycle.rs (rust)
  fn SpyGit::abort_merge, fn RunCycleUseCase<S, E>::address_pr_feedback, fn RunCycleUseCase<S, E>::advance_sprint_if_scrum, fn RunCycleUseCase<S, E>::announce_drain_hold, const ARCH_REVIEW_EVERY_SPRINTS, fn RunCycleUseCase<S, E>::architecture_audit, fn auto_merge_only_touches_prs_into_the_target_branch, fn RunCycleUseCase<S, E>::ba, fn RunCycleUseCase<S, E>::capture_retro_lesson, fn SpyGit::checkout_branch, fn RunCycleUseCase<S, E>::clarify_next_feature, fn RunCycleUseCase<S, E>::clean_base_required
## crates/application/src/use_cases/generate_docs.rs (rust)
  fn cat_from_folder, fn GenerateDocsUseCase<S, E>::execute, struct GenerateDocsUseCase, struct GenPage, fn GenerateDocsUseCase<S, E>::new, fn parse_pages, const PROMPT_PRODUCT, const PROMPT_QA, const PROMPT_TECH, fn GenerateDocsUseCase<S, E>::revise, const RULES, fn GenerateDocsUseCase<S, E>::section
## crates/application/src/use_cases/json_repair.rs (rust)
  fn repair_json
## crates/application/src/use_cases/merge_sweep.rs (rust)
  fn merge_sweep, struct SweepOutcome
## crates/application/src/use_cases/mod.rs (rust)
## crates/application/src/use_cases/recover.rs (rust)
  fn RecoverUseCase<S>::execute, fn in_progress_feature, fn MemStore::load, struct MemStore, fn RecoverUseCase<S>::new, fn nothing_to_recover_is_noop, struct RecoverUseCase, fn releases_orphaned_claims_back_to_ready, fn MemStore::save
## crates/application/src/use_cases/refine_ticket.rs (rust)
  fn RefineTicketUseCase<S, E>::advise, fn blank_if_empty, fn RefineTicketUseCase<S, E>::execute, fn first_line, fn RefineTicketUseCase<S, E>::new, fn normalise, fn parse_ticket, const PD_GUIDE, const PO_GUIDE, struct RefinedTicket, struct RefineTicketUseCase, const SA_GUIDE
## crates/application/src/use_cases/run_ba.rs (rust)
  fn ba_adds_proposed_features_to_backlog, fn ba_errors_on_unparseable_output, fn ba_errors_when_engine_fails, struct CannedEngine, fn RunBaUseCase<S, E>::execute, fn CannedEngine::id, fn MemStore::load, struct MemStore, fn RunBaUseCase<S, E>::new, fn CannedEngine::run, fn run, struct RunBaUseCase
## crates/application/src/use_cases/run_chat_reply.rs (rust)
  const COMPOSE_NAMES, fn RunChatReplyUseCase<S, E>::context, fn RunChatReplyUseCase<S, E>::deploy_now, fn RunChatReplyUseCase<S, E>::dispatch, fn RunChatReplyUseCase<S, E>::execute, fn RunChatReplyUseCase<S, E>::file_ticket, fn RunChatReplyUseCase<S, E>::new, fn parse_priority, fn RunChatReplyUseCase<S, E>::post, fn RunChatReplyUseCase<S, E>::reprioritize, fn route_persona, fn RunChatReplyUseCase<S, E>::run
## crates/application/src/use_cases/run_design_system.rs (rust)
  fn authors_once_when_ui_work_exists, fn RunDesignSystemUseCase<S, E>::build_request, struct Canned, struct DesignSystemOutput, fn RunDesignSystemUseCase<S, E>::execute, fn Canned::id, fn MemStore::load, struct MemStore, fn RunDesignSystemUseCase<S, E>::new, const OUT, fn parse, fn Canned::run
## crates/application/src/use_cases/run_dev.rs (rust)
  fn RunDevUseCase<S, E>::build_request, fn DevMode::bump, fn RunDevUseCase<S, E>::candidates, fn DevMode::complete_status, enum DevMode, fn RunDevUseCase<S, E>::execute, fn feature_dev_completes_and_bumps_minor, fn OkEngine::id, fn MemStore::load, struct MemStore, fn RunDevUseCase<S, E>::new, fn now_rfc3339
## crates/application/src/use_cases/run_discussion.rs (rust)
  struct DecidedAction, struct DiscussionOutcome, fn RunDiscussionUseCase<S, E>::execute, fn RunDiscussionUseCase<S, E>::new, fn no_marker_keeps_all_prose, fn parses_decision_with_action, fn parses_decision_without_action, fn RunDiscussionUseCase<S, E>::post, struct RunDiscussionUseCase, fn split_action, fn strip_action, fn RunDiscussionUseCase<S, E>::with_language
## crates/application/src/use_cases/run_docs.rs (rust)
  fn build_docs_prompt, fn documents_done_feature, fn done_feature, fn RunDocsUseCase<S, E>::execute, fn OkEngine::id, fn MemStore::load, struct MemStore, fn RunDocsUseCase<S, E>::new, fn nothing_to_document_is_none, struct OkEngine, fn parse_folder_hint, fn parses_folder_hint_and_body
## crates/application/src/use_cases/run_grooming.rs (rust)
  fn RunGroomingUseCase<S, E>::backlog_context, fn RunGroomingUseCase<S, E>::execute, const GROOM_LIMIT, fn RunGroomingUseCase<S, E>::new, fn RunGroomingUseCase<S, E>::post, struct RunGroomingUseCase, const VOICES, fn RunGroomingUseCase<S, E>::with_language
## crates/application/src/use_cases/run_milestones.rs (rust)
  fn RunMilestonesUseCase<S, E>::execute, struct MilestoneOut, fn RunMilestonesUseCase<S, E>::new, fn parse, fn prompt_system, struct RunMilestonesUseCase
## crates/application/src/use_cases/run_pd.rs (rust)
  fn authors_ux_and_readies_ui_feature, fn RunPdUseCase<S, E>::build_request, struct Canned, fn RunPdUseCase<S, E>::execute, fn Canned::id, fn MemStore::load, struct MemStore, fn RunPdUseCase<S, E>::new, fn nothing_needing_ux_returns_none, fn parse_ux, fn Canned::run, struct RunPdUseCase
## crates/application/src/use_cases/run_planning.rs (rust)
  fn RunPlanningUseCase<S, E>::execute, fn RunPlanningUseCase<S, E>::new, fn RunPlanningUseCase<S, E>::plan_context, fn RunPlanningUseCase<S, E>::post, struct RunPlanningUseCase, const VOICES, fn RunPlanningUseCase<S, E>::with_language, fn RunPlanningUseCase<S, E>::with_repo_note
## crates/application/src/use_cases/run_reviews.rs (rust)
  fn RunArchitectureAuditUseCase<S, E>::bank_verdict, fn RunArchitectureAuditUseCase<S, E>::call_refactor_sprint, fn RunArchitectureAuditUseCase<S, E>::execute, fn RunDocsAuditUseCase<S, E>::execute, fn RunArchitectureAuditUseCase<S, E>::file_refactors, fn gather_evidence, const MANIFESTS, fn RunArchitectureAuditUseCase<S, E>::new, fn RunDocsAuditUseCase<S, E>::new, fn parse_object, fn RunArchitectureAuditUseCase<S, E>::post, fn RunDocsAuditUseCase<S, E>::post
## crates/application/src/use_cases/run_sa.rs (rust)
  fn RunSaUseCase<S, E>::build_request, struct Canned, struct DesignOutput, fn designs_non_ui_feature_to_ready, fn RunSaUseCase<S, E>::execute, fn Canned::id, fn MemStore::load, struct MemStore, fn RunSaUseCase<S, E>::new, fn no_pending_feature_returns_none, fn parse_design, fn Canned::run
## crates/application/src/use_cases/run_standup.rs (rust)
  fn RunStandupUseCase<S, E>::active_roles, fn RunStandupUseCase<S, E>::execute, fn RunStandupUseCase<S, E>::headline, fn RunStandupUseCase<S, E>::new, const PARTICIPANTS, fn RunStandupUseCase<S, E>::post, struct RunStandupUseCase, fn RunStandupUseCase<S, E>::status_context, fn RunStandupUseCase<S, E>::with_language, fn RunStandupUseCase<S, E>::with_operator
## crates/application/src/use_cases/run_test.rs (rust)
  struct Canned, fn dedupes_existing_bug_titles, fn empty_array_files_nothing, fn RunTestUseCase<S, E>::execute, fn files_new_bugs_as_bug_tickets, fn Canned::id, fn MemStore::load, struct MemStore, fn RunTestUseCase<S, E>::new, fn Canned::run, struct RunTestUseCase, fn MemStore::save
## crates/application/src/use_cases/runner.rs (rust)
  fn RunnerHandle::clear_active, fn RunnerHandle::default, fn RunnerHandle::new, fn RunnerHandle::pause, const PAUSED, type PhaseReporter, fn RunnerHandle::resume, fn run_forever, struct RunnerHandle, struct RunnerSnapshot, const RUNNING, fn RunnerHandle::set_active
## crates/contracts/src/lib.rs (rust)
  struct BusEnvelope, const CONTRACT_VERSION, struct JobSpec, fn BusEnvelope::new
## crates/domain/src/error.rs (rust)
  enum DomainError
## crates/domain/src/events.rs (rust)
  enum DesignPart, struct DomainEvent, enum EventKind, fn DomainEvent::new
## crates/domain/src/ids.rs (rust)
  fn TicketId::as_str, fn WorkerId::as_str, fn TicketId::fmt, fn WorkerId::fmt, fn TicketId::new, fn WorkerId::new, struct TicketId, struct WorkerId
## crates/domain/src/lib.rs (rust)
## crates/domain/src/ticket.rs (rust)
  fn Ticket::acceptance_criteria, fn Ticket::add_dependency, fn Ticket::check_ready, fn Ticket::claim, fn claim_stamps_owner_and_rejects_second_claimer, fn Ticket::claimed_at, fn Ticket::claimed_by, fn completing_clears_the_claim, enum Complexity, fn Ticket::depends_on, fn Ticket::description, struct Design
## crates/domain/src/transitions.rs (rust)
  fn bug_can_reopen_but_not_document, fn can_transition, fn dev_cannot_touch_priority, fn feature_cannot_skip_to_done, fn feature_happy_path_edges_are_legal, fn field_permitted, fn only_sa_pd_open_the_design_gate, fn system_is_omnipotent_over_legal_edges, fn transition_allowed
## crates/domain/src/version.rs (rust)
  enum Bump, fn bump_major_resets_minor_and_patch, fn bump_minor_resets_patch, fn bump_patch_increments_last, fn SemVer::bumped, fn SemVer::default, fn SemVer::fmt, fn SemVer::new, fn SemVer::parse, fn parse_rejects_garbage, struct SemVer
## crates/infrastructure/src/audit_sink.rs (rust)
  fn SqlAuditSink::connect, fn MemoryAuditSink::default, const MEM_CAP, fn memory_sink_bounds_and_orders_newest_first, struct MemoryAuditSink, fn SqlAuditSink::migrate, fn SqlAuditSink::prune, const PRUNE_EVERY, fn MemoryAuditSink::recent, fn SqlAuditSink::recent, fn MemoryAuditSink::record, fn SqlAuditSink::record
## crates/infrastructure/src/auth.rs (rust)
  fn api_tokens_mint_authenticate_and_revoke, fn FileAuthService::assign_project, fn FileAuthService::attach_device, struct Attempts, fn FileAuthService::bootstrap_admin, fn bootstrap_then_login_and_reject, fn FileAuthService::clear_failures, fn FileAuthService::create_token, fn FileAuthService::create_user, fn ct_eq, fn FileAuthService::default_path, fn FileAuthService::delete_user
## crates/infrastructure/src/deploy/docker_compose.rs (rust)
  const COMPOSE_FILES, fn compose_project_name, fn compose_project_on_port, fn container_on_port, fn daemon_up, fn DockerComposeDeploy::deploy, const DEPLOY_TIMEOUT, struct DockerComposeDeploy, fn DockerComposeDeploy::down, fn DockerComposeDeploy::ensure_daemon, fn extract_bind_port, fn DockerComposeDeploy::health
## crates/infrastructure/src/deploy/mod.rs (rust)
## crates/infrastructure/src/docs_store.rs (rust)
  fn MongoDocStore::delete, fn from_doc, fn MongoDocStore::from_env, fn MongoDocStore::get, fn MongoDocStore::list, struct MongoDocStore, fn to_doc, fn MongoDocStore::upsert
## crates/infrastructure/src/engine/any.rs (rust)
  enum AnyEngine, fn AnyEngine::from_choice, fn AnyEngine::id, fn AnyEngine::run
## crates/infrastructure/src/engine/claude.rs (rust)
  fn append_live, struct ClaudeEngine, fn ClaudeEngine::id, fn live_path, fn ClaudeEngine::new, fn parse_json_output, fn parse_stream, fn parse_usage, fn render_event, fn ClaudeEngine::run, fn ClaudeEngine::with_binary
## crates/infrastructure/src/engine/failover.rs (rust)
  const ALL_EXHAUSTED, fn detects_quota_walls, fn detects_transient_stalls, struct FailoverEngine, fn FailoverEngine<E>::id, fn is_quota_wall, fn is_transient, fn FailoverEngine<E>::new, fn FailoverEngine<E>::run
## crates/infrastructure/src/engine/hermes.rs (rust)
  struct HermesEngine, fn HermesEngine::id, fn HermesEngine::new, fn HermesEngine::run
## crates/infrastructure/src/engine/metering.rs (rust)
  fn MeteringEngine<E>::id, fn Priced::id, type Meter, struct MeteringEngine, fn meters_cost_and_tokens_per_role, fn MeteringEngine<E>::new, struct Priced, fn req, fn role_key, fn MeteringEngine<E>::run, fn Priced::run
## crates/infrastructure/src/engine/mock.rs (rust)
  fn MockEngine::call_count, fn MockEngine::id, struct MockEngine, fn MockEngine::run, fn MockEngine::with_stdouts
## crates/infrastructure/src/engine/mod.rs (rust)
  fn apply_shim_path, fn role_key
## crates/infrastructure/src/engine/opencode.rs (rust)
  fn estimate_tokens_raw, fn OpencodeEngine::id, fn OpencodeEngine::new, struct OpencodeEngine, fn OpencodeEngine::run, fn OpencodeEngine::with_binary
## crates/infrastructure/src/engine/registry.rs (rust)
  const BREW_INSTALL, struct DetectedEngine, struct DetectedTool, fn discover, fn discover_in, fn discover_tooling, fn find_binary, fn install_for, fn is_executable_file, fn is_executable_file, struct Tooling, const TOOLING
## crates/infrastructure/src/engine/routing.rs (rust)
  fn RoutingEngine<E>::id, fn RoutingEngine<E>::new, struct RoutingEngine, fn RoutingEngine<E>::run
## crates/infrastructure/src/engine/scripted.rs (rust)
  const APP_PY, const BA_BACKLOG, const COMPOSE, const DOCKERFILE, fn ScriptedEngine::id, fn ScriptedEngine::new, fn ScriptedEngine::ok, const PD_DESIGN_SYSTEM, const PD_UX, const PO_MILESTONES, const README_MD, fn ScriptedEngine::run
## crates/infrastructure/src/engine/transcript.rs (rust)
  fn TranscriptEngine<E>::id, fn TranscriptEngine<E>::new, fn role_key, fn TranscriptEngine<E>::run, struct TranscriptEngine, fn TranscriptEngine<E>::write
## crates/infrastructure/src/forge/gh_forge.rs (rust)
  fn ci_rollup, fn GhForge::close_pr, fn GhForge::comment_pr, fn PullRequest::from, fn GhForge::gh, fn gh, struct GhForge, fn GhForge::list_open_prs, fn GhForge::merge_pr, fn GhForge::new, fn GhForge::open_pr, fn GhForge::pr_diff
## crates/infrastructure/src/forge/gl_forge.rs (rust)
  fn ci_from_pipeline, fn ci_rollup_covers_pipeline_states, fn GlForge::close_pr, fn PullRequest::from, fn GlForge::glab, fn glab, struct GlForge, fn GlForge::list_open_prs, fn maps_gitlab_mr_json_to_pull_request, fn GlForge::merge_pr, fn GlForge::new, fn GlForge::open_pr
## crates/infrastructure/src/forge/mod.rs (rust)
## crates/infrastructure/src/git/mod.rs (rust)
## crates/infrastructure/src/git/system_git.rs (rust)
  fn SystemGit::abort_merge, fn author, fn SystemGit::checkout_branch, fn checkout_creates_then_switches_branch, fn SystemGit::commit_all, fn commit_all_commits_then_reports_clean, fn SystemGit::current_branch, fn git, fn init_repo, fn SystemGit::is_repo, fn is_repo_true_only_after_init, fn SystemGit::new
## crates/infrastructure/src/kv_doc.rs (rust)
  fn PgKvDoc::client, fn PgKvDoc::connect, const INIT_SQL, fn PgKvDoc::load, struct PgKvDoc, fn PgKvDoc::save
## crates/infrastructure/src/lib.rs (rust)
## crates/infrastructure/src/notifier.rs (rust)
  fn WebhookNotifier::new, fn WebhookNotifier::notify, fn posts_event_json_to_the_url, struct WebhookNotifier
## crates/infrastructure/src/sql_auth.rs (rust)
  fn SqlAuthService::assign_project, fn SqlAuthService::attach_device, struct Attempts, fn SqlAuthService::bootstrap_admin, fn SqlAuthService::cache_session, fn SqlAuthService::clear_failures, fn SqlAuthService::client, fn SqlAuthService::connect, fn SqlAuthService::create_token, fn SqlAuthService::create_user, fn SqlAuthService::delete_user, fn SqlAuthService::disable_2fa
## crates/infrastructure/src/state/any_store.rs (rust)
  fn AnyStateStore::acquire_leader, fn AnyStateStore::acquire_operator, enum AnyStateStore, fn AnyStateStore::claim_stage, fn AnyStateStore::claim_ticket, fn AnyStateStore::get_desired, fn AnyStateStore::heartbeat_worker, fn AnyStateStore::load, fn AnyStateStore::save, fn AnyStateStore::set_desired, fn AnyStateStore::workers
## crates/infrastructure/src/state/json_store.rs (rust)
  fn JsonStateStore::acquire_leader, fn JsonStateStore::acquire_leader_blocking, fn acquire_lock, fn age_secs, fn atomic_write, const BACKUP_DIR, fn JsonStateStore::claim_blocking, fn JsonStateStore::claim_stage, fn JsonStateStore::claim_stage_blocking, fn JsonStateStore::claim_ticket, struct Coord, const COORD_FILE
## crates/infrastructure/src/state/mod.rs (rust)
## crates/infrastructure/src/state/redis_coord.rs (rust)
  fn RedisCoord::acquire_leader, fn RedisCoord::acquire_operator_lock, fn RedisCoord::claim_stage, fn RedisCoord::conn, fn RedisCoord::connect, fn RedisCoord::get_desired, fn RedisCoord::heartbeat_worker, const LEADER_TTL_MS, const OPLOCK_TTL_MS, struct RedisCoord, fn RedisCoord::set_desired, const STAGE_TTL_MS
## crates/infrastructure/src/state/sql_store.rs (rust)
  fn SqlStateStore::acquire_leader, fn SqlStateStore::acquire_operator, fn SqlStateStore::claim_stage, fn SqlStateStore::claim_ticket, fn SqlStateStore::client, fn SqlStateStore::connect, fn SqlStateStore::get_desired, fn SqlStateStore::heartbeat_worker, const INIT_SQL, const LEADER_TTL_SECS, fn SqlStateStore::load, fn SqlStateStore::load_versioned
## crates/infrastructure/src/storage.rs (rust)
  fn S3Storage::ensure_bucket, fn fmt_amz_datetime, fn S3Storage::from_env, fn LocalStorage::get, fn S3Storage::get, fn hex, const HEX_LOWER, const HEX_UPPER, fn hmac, type HmacSha256, fn S3Storage::host, struct LocalStorage
## crates/infrastructure/src/totp.rs (rust)
  const B32, fn base32_decode, fn base32_encode, fn code_at, fn code_verifies_within_window_and_rejects_wrong, const DIGITS, fn generate_secret, type HmacSha1, fn hotp, fn provisioning_uri, fn rfc6238_test_vector_sha1, fn roundtrip_base32
## crates/infrastructure/tests/distributed_coord.rs (rust)
  fn distributed_coordination_across_two_hubs, fn env, fn ready_feature, fn store
## crates/infrastructure/tests/engine_discovery.rs (rust)
  fn empty_dirs_find_nothing, fn finds_executables_and_ignores_non_executables, fn make_executable
## crates/infrastructure/tests/sql_store_contract.rs (rust)
  fn sample_ticket, fn sql_store_satisfies_contract
## crates/infrastructure/tests/state_store_contract.rs (rust)
  fn json_store_creates_backup_on_overwrite, fn json_store_recovers_corrupt_state_from_backup, fn json_store_satisfies_contract, fn sample_ticket, fn state_store_contract
## crates/presentation/src/cli.rs (rust)
  struct Cli, enum CodegraphQuery, enum Command, fn parse
## crates/presentation/src/lib.rs (rust)
  fn render_changelog, fn render_report, fn status_label
## crates/presentation/src/server.rs (rust)
  const A, fn add_member_ep, fn agent_log_ep, struct AgentLogQuery, fn analyze_goal_ep, struct AnalyzeReq, fn app_download_ep, fn app_latest_ep, struct AppState, fn architecture_review_ep, fn assign_space_members, fn audit_ep
## crates/presentation/src/web/xterm-addon-fit.min.js (javascript)
  fn activate, fn dispose, fn fit, fn proposeDimensions
## crates/presentation/src/web/xterm.min.js (javascript)
  fn _, fn k::_, fn _addLineToZone, fn _addMouseDownListeners, fn _addStyle, fn P::_afterResize, fn _announceCharacters, fn _applyMinimumContrast, fn _applyScrollModifier, fn _areCoordsInSelection, fn _askForLink, fn _batchedMemoryCleanup
## crates/services/gateway/src/main.rs (rust)
  fn main
## crates/services/knowledge/src/main.rs (rust)
  fn main
## crates/services/realtime/src/main.rs (rust)
  fn main
## crates/services/runner/src/main.rs (rust)
  fn main
## desktop/CoXAgentApp.swift (swift)
  class AppDelegate, fn AppDelegate::applicationDidFinishLaunching, fn AppDelegate::applicationShouldTerminateAfterLastWindowClosed, fn AppDelegate::applicationWillTerminate, fn AppDelegate::applyUpdate, fn AppDelegate::coxagentURL, fn AppDelegate::loadDotEnv, fn AppDelegate::loadWhenReady, fn AppDelegate::minioReachable, fn AppDelegate::notifLog, fn AppDelegate::promptForPassword, fn AppDelegate::remoteHub
## desktop/coxagent-desktop/src/main.rs (rust)
  fn hub_path, fn main, const PORT, fn start_hub, fn wait_ready, fn workspace
## playwright.config.js (javascript)
## tests/ui.spec.js (javascript)
  fn initPage, fn loginViaApi
