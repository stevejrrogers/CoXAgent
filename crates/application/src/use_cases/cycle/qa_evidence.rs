//! What a test result has to SHOW: screenshots for UI, request and response
//! for an API, and the visual QA pass over a deployed page.
//!
//! "It passed" is not evidence; these gather what a person could check.

use super::{CycleReport, RunCycleUseCase};
use crate::ports::outbound::{AgentEnginePort, StateStorePort};
use coxagent_domain::TicketId;
use std::sync::Arc;

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// File a High bug when the test-suite DoD gate goes red (deduped on an open
    /// one). Deterministic quality signal from the actual toolchain.
    pub(super) async fn file_test_failure(&self, summary: &str) -> Option<TicketId> {
        use coxagent_domain::ticket::{Complexity, Priority, Status, TicketType};
        const MARKER: &str = "Tests failing";
        let Ok(state) = self.store.load().await else {
            return None;
        };
        if state.tickets.iter().any(|t| {
            t.ticket_type() == TicketType::Bug
                && t.status() == Status::Open
                && t.title().starts_with(MARKER)
        }) {
            return None;
        }
        let first = summary.lines().next().unwrap_or("test suite is red");
        crate::use_cases::AddTicketUseCase::new(Arc::clone(&self.store))
            .execute(crate::use_cases::AddTicketInput {
                ticket_type: TicketType::Bug,
                title: format!("{MARKER}: {first}"),
                description: format!(
                    "The test suite is failing — Definition of Done is not met. Make the tests \
                     pass (fix the code or the test).\n\nOutput:\n{summary}"
                ),
                priority: Priority::High,
                complexity: Complexity::Medium,
                has_ui: false,
                acceptance_criteria: vec!["The full test suite passes".to_owned()],
            })
            .await
            .ok()
    }
    /// Collect context-appropriate Definition-of-Done evidence for a shipped
    /// ticket and POST IT ON THE TICKET'S COMMENT THREAD: a UI ticket gets a
    /// real screenshot of the deployed app (stored as project media — local
    /// disk or MinIO/S3); a non-UI ticket gets a captured request/response.
    /// When the host cannot collect (no browser / no probe / app down), a
    /// `waived` record explains why so the TEST gate stays honest but
    /// unblocked.
    pub(super) async fn collect_evidence(&self, ticket: &TicketId) {
        let Some(port) = self.config.deploy.host_port else {
            return; // nothing deployed to prove against — gate is off
        };
        let key = ticket.to_string();
        let (has_ui, already) = match self.store.load().await {
            Ok(s) => (
                s.ticket(ticket)
                    .is_some_and(coxagent_domain::Ticket::has_ui),
                s.ticket_evidence.contains_key(&key),
            ),
            Err(_) => return,
        };
        if already {
            return;
        }
        if has_ui {
            self.collect_ui_evidence(ticket, port).await;
        } else {
            self.collect_api_evidence(ticket, port).await;
        }
    }
    pub(super) async fn collect_ui_evidence(&self, ticket: &TicketId, port: u16) {
        let key = ticket.to_string();
        let shot = match &self.shot {
            Some(shot) => shot.capture(&format!("http://127.0.0.1:{port}/")).await,
            None => None,
        };
        let uploaded = match (shot, &self.storage) {
            (Some(bytes), Some(storage)) => {
                let pid = self.config_project_label();
                let file = format!("evidence-{key}.png");
                match storage
                    .put(&format!("proj/{pid}/{file}"), &bytes, "image/png")
                    .await
                {
                    Ok(()) => Some((
                        format!("/api/projects/{pid}/media/{file}"),
                        bytes.len() as u64,
                    )),
                    Err(_) => None,
                }
            }
            _ => None,
        };
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
            match &uploaded {
                Some((url, size)) => {
                    s.add_evidence(&key, "screenshot", "deployed UI screenshot", url);
                    s.post_comment_att(
                        "TEST",
                        &format!("📸 DoD evidence for {key}: screenshot of the deployed UI."),
                        Some(key.clone()),
                        vec![crate::state::Attachment {
                            name: format!("evidence-{key}.png"),
                            url: url.clone(),
                            mime: "image/png".to_owned(),
                            size: *size,
                        }],
                    );
                }
                None => {
                    s.add_evidence(
                        &key,
                        "waived",
                        "screenshot unavailable",
                        "no headless browser/storage on this host, or the app did not render",
                    );
                }
            }
            Ok(())
        })
        .await;
    }
    pub(super) async fn collect_api_evidence(&self, ticket: &TicketId, port: u16) {
        let key = ticket.to_string();
        let Some(probe) = &self.probe else {
            let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                s.add_evidence(
                    &key,
                    "waived",
                    "probe unavailable",
                    "no HTTP probe on this host",
                );
                Ok(())
            })
            .await;
            return;
        };
        // Health first (universal), then root — first answer wins.
        let mut proof = None;
        for path in ["/api/health", "/"] {
            let url = format!("http://127.0.0.1:{port}{path}");
            if let Some(p) = probe.get(&url).await {
                proof = Some((url, p));
                break;
            }
        }
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
            match &proof {
                Some((url, p)) => {
                    let detail = format!("GET {url}\nHTTP {}\n{}", p.status, p.body_snippet.trim());
                    s.add_evidence(&key, "api", "live request/response", &detail);
                    s.post_comment(
                        "TEST",
                        &format!("🧾 DoD evidence for {key} — live API proof:\n```\n{detail}\n```"),
                        Some(key.clone()),
                    );
                }
                None => {
                    s.add_evidence(
                        &key,
                        "waived",
                        "app did not answer",
                        "probe got no response on health or root",
                    );
                }
            }
            Ok(())
        })
        .await;
    }
    /// Retry pass: shipped tickets still missing evidence, capped 2/cycle.
    pub(super) async fn collect_missing_evidence(&self) {
        use coxagent_domain::ticket::Status;
        let Ok(state) = self.store.load().await else {
            return;
        };
        let missing: Vec<TicketId> = state
            .tickets
            .iter()
            .filter(|t| {
                matches!(
                    t.status(),
                    Status::Done | Status::Fixed | Status::Documented
                ) && !state.ticket_evidence.contains_key(&t.id().to_string())
            })
            .map(|t| t.id().clone())
            .take(2)
            .collect();
        drop(state);
        for id in missing {
            self.collect_evidence(&id).await;
        }
    }
    /// Post-deploy visual QA on a UI feature: screenshot the running app,
    /// have PD review the ACTUAL pixels against the design system, and file
    /// at most 2 concrete UI bugs. Every step best-effort — no browser, no
    /// port, or an unparseable review just skips the pass.
    pub(super) async fn visual_qa(&self, ticket: &TicketId, report: &mut CycleReport) {
        let (Some(shot), Some(port)) = (&self.shot, self.config.deploy.host_port) else {
            return;
        };
        let is_ui = self
            .store
            .load()
            .await
            .ok()
            .and_then(|s| s.ticket(ticket).map(coxagent_domain::Ticket::has_ui))
            .unwrap_or(false);
        if !is_ui {
            return;
        }
        // The PD agent inspects the shot with its file tools, so it must land
        // on disk — through the files port.
        let out = self.work_dir.join(".coxagent").join("ui-shot.png");
        let (Some(bytes), Some(files)) = (
            shot.capture(&format!("http://127.0.0.1:{port}/")).await,
            &self.files,
        ) else {
            return;
        };
        if !files.write_bytes(&out, &bytes).await {
            return;
        }
        self.report("PD", "visual QA on the deployed UI");
        let request = crate::ports::outbound::AgentRequest {
            role: coxagent_domain::Role::Pd,
            system_prompt: crate::prompts::system_prompt(crate::prompts::PD),
            task_prompt: format!(
                "Visual QA. A screenshot of the app as ACTUALLY deployed (after \
                 shipping ticket {ticket}) is at `.coxagent/ui-shot.png` — open and \
                 LOOK at it with your file tools. Judge it against the project's \
                 design system and basic UI craft (alignment, contrast, spacing, \
                 broken layout, placeholder junk). Output ONLY a JSON array of at \
                 most 2 CONCRETE, visible defects: \
                 [{{\"title\": string, \"description\": string}}] — or [] if it looks right.",
            ),
            work_dir: self.work_dir.clone(),
            timeout: std::time::Duration::from_secs(600),
            escalation_level: 0,
            label: Some(ticket.to_string()),
        };
        let Ok(o) = self.engine.run(request).await else {
            return;
        };
        if !o.succeeded() {
            return;
        }
        let raw = &o.stdout;
        let (Some(a), Some(b)) = (raw.find('['), raw.rfind(']')) else {
            return;
        };
        let Ok(parsed) = serde_json::from_str::<Vec<serde_json::Value>>(&raw[a..=b]) else {
            return;
        };
        for item in parsed.iter().take(2) {
            let (Some(title), Some(desc)) = (
                item.get("title").and_then(serde_json::Value::as_str),
                item.get("description").and_then(serde_json::Value::as_str),
            ) else {
                continue;
            };
            let adder = crate::use_cases::AddTicketUseCase::new(Arc::clone(&self.store));
            if let Ok(id) = adder
                .execute(crate::use_cases::AddTicketInput {
                    ticket_type: coxagent_domain::ticket::TicketType::Bug,
                    title: format!("UI: {title}"),
                    description: format!("{desc}\n\n(Found by PD visual QA after {ticket}; screenshot: .coxagent/ui-shot.png)"),
                    priority: coxagent_domain::ticket::Priority::Medium,
                    complexity: coxagent_domain::ticket::Complexity::Small,
                    has_ui: true,
                    acceptance_criteria: Vec::new(),
                })
                .await
            {
                report.bugs_filed.push(id);
            }
        }
    }
    #[allow(clippy::too_many_lines)]
    /// Whether TEST has anything NEW to verify this cycle: work completed in
    /// this cycle, or Fixed tickets awaiting regression verification.
    pub(super) async fn test_has_work(&self, report: &CycleReport) -> bool {
        if report.feature_done.is_some() || report.bug_fixed.is_some() {
            return true;
        }
        // "A Fixed ticket exists" was always true while anything sat waiting to
        // be verified, so TEST re-ran every cycle and re-confirmed the same
        // tickets — 412 runs of it. Ask instead whether the set has CHANGED
        // since the last pass.
        let Ok(state) = self.store.load().await else {
            return false;
        };
        let waiting: Vec<String> = state
            .tickets
            .iter()
            .filter(|t| t.status() == coxagent_domain::Status::Fixed)
            .map(|t| t.id().to_string())
            .collect();
        if waiting.is_empty() {
            return false;
        }
        let fingerprint = waiting.join(",");
        if state.daily_jobs.get("test-verified-set") == Some(&fingerprint) {
            return false;
        }
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
            s.daily_jobs
                .insert("test-verified-set".to_owned(), fingerprint.clone());
            Ok(())
        })
        .await;
        true
    }

    /// Screenshot pass: attach a real screenshot to each UI ticket's test cases
    /// that TEST already marked pass/fail but which still lack an image. Runs
    /// right after the TEST engine so every verdict gets visual per-case proof.
    /// Deterministic (ScreenshotPort) and best-effort — no browser, no storage,
    /// or nothing to attach just skips.
    pub(super) async fn attach_test_case_screenshots(&self) {
        let Some(port) = self.config.deploy.host_port else {
            return;
        };
        let (Some(shot), Some(storage)) = (&self.shot, &self.storage) else {
            return;
        };
        let Ok(state) = self.store.load().await else {
            return;
        };
        let targets: Vec<(TicketId, Vec<String>)> = state
            .tickets
            .iter()
            .filter(|t| t.has_ui())
            .filter_map(|t| {
                let missing: Vec<String> = t
                    .test_cases()
                    .iter()
                    .filter(|tc| {
                        tc.status != coxagent_domain::ticket::TestCaseStatus::Pending
                            && tc.evidence.as_ref().map_or(true, |e| e.image.is_none())
                    })
                    .map(|tc| tc.description.clone())
                    .collect();
                (!missing.is_empty()).then(|| (t.id().clone(), missing))
            })
            .collect();

        let pid = self.config_project_label();
        for (ticket, missing) in &targets {
            let Some(bytes) = shot.capture(&format!("http://127.0.0.1:{port}/")).await else {
                continue;
            };
            let file = format!("evidence-{ticket}-cases.png");
            if storage
                .put(&format!("proj/{pid}/{file}"), &bytes, "image/png")
                .await
                .is_err()
            {
                continue;
            }
            let url = format!("/api/projects/{pid}/media/{file}");
            let at = crate::state::now_rfc3339();
            let Ok(mut st) = self.store.load().await else {
                continue;
            };
            let Some(t) = st.ticket_mut(ticket) else {
                continue;
            };
            let mut changed = false;
            for desc in missing {
                changed |= t.set_test_case_image(desc, url.clone(), at.clone());
            }
            if changed {
                let _ = self.store.save(&st).await;
            }
        }
    }
}
