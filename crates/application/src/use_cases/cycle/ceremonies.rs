//! Scrum ceremonies: the sprint boundary, standup, grooming, the daily
//! digest, and the self-tuning that reads the team's own numbers.
//!
//! Split out of the cycle so the rhythm of the team lives in one place and a
//! ticket about a ceremony stops colliding with a ticket about a deploy.

use super::{prune_memory_index, CycleReport, RunCycleUseCase, ARCH_REVIEW_EVERY_SPRINTS};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use std::fmt::Write as _;
use std::sync::Arc;

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// Scrum only: open/roll over the sprint at the start of a cycle, with an SM
    /// retro line when a previous sprint closes and a standup comment on open.
    pub(super) async fn advance_sprint_if_scrum(&self, cycle: u64) {
        if self.config.workflow.mode != crate::config::Mode::Scrum {
            return;
        }
        let Ok(mut state) = self.store.load().await else {
            return;
        };
        let _ = cycle; // the per-process cycle resets on restart — use the
                       // persistent counter below so sprints keep advancing.
        let len = self.config.workflow.sprint_length_cycles;
        // Migrate: seed the persistent counter from the current sprint's stored
        // (old per-process) cycle the first time, so an in-flight sprint doesn't
        // roll instantly, then advance it once per leader cycle.
        if state.sprint_cycle == 0 {
            state.sprint_cycle = state.sprint.as_ref().map_or(0, |s| s.started_cycle);
        }
        state.sprint_cycle += 1;
        let sc = state.sprint_cycle;
        let prev = state.sprint.as_ref().map(|s| {
            (
                s.number,
                s.committed.len(),
                crate::sprint::done_count(&state),
            )
        });
        // Capture the closing sprint before `advance` replaces it, so we can run
        // a real review + retro on it.
        let closing = state.sprint.clone();
        let Some(n) = crate::sprint::advance(&mut state, sc, len) else {
            // No roll this cycle — still persist the bumped counter.
            let _ = self.store.save(&state).await;
            return;
        };
        let _ = prev; // superseded by the richer review below
        let lang = self.config.workflow.language;
        if let Some(cl) = &closing {
            Self::sprint_review_and_retro(&mut state, cl, lang);
        }
        Self::sprint_planning(&mut state, n, lang);
        state.log_activity("SM", "opened sprint", Some(format!("sprint {n}")));
        let goal = state
            .sprint
            .as_ref()
            .map(|s| s.goal.clone())
            .unwrap_or_default();
        let _ = self.store.save(&state).await;
        self.notify("sprint_rolled", format!("Sprint {n} opened — goal: {goal}"))
            .await;
        // Learn: distill one concrete lesson from the closing sprint and keep
        // it — it gets fed back into the agents' prompts so they improve.
        if closing.is_some() {
            self.capture_retro_lesson().await;
        }
        // The team actually talks the plan through (PO/SA/DEV weigh in, SM
        // confirms the commitment) — planning as a ceremony, not an announce.
        self.scrum_planning().await;
        // Every few sprints the SA steps back and reviews the whole architecture,
        // filing refactor tickets and asking the PO to prioritise a hardening
        // sprint before tech debt compounds.
        if n % ARCH_REVIEW_EVERY_SPRINTS == 0 {
            self.architecture_audit(n).await;
            self.docs_audit(n).await;
        }
    }
    /// Drive the refactor sprint the SA called for. Returns whether one is active
    /// (so BA stays quiet). When the refactor chores are all done it clears the
    /// mode and announces; while they're open the SA realigns one pending
    /// feature's technical spec to the target architecture.
    pub(super) async fn maintain_refactor_sprint(&self) -> bool {
        let Ok(state) = self.store.load().await else {
            return false;
        };
        if !state.refactor_mode {
            return false;
        }
        let vi = self.config.workflow.language.is_vi();
        if state.open_refactor_count() == 0 {
            if let Ok(mut s) = self.store.load().await {
                s.refactor_mode = false;
                let msg = if vi {
                    "✅ Refactor sprint hoàn tất — nền tảng đã dọn xong, quay lại làm feature."
                } else {
                    "✅ Refactor sprint complete — the foundation is cleaned up; back to features."
                };
                s.post_comment("SA", msg, None);
                s.log_activity("SA", "refactor sprint complete", None);
                let _ = self.store.save(&s).await;
            }
            return false;
        }
        self.realign_one_spec(vi).await;
        true
    }
    /// Run the Sprint Planning ceremony: after the deterministic announce, the
    /// team discusses scope, risk, and capacity, and the SM confirms commitment.
    pub(super) async fn scrum_planning(&self) {
        self.report("SM", "sprint planning");
        let uc = crate::use_cases::RunPlanningUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.work_dir.clone(),
        )
        .with_language(self.config.workflow.language)
        .with_repo_note(self.pr_queue_note().await);
        if let Err(e) = uc.execute().await {
            tracing::warn!("sprint planning: {e}");
        }
    }
    /// A one-line summary of the open-PR queue for planning ("6 open PRs into
    /// main (#80 #82 …), 3 with merge conflicts"), or empty when clean/no forge.
    pub(super) async fn pr_queue_note(&self) -> String {
        let Some(forge) = &self.forge else {
            return String::new();
        };
        let Ok(prs) = forge.list_open_prs().await else {
            return String::new();
        };
        let target = self.flow_base().to_owned();
        let open: Vec<_> = prs.into_iter().filter(|p| p.base == target).collect();
        if open.is_empty() {
            return String::new();
        }
        let conflicted = open.iter().filter(|p| !p.mergeable).count();
        let ids = open
            .iter()
            .map(|p| format!("#{}", p.number))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "- Open PR queue into {target}: {} PR(s) ({ids}), {conflicted} with merge conflicts.",
            open.len()
        )
    }
    /// Run the Backlog Grooming ceremony: BA/SA/PO refine the top un-ready items
    /// so the backlog is healthy for the next planning.
    pub(super) async fn scrum_grooming(&self) {
        self.report("SM", "backlog grooming");
        let uc = crate::use_cases::RunGroomingUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.work_dir.clone(),
        )
        .with_language(self.config.workflow.language);
        if let Err(e) = uc.execute().await {
            tracing::warn!("backlog grooming: {e}");
        }
    }
    /// Ask the SM to distill ONE concrete, actionable lesson from how the last
    /// sprint went, store it, and post it — closing the learn-and-improve loop.
    pub(super) async fn capture_retro_lesson(&self) {
        let Ok(state) = self.store.load().await else {
            return;
        };
        let mut ctx = String::from("Recent activity + outcomes:\n");
        for a in state.activity.iter().rev().take(20) {
            let _ = writeln!(ctx, "- {}: {}", a.agent, a.action);
        }
        let prior = if state.lessons.is_empty() {
            String::new()
        } else {
            format!(
                "\nLessons already recorded:\n- {}\n",
                state.lessons.join("\n- ")
            )
        };
        let request = AgentRequest {
            role: coxagent_domain::Role::Sm,
            system_prompt: format!(
                "You are the SM running a sprint retrospective. Output ONE concrete, \
                 actionable lesson the team should apply next sprint — a single sentence, \
                 imperative, specific to what actually happened. No preamble.{}",
                self.config.workflow.language.reply_directive()
            ),
            task_prompt: format!(
                "{ctx}{prior}\nWhat is the single most valuable NEW lesson to carry forward? \
                 One sentence."
            ),
            work_dir: self.work_dir.clone(),
            timeout: std::time::Duration::from_secs(90),
            escalation_level: 0,
        };
        let Ok(outcome) = self.engine.run(request).await else {
            return;
        };
        let lesson = outcome.stdout.trim().trim_start_matches("- ").to_owned();
        if lesson.is_empty() || !outcome.succeeded() {
            return;
        }
        if let Ok(mut s) = self.store.load().await {
            s.add_lesson(&lesson);
            s.post_comment("SM", &format!("🎓 Retro lesson: {lesson}"), None);
            let _ = self.store.save(&s).await;
        }
        // Cross-project: the same lesson benefits every other project on this hub.
        crate::prompts::record_hub_lesson(&lesson);
    }
    /// Sprint Review + Retrospective, posted to the team channel: what shipped
    /// vs. what was committed, and a plain-spoken takeaway for next time.
    pub(super) fn sprint_review_and_retro(
        state: &mut crate::state::ProjectState,
        closing: &crate::state::Sprint,
        lang: crate::config::Language,
    ) {
        use coxagent_domain::ticket::Status;
        let is_done = |id: &coxagent_domain::TicketId| {
            state
                .tickets
                .iter()
                .any(|t| t.id() == id && matches!(t.status(), Status::Done | Status::Documented))
        };
        let shipped: Vec<String> = closing
            .committed
            .iter()
            .filter(|id| is_done(id))
            .map(ToString::to_string)
            .collect();
        let carry: Vec<String> = closing
            .committed
            .iter()
            .filter(|id| !is_done(id))
            .map(ToString::to_string)
            .collect();
        let total = closing.committed.len();
        let pct = (shipped.len() * 100).checked_div(total).unwrap_or(100);
        let shipped_list = if shipped.is_empty() {
            if lang.is_vi() {
                "chưa có gì lần này".to_owned()
            } else {
                "nothing this time".to_owned()
            }
        } else {
            shipped.join(", ")
        };
        let review = if lang.is_vi() {
            format!(
                "📋 Sprint {} review — đã ship {}/{}: {shipped_list}.",
                closing.number,
                shipped.len(),
                total
            )
        } else {
            format!(
                "📋 Sprint {} review — shipped {}/{}: {shipped_list}.",
                closing.number,
                shipped.len(),
                total
            )
        };
        state.post_comment("SM", &review, None);
        let takeaway = match (carry.is_empty(), lang.is_vi()) {
            (true, true) => {
                "Sprint gọn — mọi thứ cam kết đều ship. Giữ phạm vi thực tế thì sẽ duy trì được."
                    .to_owned()
            }
            (true, false) => {
                "Clean sprint — everything committed shipped. Keep the scope realistic and this holds."
                    .to_owned()
            }
            (false, true) => format!(
                "{} ticket bị mang sang ({}). Có thể đã cam kết quá tay — sprint sau lấy phần nhỏ hơn, rõ hơn.",
                carry.len(),
                carry.join(", ")
            ),
            (false, false) => format!(
                "{} ticket(s) carried over ({}). Likely over-committed — pull a smaller, clearer slice next sprint.",
                carry.len(),
                carry.join(", ")
            ),
        };
        state.post_comment(
            "SM",
            &format!(
                "🔄 Sprint {} retro — velocity {pct}%. {takeaway}",
                closing.number // "Sprint N retro" giữ nguyên nhãn cho bộ lọc timeline
            ),
            None,
        );
        state.log_activity(
            "SM",
            &format!("sprint {} review & retro", closing.number),
            None,
        );
    }
    /// Sprint Planning: announce the goal and the committed tickets, so the plan
    /// is visible rather than implicit.
    pub(super) fn sprint_planning(
        state: &mut crate::state::ProjectState,
        number: u32,
        lang: crate::config::Language,
    ) {
        let Some(sp) = state.sprint.clone() else {
            return;
        };
        let committed: Vec<String> = sp.committed.iter().map(ToString::to_string).collect();
        let list = if committed.is_empty() {
            if lang.is_vi() {
                "backlog trống".to_owned()
            } else {
                "backlog empty".to_owned()
            }
        } else {
            committed.join(", ")
        };
        let tag = if state.refactor_mode {
            " · 🛠️ REFACTOR SPRINT"
        } else {
            ""
        };
        let post = if lang.is_vi() {
            format!(
                "🏃 Sprint {number} planning{tag} — mục tiêu: {}. Cam kết {} ticket theo ưu tiên: {list}.",
                sp.goal,
                committed.len()
            )
        } else {
            format!(
                "🏃 Sprint {number} planning{tag} — goal: {}. Committed {} ticket(s) by priority: {list}.",
                sp.goal,
                committed.len()
            )
        };
        state.post_comment("SM", &post, None);
    }
    /// Daily self-tuning pass: recompute the evals and set/clear the quality
    /// and intake brakes (see `metrics::decide_tuning`). SM announces changes.
    pub(super) async fn self_tune(&self) {
        let today = crate::state::now_rfc3339()[..10].to_owned();
        let Ok(state) = self.store.load().await else {
            return;
        };
        if state.tuning.last_eval_day == today {
            return;
        }
        let evals = crate::metrics::agent_evals(&state);
        let backlog = state
            .tickets
            .iter()
            .filter(|t| {
                use coxagent_domain::ticket::Status;
                matches!(t.status(), Status::Pending | Status::Ready | Status::Open)
            })
            .count();
        let next = crate::metrics::decide_tuning(&evals, backlog, &state.tuning);
        let was = state.tuning.clone();
        drop(state);
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
            if s.tuning.last_eval_day == today {
                return Ok(());
            }
            let mut announce: Vec<String> = Vec::new();
            if next.bugs_first != was.bugs_first {
                announce.push(if next.bugs_first {
                    format!(
                        "quality brake ON — retry churn {:.2}/ship; features pause, bugs first",
                        evals.churn_per_ship
                    )
                } else {
                    "quality brake OFF — churn recovered, features resume".to_owned()
                });
            }
            if next.skip_ba != was.skip_ba {
                announce.push(if next.skip_ba {
                    format!(
                        "intake brake ON — backlog {backlog} tickets; BA pauses until it drains"
                    )
                } else {
                    "intake brake OFF — backlog drained, BA resumes".to_owned()
                });
            }
            s.tuning = next.clone();
            s.tuning.last_eval_day.clone_from(&today);
            // Mirror the hub-wide lessons into this project's Wiki (daily),
            // so cross-project knowledge is readable where people read —
            // not only injected into prompts.
            let hub =
                std::fs::read_to_string(crate::prompts::hub_lessons_path()).unwrap_or_default();
            if !hub.trim().is_empty() {
                s.ensure_standard_folders();
                s.upsert_doc(
                    "hub-lessons",
                    "Team",
                    crate::state::doc_category_of("Team"),
                    "Hub lessons (all projects)",
                    &format!(
                        "Lessons learned across EVERY project on this hub — auto-synced \
                         daily; agents also receive the most recent ones in their prompts.\n\n{hub}"
                    ),
                    "SM",
                );
            }
            for a in announce {
                let msg = format!("🎛️ Self-tuning: {a}");
                s.post_comment("SM", &msg, None);
                s.post_chat_in("SM", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
            }
            Ok(())
        })
        .await;
    }
    #[allow(clippy::too_many_lines)] // one linear pass; splitting hurts readability
    pub(super) async fn memory_hygiene(&self) {
        let today = crate::state::now_rfc3339()[..10].to_owned();
        {
            let Ok(state) = self.store.load().await else {
                return;
            };
            if state.last_memory_hygiene_day == today {
                return;
            }
        }
        let claimed = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            if s.last_memory_hygiene_day == today {
                return Err(crate::PortError::Conflict("already ran".into()));
            }
            s.last_memory_hygiene_day.clone_from(&today);
            Ok(())
        })
        .await;
        if claimed.is_err() {
            return; // another operator ran it today
        }
        self.report("SM", "memory hygiene");
        let Some(dir) = self.engine_memory_dir() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        // Oldest-modified first; MEMORY.md (the index) is never judged directly.
        let mut files: Vec<std::path::PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|e| e == "md")
                    && p.file_name().is_some_and(|n| n != "MEMORY.md")
            })
            .collect();
        files.sort_by_key(|p| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH)
        });
        // Batch scales with pressure: normally 3/day; when the memory dir has
        // grown past its budget, judge up to 10 so it converges back under.
        let total: u64 = files
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .sum();
        let batch = if total > 150_000 { 10 } else { 3 };
        let mut actions: Vec<String> = Vec::new();
        for path in files.into_iter().take(batch) {
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            if content.chars().count() < 1200 {
                continue; // small notes are cheap to keep
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let capped: String = content.chars().take(20_000).collect();
            let request = crate::ports::outbound::AgentRequest {
                role: coxagent_domain::Role::Sm,
                system_prompt: String::new(),
                task_prompt: format!(
                    "You are auditing one AGENT MEMORY file against the team's current process \
                     law. The law WINS over the memory — anything in the memory that \
                     contradicts it is stale and must go.\n\n{invariants}\n\n\
                     MEMORY FILE `{name}`:\n---\n{capped}\n---\n\n\
                     Reply with EXACTLY one of:\n\
                     KEEP — still accurate and worth its size.\n\
                     DELETE — mostly stale/contradicting; better gone than misleading.\n\
                     REWRITE\\n<new content> — keep the still-true parts, corrected to match \
                     the law, compressed under 2500 characters, same frontmatter style.\n\
                     Additionally, if the file contains a durable, TEAM-WIDE lesson (true on \
                     every machine, worth versioning), append at the very end:\n\
                     TEAM-NOTE: <one paragraph, under 500 characters>",
                    invariants = crate::prompts::PROCESS_INVARIANTS,
                ),
                work_dir: self.work_dir.clone(),
                timeout: std::time::Duration::from_secs(300),
                escalation_level: 0,
            };
            let Ok(o) = self.engine.run(request).await else {
                continue;
            };
            let full = o.stdout.trim();
            // A durable team-wide lesson gets PROMOTED into the repo's CLAUDE.md
            // — versioned, shared by every machine — before the verdict applies.
            let (out, team_note) = match full.split_once("TEAM-NOTE:") {
                Some((v, note)) => (v.trim(), Some(note.trim().to_owned())),
                None => (full, None),
            };
            if let Some(note) = team_note.filter(|n| n.len() > 40 && n.len() < 1000) {
                if self.promote_team_note(&note) {
                    actions.push(format!("📌 {name} → CLAUDE.md: {note}"));
                }
            }
            if out.starts_with("DELETE") {
                if std::fs::remove_file(&path).is_ok() {
                    prune_memory_index(&dir, &name);
                    actions.push(format!("🗑️ {name} — stale, contradicted current process"));
                }
            } else if let Some(rest) = out.strip_prefix("REWRITE") {
                let new = rest.trim_start_matches(['\n', '\r', ' ']);
                if new.len() > 100 && std::fs::write(&path, new).is_ok() {
                    actions.push(format!(
                        "✏️ {name} — corrected & compressed ({} → {} chars)",
                        content.chars().count(),
                        new.chars().count()
                    ));
                }
            }
        }
        if !actions.is_empty() {
            let msg = format!(
                "🧹 Memory hygiene: engine memory audited against current process law.\n{}",
                actions.join("\n")
            );
            let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                s.post_chat_in("SM", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
                s.log_activity("SM", "memory hygiene — engine memory corrected", None);
                Ok(())
            })
            .await;
        }
    }
    /// Claim a once-a-day job for today, atomically. Returns whether THIS call
    /// won it, so a job runs once per day across every runner sharing the state
    /// rather than once per cycle per runner.
    pub(super) async fn claim_daily(&self, job: &str) -> bool {
        let today = crate::state::now_rfc3339()[..10].to_owned();
        let key = job.to_owned();
        crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
            if s.daily_jobs.get(&key).is_some_and(|d| *d == today) {
                return Err(crate::PortError::Conflict("already ran today".into()));
            }
            s.daily_jobs.insert(key.clone(), today.clone());
            Ok(())
        })
        .await
        .is_ok()
    }
    /// SM impediment watch: the Scrum Master's real job — surface everything
    /// blocking flow as ONE daily picture instead of scattered noise, and keep
    /// surfacing it until it's gone. Sources are deterministic state, not LLM
    /// judgement: stuck PRs (fix-attempt brake tripped), parked tickets
    /// (3 failed builds), a red deploy, and active queue recovery.
    pub(super) async fn impediment_watch(&self) {
        let today = crate::state::now_rfc3339()[..10].to_owned();
        let Ok(state) = self.store.load().await else {
            return;
        };
        if state.last_impediment_day == today {
            return;
        }
        let mut items: Vec<String> = Vec::new();
        // Only post-ladder states reach the human report: a stuck PR the SA
        // already rescued once, a parked ticket the SA already re-designed.
        let stuck: Vec<String> = state
            .pr_fix_attempts
            .iter()
            .filter(|(pr, n)| **n >= 3 && state.pr_rescues.contains_key(pr))
            .map(|(pr, _)| format!("#{pr}"))
            .collect();
        if !stuck.is_empty() {
            items.push(format!(
                "PR kẹt SAU khi SA đã rescue (cần người quyết): {}",
                stuck.join(", ")
            ));
        }
        let parked: Vec<String> = state
            .ticket_fail_attempts
            .iter()
            .filter(|(_, n)| **n >= 3)
            .map(|(id, _)| id.clone())
            .collect();
        if !parked.is_empty() {
            items.push(format!(
                "Ticket bị PARK sau 3 lần build đỏ: {}",
                parked.join(", ")
            ));
        }
        if let Some(d) = &state.deploy {
            if !d.ok {
                items.push(format!(
                    "Deploy đang ĐỎ: {}",
                    d.summary.lines().next().unwrap_or("")
                ));
            }
        }
        if state.queue_recovery {
            items.push("Merge queue đang trong RECOVERY — chỉ merge, không code mới".to_owned());
        }
        drop(state);
        if items.is_empty() {
            // Still stamp the day so we don't re-scan every cycle.
            let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                s.last_impediment_day.clone_from(&today);
                Ok(())
            })
            .await;
            return;
        }
        // No embedded 🚧 prefix here — the icon is the notifier adapter's job
        // (`ChatNotifier`'s `kind_icon`), same as every other notify()-routed
        // event (pr_stuck, sprint_rolled, deploy_*).
        let msg = format!(
            "Impediment watch ({} mục) — SM theo sát tới khi sạch:\n- {}",
            items.len(),
            items.join("\n- ")
        );
        // `build_notifier` always wires a `ChatNotifier`, so in production the
        // in-app post arrives via the fanout (alongside the external webhook).
        // Only when NO notifier is attached at all do we keep the direct chat
        // write, so the digest is never silently lost.
        let has_notifier = self.notifier.is_some();
        let mut claimed = false;
        let wrote = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            if s.last_impediment_day == today {
                return Ok(()); // another operator beat us to it today
            }
            s.last_impediment_day.clone_from(&today);
            s.post_comment("SM", &msg, None);
            if !has_notifier {
                s.post_chat_in(
                    "SM",
                    &format!("🚧 {msg}"),
                    crate::state::AGENTS_CHANNEL,
                    Vec::new(),
                );
            }
            claimed = true;
            Ok(())
        })
        .await;
        // Notify only once the day-stamp is durably persisted: otherwise a
        // failed write would re-fire the digest on the next cycle.
        if wrote.is_ok() && claimed {
            self.notify("impediment_digest", msg).await;
        }
    }
    pub(super) async fn post_daily_digest(&self) {
        let today = crate::state::now_rfc3339()[..10].to_owned();
        let Ok(state) = self.store.load().await else {
            return;
        };
        if state.last_digest_day == today {
            return;
        }
        let first_ever = state.last_digest_day.is_empty();
        let digest = crate::metrics::digest_markdown(&state, &crate::state::now_rfc3339());
        drop(state);
        let posted = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            if s.last_digest_day == today {
                return Ok(()); // another operator beat us to it
            }
            s.last_digest_day.clone_from(&today);
            if !first_ever {
                s.post_chat_in(
                    "COX",
                    &format!("📰 {digest}"),
                    crate::state::AGENTS_CHANNEL,
                    Vec::new(),
                );
            }
            Ok(())
        })
        .await;
        if posted.is_ok() && !first_ever {
            tracing::info!("posted daily digest for {today}");
        }
    }
    /// Run the daily standup: the SM opens, each active agent posts a grounded
    /// update (done/next/blockers), and the SM highlights blockers + focus.
    pub(super) async fn scrum_standup(&self) {
        self.report("SM", "running standup");
        let uc = crate::use_cases::RunStandupUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.work_dir.clone(),
        )
        .with_language(self.config.workflow.language)
        .with_operator(self.worker.clone());
        match uc.execute().await {
            Ok(blockers) if blockers > 0 => {
                tracing::info!("standup surfaced {blockers} blocker(s)");
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("standup: {e}"),
        }
    }
    /// Make Scrum lively: when a real tension exists, run a facilitated
    /// discussion (PO & SA weigh in, SM decides, a decision may create a ticket),
    /// posting the whole exchange to the Scrum feed.
    pub(super) async fn scrum_discussion(&self, report: &CycleReport, cycle: u64) {
        let Ok(state) = self.store.load().await else {
            return;
        };
        let Some(topic) = Self::scrum_topic(&state, report, cycle, self.config.workflow.language)
        else {
            return;
        };
        // Skip if we discussed the exact same topic last cycle — prevents
        // duplicate noise when the trigger condition persists across cycles.
        {
            // The guard only holds a topic string, so a poisoned lock (another
            // thread panicked mid-update) costs nothing to recover from — take
            // the inner value rather than panic a whole cycle over dedupe state.
            let mut last = self
                .last_discussion_topic
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *last == topic {
                return;
            }
            (*last).clone_from(&topic);
        }
        self.report("SM", "scrum discussion");
        let uc = crate::use_cases::RunDiscussionUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.work_dir.clone(),
        )
        .with_language(self.config.workflow.language);
        if let Err(e) = uc.execute(&topic).await {
            tracing::warn!("scrum discussion: {e}");
        }
    }
}
