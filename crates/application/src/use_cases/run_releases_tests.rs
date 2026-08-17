//! Tests for `RunReleasesUseCase` — split out of `run_releases.rs` so the
//! release use case file stays small (AGENTS.md: one cohesive unit per file).
//! Test fixtures (`MemStore`, `SpyGit`) and every CXA-F008 acceptance
//! scenario live here; they import the use case through `super`.

#[cfg(test)]
mod tests {
    use crate::config::{Config, ReleasesConfig};
    use crate::ports::outbound::{GitAuthor, GitPort, StateStorePort};
    use crate::state::{DeployRecord, Milestone, ProjectState};
    use crate::use_cases::RunReleasesUseCase;
    use crate::PortError;
    use async_trait::async_trait;
    use coxagent_domain::{Complexity, Priority, SemVer, TicketId, TicketType};
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct MemStore {
        state: Mutex<ProjectState>,
    }

    #[async_trait]
    impl StateStorePort for MemStore {
        async fn load(&self) -> Result<ProjectState, PortError> {
            Ok(self.state.lock().expect("lock").clone())
        }
        async fn save(&self, state: &ProjectState) -> Result<(), PortError> {
            state.validate().map_err(PortError::Corrupt)?;
            *self.state.lock().expect("lock") = state.clone();
            Ok(())
        }
    }

    #[derive(Default)]
    struct SpyGit {
        tags: Mutex<Vec<String>>,
        head_sha: Mutex<Option<String>>,
        existing_tags: Mutex<BTreeSet<String>>,
    }

    #[async_trait]
    impl GitPort for SpyGit {
        async fn is_repo(&self, _: &std::path::Path) -> bool {
            true
        }

        async fn current_branch(&self, _: &std::path::Path) -> Result<String, PortError> {
            Ok("main".to_owned())
        }

        async fn checkout_branch(&self, _: &std::path::Path, _: &str) -> Result<(), PortError> {
            Ok(())
        }

        async fn commit_all(
            &self,
            _: &std::path::Path,
            _message: &str,
            _author: &GitAuthor,
        ) -> Result<Option<String>, PortError> {
            Ok(Some("abc123".to_owned()))
        }

        async fn push(&self, _: &std::path::Path, _: &str) -> Result<(), PortError> {
            Ok(())
        }

        async fn sync_base(
            &self,
            _: &std::path::Path,
            _: &str,
        ) -> Result<crate::ports::outbound::SyncBase, PortError> {
            Ok(crate::ports::outbound::SyncBase::UpToDate)
        }

        async fn abort_merge(&self, _: &std::path::Path) -> Result<(), PortError> {
            Ok(())
        }

        async fn head_sha(&self, _: &std::path::Path) -> Result<String, PortError> {
            self.head_sha
                .lock()
                .expect("lock")
                .clone()
                .ok_or(PortError::Backend("no head sha set".to_owned()))
        }

        async fn update_ref(
            &self,
            _: &std::path::Path,
            _refname: &str,
            _sha: &str,
        ) -> Result<(), PortError> {
            Ok(())
        }

        async fn worktree_add(
            &self,
            _: &std::path::Path,
            _path: &std::path::Path,
            _sha: &str,
        ) -> Result<(), PortError> {
            Ok(())
        }

        async fn worktree_remove(
            &self,
            _: &std::path::Path,
            _path: &std::path::Path,
        ) -> Result<(), PortError> {
            Ok(())
        }

        async fn changed_paths(
            &self,
            _: &std::path::Path,
            _from_sha: &str,
            _to_sha: &str,
        ) -> Result<Vec<String>, PortError> {
            Ok(Vec::new())
        }

        async fn create_tag(
            &self,
            _work_dir: &std::path::Path,
            name: &str,
            _ref_target: &str,
            _author: &GitAuthor,
        ) -> Result<(), PortError> {
            self.tags.lock().expect("lock").push(name.to_owned());
            Ok(())
        }

        async fn tag_exists(&self, _work_dir: &std::path::Path, name: &str) -> bool {
            self.existing_tags.lock().expect("lock").contains(name)
        }
    }

    fn make_ticket(id: &str, title: &str, desc: &str) -> coxagent_domain::Ticket {
        coxagent_domain::Ticket::new(
            TicketId::new(id).expect("valid id"),
            TicketType::Feature,
            title,
            desc,
            Priority::Medium,
            Complexity::Medium,
            false,
        )
        .expect("valid ticket")
    }

    fn make_deploy(version: &str, ticket: &str, title: &str, at: &str) -> DeployRecord {
        DeployRecord {
            version: SemVer::parse(version).expect("version"),
            ticket: TicketId::new(ticket).expect("ticket id"),
            title: title.to_owned(),
            at: at.to_owned(),
        }
    }

    fn seeded(
        version: &str,
        name: &str,
        target: &str,
        goal: &str,
        goal_complete: bool,
        fulfilled: bool,
        history: Vec<DeployRecord>,
    ) -> (Arc<MemStore>, Arc<SpyGit>) {
        let mut state = ProjectState {
            current_version: SemVer::parse(version).expect("version"),
            milestones: vec![Milestone {
                name: name.to_owned(),
                goal: goal.to_owned(),
                target_version: target.to_owned(),
                goal_complete,
                fulfilled,
            }],
            history,
            ..ProjectState::default()
        };
        state
            .tickets
            .push(make_ticket("CXA-F001", "Add user auth", "login/logout"));
        (
            Arc::new(MemStore {
                state: Mutex::new(state),
            }),
            Arc::new(SpyGit {
                tags: Mutex::new(Vec::new()),
                head_sha: Mutex::new(Some("def456".to_owned())),
                existing_tags: Mutex::new(BTreeSet::new()),
            }),
        )
    }

    fn uc(store: &Arc<MemStore>, git: &Arc<SpyGit>) -> RunReleasesUseCase<MemStore> {
        RunReleasesUseCase::new(Arc::clone(store), PathBuf::from("/tmp"))
            .with_config(Config {
                releases: ReleasesConfig { enabled: true, cut_every_days: 0 },
                ..Config::default()
            })
            .with_git(Some(git.clone() as Arc<dyn GitPort>))
    }

    /// CXA-F008: the pipeline is inert until `releases.enabled` is set — a
    /// default-off skip, because tagging mutates the codebase's git history.
    #[tokio::test]
    async fn releases_are_skipped_until_enabled() {
        let (store, git) = seeded(
            "0.5.0",
            "Alpha",
            "0.5.0",
            "Ship dashboard",
            true,
            false,
            vec![make_deploy(
                "0.5.0",
                "CXA-F001",
                "Auth",
                "2026-07-01T00:00:00Z",
            )],
        );
        // Default config: `releases.enabled` is OFF — no tag, no chore, and the
        // milestone stays unfulfilled so it can still release later.
        let uc = RunReleasesUseCase::new(Arc::clone(&store), PathBuf::from("/tmp"))
            .with_git(Some(git.clone() as Arc<dyn GitPort>));

        let released = uc.execute().await.expect("pipeline runs disabled");
        assert!(
            released.is_empty(),
            "no release while releases.enabled is off"
        );
        assert!(
            git.tags.lock().expect("lock").is_empty(),
            "no tag created while releases.enabled is off"
        );
        let state = store.load().await.expect("load state");
        assert!(
            state
                .tickets
                .iter()
                .all(|t| t.ticket_type() != TicketType::Chore),
            "no Release chore filed while releases.enabled is off"
        );
        assert!(
            !state.milestones[0].fulfilled,
            "milestone not marked fulfilled while releases.enabled is off"
        );
    }

    #[tokio::test]
    async fn creates_git_tag_when_milestone_reached_and_goal_complete() {
        let (store, git) = seeded(
            "0.5.0",
            "Alpha",
            "0.5.0",
            "Ship dashboard",
            true,
            false,
            vec![make_deploy(
                "0.5.0",
                "CXA-F001",
                "Auth",
                "2026-07-01T00:00:00Z",
            )],
        );
        let released = uc(&store, &git)
            .execute()
            .await
            .expect("run release pipeline");
        assert!(
            released.contains(&"Alpha".to_owned()),
            "Alpha milestone should be released"
        );
        assert_eq!(
            *git.tags.lock().expect("lock"),
            vec!["Alpha".to_owned()],
            "Git tag created with milestone name"
        );
    }

    #[tokio::test]
    async fn skips_when_version_not_reached() {
        let (store, git) = seeded("0.4.9", "Beta", "0.5.0", "Beta goal", true, false, vec![]);
        let released = uc(&store, &git).execute().await.expect("pipeline runs");
        assert!(
            released.is_empty(),
            "no release triggered below target version"
        );
        assert!(
            git.tags.lock().expect("lock").is_empty(),
            "no tag created when version too low"
        );
    }

    #[tokio::test]
    async fn skips_when_goal_not_complete() {
        let (store, git) = seeded("0.5.0", "Beta", "0.5.0", "Beta goal", false, false, vec![]);
        let released = uc(&store, &git).execute().await.expect("pipeline runs");
        assert!(released.is_empty(), "no release when goal incomplete");
        assert!(git.tags.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn creates_release_chore_ticket_with_dependencies_and_marks_fulfilled() {
        let (store, git) = seeded(
            "0.5.0",
            "Alpha",
            "0.5.0",
            "Ship MVP",
            true,
            false,
            vec![make_deploy(
                "0.5.0",
                "CXA-F001",
                "Auth",
                "2026-07-01T00:00:00Z",
            )],
        );
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-F002",
            "Dashboard",
            "main screen",
        ));
        uc(&store, &git)
            .execute()
            .await
            .expect("pipeline completes");

        let state = store.load().await.expect("load state");
        let chore = state
            .tickets
            .iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains("Alpha"))
            .expect("Release chore ticket should be created");
        assert!(state.milestones[0].fulfilled, "Milestone marked fulfilled");
        assert!(
            chore.description().contains("CXA-F001"),
            "release notes reference producing tickets: {}",
            chore.description()
        );
    }

    /// CXA-F008: releasing posts the release notes to the team room (the same
    /// channel the PO uses for milestone announcements), attributed to RELEASE.
    #[tokio::test]
    async fn posts_release_notes_to_the_team_channel() {
        let (store, git) = seeded(
            "0.5.0",
            "Alpha",
            "0.5.0",
            "Ship MVP",
            true,
            false,
            vec![make_deploy(
                "0.5.0",
                "CXA-F001",
                "Auth",
                "2026-07-01T00:00:00Z",
            )],
        );
        uc(&store, &git)
            .execute()
            .await
            .expect("pipeline completes");

        let state = store.load().await.expect("load state");
        let comment = state
            .comments
            .iter()
            .find(|c| c.author == "RELEASE")
            .expect("release note posted to the team channel");
        assert!(
            comment.body.contains("CXA-F001"),
            "posted notes reference producing tickets: {}",
            comment.body
        );
        assert!(
            comment.body.contains("Auth"),
            "posted notes include the producing ticket title: {}",
            comment.body
        );
    }

    #[tokio::test]
    async fn skips_when_tag_exists_and_logs_activity() {
        let (store, git) = seeded("0.5.0", "Alpha", "0.5.0", "Ship MVP", true, false, vec![]);
        git.existing_tags
            .lock()
            .expect("lock")
            .insert("Alpha".to_owned());
        let initial = store.state.lock().expect("lock").tickets.len();
        uc(&store, &git)
            .execute()
            .await
            .expect("pipeline does not fail");
        assert!(
            git.tags.lock().expect("lock").is_empty(),
            "no duplicate tag created"
        );
        let state = store.load().await.expect("load state");
        assert!(
            state
                .activity
                .iter()
                .any(|e| e.action.contains("already exists")),
            "activity entry for existing release logged"
        );
        assert_eq!(
            state.tickets.len(),
            initial,
            "no new ticket when release exists"
        );
    }

    // --- ACs for CXA-F008: Automated Release Pipeline ---

    /// AC1: When current_version *exceeds* the milestone target_version and
    /// the goal is complete, the pipeline still creates a tag matching the
    /// milestone name. (Regression: version above target is not a skip.)
    #[tokio::test]
    async fn tags_milestone_when_version_exceeds_target() {
        // Version 0.6.0 exceeds Alpha target 0.5.0
        let (store, git) = seeded(
            "0.6.0",
            "Alpha",
            "0.5.0",
            "Ship dashboard",
            true,
            false,
            vec![
                make_deploy("0.5.0", "CXA-H001", "Auth", "2026-07-01"),
                make_deploy("0.6.0", "CXA-H002", "Dash", "2026-07-15"),
            ],
        );
        // Use unique IDs to avoid collision with seeded default ticket
        store
            .state
            .lock()
            .expect("lock")
            .tickets
            .push(make_ticket("CXA-H001", "Auth", "login"));
        store
            .state
            .lock()
            .expect("lock")
            .tickets
            .push(make_ticket("CXA-H002", "Dash", "screen"));

        let released = uc(&store, &git)
            .execute()
            .await
            .expect("pipeline runs even when version exceeds target");
        assert!(
            released.contains(&"Alpha".to_owned()),
            "release triggered when version exceeds target"
        );
        assert_eq!(
            *git.tags.lock().expect("lock"),
            vec!["Alpha".to_owned()],
            "git tag created with milestone name"
        );
    }

    /// AC2: Release notes include all ticket IDs, titles, and changelog
    /// entries produced during the version span from the last shipped version
    /// to the current version — not the whole deploy history.
    #[tokio::test]
    async fn release_notes_cover_version_span_from_last_shipped_to_current() {
        // Previous shipment: "Alpha" at 0.4.0. Target/milestone "Beta" at 0.5.0.
        // Current: 0.5.2 — deploy history at 0.3.0, 0.4.0, 0.5.0, 0.5.2.
        // Notes must include 0.4.0 (last shipped) and 0.5.0 entries — not 0.3.0.
        // Use unique IDs to avoid collision with seeded default CXA-F001.
        let (store, git) = seeded(
            "0.5.2",
            "Beta",
            "0.5.0",
            "Beta goal",
            true,
            false,
            vec![
                make_deploy("0.3.0", "COX-E001", "Old feature", "2026-04-01"),
                make_deploy("0.4.0", "CXA-E002", "Auth ticket", "2026-05-15"),
                make_deploy("0.5.0", "CXA-E003", "Dash ticket", "2026-06-01"),
                make_deploy("0.5.2", "CXA-E004", "Fix crash", "2026-06-20"),
            ],
        );

        // Prior milestone was already fulfilled — represents the last shipped
        // version for this span.
        {
            let mut state = store.state.lock().expect("lock");
            state.milestones.push(Milestone {
                name: "Alpha".to_owned(),
                goal: "Alpha goal".to_owned(),
                target_version: "0.4.0".to_owned(),
                goal_complete: true,
                fulfilled: true,
            });
            // Seeds the producing tickets so the notes can reference them.
            state.tickets.push(make_ticket("COX-E001", "Old", "old"));
            state
                .tickets
                .push(make_ticket("CXA-E002", "Auth ticket", "auth"));
            state
                .tickets
                .push(make_ticket("CXA-E003", "Dash ticket", "dash"));
            state
                .tickets
                .push(make_ticket("CXA-E004", "Fix crash", "fix"));
        }

        uc(&store, &git)
            .execute()
            .await
            .expect("pipeline completes");

        let state = store.load().await.expect("load state");
        let chore = state
            .tickets
            .iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains("Beta"))
            .expect("Beta release chore created");
        let body = chore.description();

        // Must reference tickets deployed at/after the last shipped version.
        assert!(
            body.contains("CXA-E002"),
            "notes must include last-shipped ticket: \n{body}"
        );
        assert!(
            body.contains("Auth ticket"),
            "notes must include ticket title from last shipped: \n{body}"
        );
        assert!(
            body.contains("CXA-E003") || body.contains("CXA-E004"),
            "notes must include current-range tickets: \n{body}"
        );

        // Must NOT include entries from before the last shipped version.
        assert!(
            !body.contains("COX-E001"),
            "notes must NOT include pre-shipment tickets: \n{body}"
        );
    }

    /// AC4: Release chore ticket references the milestone and carries all
    /// producing feature/bug ticket IDs as dependencies — the formal `depends_on`
    /// linkage, not just prose in the description.
    #[tokio::test]
    async fn release_chore_ticket_has_producing_ticket_ids_as_dependencies() {
        // Use separate ticket/produce IDs to avoid collision with seeded defaults.
        let (store, git) = seeded(
            "0.5.0",
            "Alpha",
            "0.5.0",
            "Ship MVP",
            true,
            false,
            vec![
                make_deploy("0.5.0", "CXA-D001", "Auth ticket", "2026-07-01"),
                make_deploy("0.5.0", "CXA-D002", "Dash ticket", "2026-07-02"),
            ],
        );
        // Producing feature tickets in the project.
        {
            let mut state = store.state.lock().expect("lock");
            state
                .tickets
                .push(make_ticket("CXA-D001", "Auth ticket", "login/logout"));
            state
                .tickets
                .push(make_ticket("CXA-D002", "Dash ticket", "dashboard"));
        }

        uc(&store, &git).execute().await.expect("pipeline runs");

        let state = store.load().await.expect("load state");
        let chore = state
            .tickets
            .iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains("Alpha"))
            .expect("release chore ticket created");

        // The chore must list producing tickets as formal dependencies.
        let deps = chore.depends_on();
        assert!(
            deps.iter().any(|d| d.to_string() == "CXA-D001"),
            "chore must depend on producing feature CXA-D001; got deps: {:?}",
            deps.iter().map(ToString::to_string).collect::<Vec<_>>()
        );
        assert!(
            deps.iter().any(|d| d.to_string() == "CXA-D002"),
            "chore must depend on producing feature CXA-D002; got deps: {:?}",
            deps.iter().map(ToString::to_string).collect::<Vec<_>>()
        );
    }

    // --- CXA-F008: Additional failing tests for Automated Release Pipeline ---

    /// AC1: When current_version is EXACTLY the target_version (equality gate),
    /// the pipeline still creates a git tag. Regression: only "greater than" works,
    /// not "meets or exceeds".
    #[tokio::test]
    async fn tags_milestone_when_version_exactly_meets_target() {
        // Version 0.5.0 exactly equals target (not above, not below)
        let (store, git) = seeded(
            "0.5.0",
            "Gamma-Exact",
            "0.5.0",
            "Exact target milestone",
            true,
            false,
            vec![make_deploy(
                "0.5.0",
                "CXA-GEX01",
                "Exact ticket",
                "2026-08-01",
            )],
        );
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-GEX01",
            "Exact ticket",
            "test",
        ));

        let released = uc(&store, &git)
            .execute()
            .await
            .expect("pipeline runs at exact version");
        assert!(
            released.contains(&"Gamma-Exact".to_owned()),
            "release triggered at exact version match"
        );
        assert_eq!(
            *git.tags.lock().expect("lock"),
            vec!["Gamma-Exact".to_owned()],
            "git tag created at exact target version"
        );
    }

    /// AC1: Release notes are sourced from deploy history — must include
    /// deploy history entries, not just milestone info.
    #[tokio::test]
    async fn release_notes_sourced_from_deploy_history() {
        // Use a ticket with a unique deploy title to prove notes come from
        // deploy history, not just ticket list.
        let (store, git) = seeded(
            "0.5.0",
            "HistoryMilestone",
            "0.5.0",
            "History milestone",
            true,
            false,
            vec![make_deploy(
                "0.5.0",
                "CXA-HIS01",
                "Deploy history title unique to changelog",
                "2026-08-01T00:00:00Z",
            )],
        );
        // The ticket has a DIFFERENT body — notes must use deploy history title.
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-HIS01",
            "Deploy history title unique to changelog",
            "Different ticket description body",
        ));

        uc(&store, &git).execute().await.expect("pipeline runs");

        let state = store.load().await.expect("load state");
        let chore = state
            .tickets
            .iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains("History"))
            .expect("release chore created");
        let body = chore.description();

        // Notes from deploy history must contain the deploy title, not just
        // the ticket body. This guards against a shortcut where notes just say
        // "released X" without the actual deploy details.
        assert!(
            body.contains("Deploy history title unique to changelog"),
            "release notes must include deploy history title: \n{body}"
        );
        // Must also include version info from the deploy history entry.
        assert!(
            body.contains("v0.5.0"),
            "release notes must include version from deploy history: \n{body}"
        );
    }

    /// AC2: Release notes include ALL ticket IDs and titles from the version
    /// span, not just a count or reference to some subset.
    #[tokio::test]
    async fn release_notes_include_all_ticket_ids_and_titles() {
        // Three producing tickets in the version span — all must appear.
        let (store, git) = seeded(
            "0.5.2",
            "AllIncluded",
            "0.5.0",
            "Include all milestone",
            true,
            false,
            vec![
                make_deploy("0.5.0", "CXA-AI01", "First ticket title", "2026-08-01"),
                make_deploy("0.5.1", "CXA-AI02", "Second ticket title", "2026-08-02"),
                make_deploy("0.5.2", "CXA-AI03", "Third ticket title", "2026-08-03"),
            ],
        );
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-AI01",
            "First ticket title",
            "desc",
        ));
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-AI02",
            "Second ticket title",
            "desc",
        ));
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-AI03",
            "Third ticket title",
            "desc",
        ));

        uc(&store, &git).execute().await.expect("pipeline runs");

        let state = store.load().await.expect("load state");
        let chore = state
            .tickets
            .iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains("AllIncluded"))
            .expect("release chore created");
        let body = chore.description();

        // Every producing ticket's ID must appear.
        assert!(
            body.contains("CXA-AI01"),
            "all ticket IDs required; missing CXA-AI01: \n{body}"
        );
        assert!(
            body.contains("CXA-AI02"),
            "all ticket IDs required; missing CXA-AI02: \n{body}"
        );
        assert!(
            body.contains("CXA-AI03"),
            "all ticket IDs required; missing CXA-AI03: \n{body}"
        );

        // Every producing ticket's title must appear.
        assert!(
            body.contains("First ticket title"),
            "all titles required; missing first: \n{body}"
        );
        assert!(
            body.contains("Second ticket title"),
            "all titles required; missing second: \n{body}"
        );
        assert!(
            body.contains("Third ticket title"),
            "all titles required; missing third: \n{body}"
        );
    }

    /// AC3: When tag exists, pipeline logs a 'release already exists' activity
    /// entry — not some vague message.
    #[tokio::test]
    async fn existing_tag_logs_release_already_exists_activity_entry() {
        let (store, git) = seeded(
            "0.5.0",
            "LogTest",
            "0.5.0",
            "Logging test milestone",
            true,
            false,
            vec![],
        );
        git.existing_tags
            .lock()
            .expect("lock")
            .insert("LogTest".to_owned());

        // Pipeline should NOT fail — just log and skip.
        let released = uc(&store, &git)
            .execute()
            .await
            .expect("pipeline does not fail when tag exists");
        assert!(released.is_empty(), "no release created when tag exists");

        let state = store.load().await.expect("load state");
        // Must find RELEASE activity entry with the specific text.
        let release_activity = state
            .activity
            .iter()
            .find(|e| e.agent == "RELEASE" && e.action.contains("release already exists"));
        assert!(
            release_activity.is_some(),
            "RELEASE activity must contain 'release already exists' text: {:?}",
            state
                .activity
                .iter()
                .map(|e| (&e.agent, &e.action))
                .collect::<Vec<_>>()
        );
    }

    /// AC2: First release with no prior shipped version — release notes must
    /// still be created using the deploy history available, not silently fail.
    #[tokio::test]
    async fn first_release_with_no_prior_shipped_version_produces_notes() {
        // Fresh project: milestone at 0.5.0, current version 0.5.0, one deploy
        // at 0.4.0 (below milestone) — that's the only history.
        let (store, git) = seeded(
            "0.5.0",
            "FirstRelease",
            "0.5.0",
            "First milestone",
            true,
            false,
            vec![make_deploy(
                "0.4.0",
                "CXA-FR01",
                "First deploy ever",
                "2026-06-01T00:00:00Z",
            )],
        );
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-FR01",
            "First deploy ever",
            "desc",
        ));

        uc(&store, &git)
            .execute()
            .await
            .expect("pipeline runs even without prior shipped version");

        let state = store.load().await.expect("load state");
        let chore = state
            .tickets
            .iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains("FirstRelease"))
            .expect("release chore created for first milestone");
        let body = chore.description();

        // Even with no prior shipped version, notes must include deploy info
        // from history that exists.
        assert!(
            body.contains("CXA-FR01"),
            "first release notes must include available deploy entries: \n{body}"
        );
    }

    /// AC1: When current version is 0.0.1 patch above the target (barely
    /// exceeds), the comparison is not a strict greater-than — it is "meets or
    /// exceeds". Regression: only versions far above target release, not
    /// patches.
    #[tokio::test]
    async fn tags_milestone_when_patch_below_major_exceeds_target() {
        // Version 0.5.1 is just 1 patch above 0.5.0 target
        let (store, git) = seeded(
            "0.5.1",
            "PatchExceeds",
            "0.5.0",
            "Target 0.5.0",
            true,
            false,
            vec![
                make_deploy("0.5.0", "CXA-P001", "Base ticket", "2026-08-01"),
                make_deploy("0.5.1", "CXA-P002", "Patch ticket", "2026-08-02"),
            ],
        );
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-P001",
            "Base ticket",
            "desc",
        ));
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-P002",
            "Patch ticket",
            "desc",
        ));

        let released = uc(&store, &git).execute().await.expect("pipeline runs");
        assert!(
            released.contains(&"PatchExceeds".to_owned()),
            "release at 0.5.1 triggers for 0.5.0 target"
        );
        assert!(
            !git.tags.lock().expect("lock").is_empty(),
            "tag created even when only 1 patch above target"
        );
    }

    /// AC2: Release notes include ticket titles, not just IDs. Without the
    /// title a human cannot verify the release scope from the notes alone.
    #[tokio::test]
    async fn release_notes_include_ticket_titles() {
        // Unique deploy title different from ticket body
        let (store, git) = seeded(
            "0.5.0",
            "TitleMustAppear",
            "0.5.0",
            "Title milestone",
            true,
            false,
            vec![make_deploy(
                "0.5.0",
                "CXA-T001",
                "The exact deploy title to appear",
                "2026-08-01T00:00:00Z",
            )],
        );
        // Ticket body differs from deploy title to prove notes come from deploy
        // history.
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-T001",
            "The exact deploy title to appear",
            "A totally different description body",
        ));

        uc(&store, &git).execute().await.expect("pipeline runs");

        let state = store.load().await.expect("load state");
        let chore = state
            .tickets
            .iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains("Title"))
            .expect("release chore created");
        let body = chore.description();

        // Verify the deploy title is present (not just the generic ticket body)
        assert!(
            body.contains("The exact deploy title to appear"),
            "release notes must include deploy title: \n{body}"
        );
    }

    /// AC3: Activity entry for "release already exists" must include the
    /// milestone name for audit traceability.
    #[tokio::test]
    async fn existing_tag_activity_entry_includes_milestone_name() {
        let milestone_name = "AuditTrailCheck";
        let (store, git) = seeded(
            "0.5.0",
            milestone_name,
            "0.5.0",
            "Audit milestone",
            true,
            false,
            vec![],
        );
        git.existing_tags
            .lock()
            .expect("lock")
            .insert(milestone_name.to_owned());
        let _ = uc(&store, &git)
            .execute()
            .await
            .expect("pipeline skips gracefully");

        let state = store.load().await.expect("load state");
        let activity = state
            .activity
            .iter()
            .find(|e| e.agent == "RELEASE" && e.action.contains("already exists"));
        assert!(
            activity.is_some(),
            "activity entry must exist for skipped release"
        );
        // Verify the milestone name is in the activity text
        assert!(
            activity.unwrap().action.contains(milestone_name),
            "activity must name the milestone: {}",
            activity.unwrap().action
        );
    }

    /// AC2: When no deploy history exists yet (fresh project), release notes
    /// still include milestone info rather than being empty or causing an error.
    #[tokio::test]
    async fn release_with_no_deploy_history_produces_meaningful_notes() {
        // No deploy history — the notes cannot list tickets, but must not fail.
        let (store, git) = seeded(
            "0.5.0",
            "NoHistory",
            "0.5.0",
            "No history milestone",
            true,
            false,
            vec![], // Empty deploy history
        );

        uc(&store, &git)
            .execute()
            .await
            .expect("pipeline completes without deploy history");

        let state = store.load().await.expect("load state");
        let chore = state
            .tickets
            .iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains("NoHistory"))
            .expect("release chore created even with no history");

        // Notes should contain the milestone name at minimum
        assert!(
            !chore.description().is_empty(),
            "release notes must not be empty"
        );
        assert!(
            chore.description().contains("NoHistory"),
            "notes must reference the milestone: {}",
            chore.description()
        );
    }

    /// AC4: The Release chore ticket's title must include "Release" to
    /// distinguish it from other chores.
    #[tokio::test]
    async fn release_chore_ticket_title_contains_release() {
        let (store, git) = seeded(
            "0.5.0",
            "ReleaseTitle",
            "0.5.0",
            "Title check milestone",
            true,
            false,
            vec![make_deploy("0.5.0", "CXA-TL01", "Ticket", "2026-08-01")],
        );
        store
            .state
            .lock()
            .expect("lock")
            .tickets
            .push(make_ticket("CXA-TL01", "Ticket", "desc"));

        uc(&store, &git).execute().await.expect("pipeline runs");

        let state = store.load().await.expect("load state");
        let chore = state
            .tickets
            .iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains("ReleaseTitle"))
            .expect("chore created");

        assert!(
            chore.title().contains("Release"),
            "chore title must contain 'Release' for identification: {}",
            chore.title()
        );
    }

    /// AC1: When two milestones can be released in one pass, both are tagged.
    /// Regression: pipeline stops at the first released milestone.
    #[tokio::test]
    async fn multiple_milestones_released_in_one_pass() {
        let mut state = ProjectState {
            current_version: SemVer::new(0, 6, 0), // Exceeds both 0.5.0 and 0.6.0
            milestones: vec![
                Milestone {
                    name: "Milestone-A".to_owned(),
                    goal: "A goal".to_owned(),
                    target_version: "0.5.0".to_owned(),
                    goal_complete: true,
                    fulfilled: false,
                },
                Milestone {
                    name: "Milestone-B".to_owned(),
                    goal: "B goal".to_owned(),
                    target_version: "0.6.0".to_owned(),
                    goal_complete: true,
                    fulfilled: false,
                },
            ],
            history: vec![
                make_deploy("0.5.0", "CXA-MA01", "Ticket A", "2026-08-01"),
                make_deploy("0.6.0", "CXA-MB01", "Ticket B", "2026-08-02"),
            ],
            ..ProjectState::default()
        };
        state
            .tickets
            .push(make_ticket("CXA-MA01", "Ticket A", "desc"));
        state
            .tickets
            .push(make_ticket("CXA-MB01", "Ticket B", "desc"));

        let store = Arc::new(MemStore {
            state: Mutex::new(state),
        });
        let git = Arc::new(SpyGit {
            tags: Mutex::new(Vec::new()),
            head_sha: Mutex::new(Some("sha".to_owned())),
            existing_tags: Mutex::new(BTreeSet::new()),
        });

        let released = uc(&store, &git).execute().await.expect("pipeline runs");

        // Both milestones should be in the released list
        assert!(
            released.contains(&"Milestone-A".to_owned()),
            "milestone A should be released: {released:?}"
        );
        assert!(
            released.contains(&"Milestone-B".to_owned()),
            "milestone B should be released: {released:?}"
        );
        // Both tags should be created (scoped so the lock guard drops before the
        // store load below — the guard must not live across an await point).
        {
            let tags = git.tags.lock().expect("lock");
            assert!(
                tags.contains(&"Milestone-A".to_owned()),
                "tag for milestone A missing"
            );
            assert!(
                tags.contains(&"Milestone-B".to_owned()),
                "tag for milestone B missing"
            );
        }

        // Both milestones marked fulfilled
        let state = store.load().await.expect("load state");
        assert!(
            state.milestones[0].fulfilled,
            "milestone A not marked fulfilled"
        );
        assert!(
            state.milestones[1].fulfilled,
            "milestone B not marked fulfilled"
        );
    }

    /// AC2: Release notes should include version information from the deploy
    /// history so consumers can trace what versions contribute.
    #[tokio::test]
    async fn release_notes_include_changelog_version_information() {
        let (store, git) = seeded(
            "0.5.0",
            "VersionInNotes",
            "0.5.0",
            "Version info milestone",
            true,
            false,
            vec![
                make_deploy("0.4.5", "CXA-V001", "Earlier ticket", "2026-08-01"),
                make_deploy("0.5.0", "CXA-V002", "Current ticket", "2026-08-02"),
            ],
        );
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-V001",
            "Earlier ticket",
            "desc",
        ));
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-V002",
            "Current ticket",
            "desc",
        ));

        uc(&store, &git).execute().await.expect("pipeline runs");

        let state = store.load().await.expect("load state");
        let chore = state
            .tickets
            .iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains("Version"))
            .expect("chore created");
        let body = chore.description();

        // Verify version info is present in notes
        assert!(
            body.contains("v0.4.5") || body.contains("v0.5.0"),
            "notes must include version information: \n{body}"
        );
    }

    /// AC3: Existing tag skip must not create a duplicate release chore ticket.
    #[tokio::test]
    async fn existing_tag_does_not_create_duplicate_chore() {
        let (store, git) = seeded(
            "0.5.0",
            "DupChore",
            "0.5.0",
            "Chore duplicate milestone",
            true,
            false,
            vec![],
        );
        git.existing_tags
            .lock()
            .expect("lock")
            .insert("DupChore".to_owned());

        let initial_chore_count = store
            .state
            .lock()
            .expect("lock")
            .tickets
            .iter()
            .filter(|t| t.ticket_type() == TicketType::Chore)
            .count();

        uc(&store, &git).execute().await.expect("pipeline skips");

        let state = store.load().await.expect("load state");
        let chore_count = state
            .tickets
            .iter()
            .filter(|t| t.ticket_type() == TicketType::Chore)
            .count();
        assert_eq!(
            chore_count, initial_chore_count,
            "no duplicate chore created when tag exists"
        );
    }

    /// AC2: Version range filtering — only includes deployments at or after
    /// the last shipped version of the PREVIOUS milestone.
    #[tokio::test]
    async fn release_notes_exclude_deployment_before_prior_milestone() {
        // Prior milestone "Alpha" shipped at 0.4.0. Current: milestone "Beta"
        // at 0.5.0, current version 0.5.2.
        // Deploy at 0.3.0 (before Alpha) must NOT be in Beta's notes.
        let (store, git) = seeded(
            "0.5.2",
            "BetaExclude",
            "0.5.0",
            "Beta must exclude prior",
            true,
            false,
            vec![
                make_deploy("0.3.0", "CXA-BE01", "Old deploy", "2026-06-01"),
                make_deploy("0.4.0", "CXA-BE02", "Alpha deploy", "2026-07-01"),
                make_deploy("0.5.0", "CXA-BE03", "Beta deploy", "2026-08-01"),
            ],
        );
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-BE01",
            "Old deploy",
            "desc",
        ));
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-BE02",
            "Alpha deploy",
            "desc",
        ));
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-BE03",
            "Beta deploy",
            "desc",
        ));

        uc(&store, &git).execute().await.expect("pipeline runs");

        let state = store.load().await.expect("load state");
        let chore = state
            .tickets
            .iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains("BetaExclude"))
            .expect("chore created");
        let body = chore.description();

        assert!(
            !body.contains("CXA-BE01"),
            "notes must NOT include deploy from before prior milestone: \n{body}"
        );
    }

    /// AC4: If milestone goal is NOT complete (even if version reached), no
    /// release is triggered.
    #[tokio::test]
    async fn no_release_when_goal_incomplete_even_at_target_version() {
        // Version at target but goal NOT complete → should skip
        let (store, git) = seeded(
            "0.5.0",
            "GoalIncomplete",
            "0.5.0",
            "Incomplete goal milestone",
            false, // goal NOT complete
            false,
            vec![make_deploy("0.5.0", "CXA-GI01", "Ticket", "2026-08-01")],
        );
        store
            .state
            .lock()
            .expect("lock")
            .tickets
            .push(make_ticket("CXA-GI01", "Ticket", "desc"));

        let released = uc(&store, &git).execute().await.expect("pipeline runs");

        assert!(
            released.is_empty(),
            "no release should occur when goal incomplete, even at target version"
        );
        assert!(
            git.tags.lock().expect("lock").is_empty(),
            "no tag created when goal incomplete"
        );
    }

    /// AC1: Major version release — when target version crosses a major
    /// boundary (e.g. 1.0.0), the pipeline still works correctly.
    #[tokio::test]
    async fn releases_major_version_milestone_correctly() {
        // Target: 1.0.0 (major release)
        let (store, git) = seeded(
            "1.0.0",
            "MajorRelease",
            "1.0.0",
            "Major release milestone",
            true,
            false,
            vec![make_deploy(
                "1.0.0",
                "CXA-MJ01",
                "Major ticket",
                "2026-08-01",
            )],
        );
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-MJ01",
            "Major ticket",
            "desc",
        ));

        let released = uc(&store, &git)
            .execute()
            .await
            .expect("pipeline handles major versions");

        assert!(
            released.contains(&"MajorRelease".to_owned()),
            "major version release should be triggered"
        );
        assert_eq!(
            *git.tags.lock().expect("lock"),
            vec!["MajorRelease".to_owned()],
            "tag created for major version"
        );
    }

    /// AC4: Release chore ticket ID format must be valid and reference the
    /// milestone name.
    #[tokio::test]
    async fn release_chore_ticket_id_format_is_valid_with_milestone_reference() {
        let milestone_name = "ValidIDFMT";
        let (store, git) = seeded(
            "0.5.0",
            milestone_name,
            "0.5.0",
            "Valid ID milestone",
            true,
            false,
            vec![make_deploy("0.5.0", "CXA-VID01", "Ticket", "2026-08-01")],
        );
        store
            .state
            .lock()
            .expect("lock")
            .tickets
            .push(make_ticket("CXA-VID01", "Ticket", "desc"));

        let _ = uc(&store, &git).execute().await.expect("pipeline runs");

        let state = store.load().await.expect("load state");
        let chore = state
            .tickets
            .iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains(milestone_name))
            .expect("chore created");

        // Ticket ID must contain the milestone name
        let id_str = chore.id().to_string();
        assert!(
            id_str.contains(milestone_name),
            "chore ID must reference milestone name: {id_str}"
        );
        // ID must be a valid TicketId (the ticket was successfully created)
        assert!(id_str.len() > 1, "chore ID must not be trivially short");
    }

    /// AC2: Release notes must list changelog entries with the ticket ID prefix
    /// to identify which ticket each entry belongs to.
    #[tokio::test]
    async fn release_notes_list_ticket_ids_prefix_for_changelog_entries() {
        let (store, git) = seeded(
            "0.5.0",
            "PrefixList",
            "0.5.0",
            "Prefix list milestone",
            true,
            false,
            vec![
                make_deploy("0.5.0", "CXA-PL001", "Feature alpha", "2026-08-01"),
                make_deploy("0.5.0", "CXA-PL002", "Feature beta", "2026-08-02"),
            ],
        );
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-PL001",
            "Feature alpha",
            "desc",
        ));
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-PL002",
            "Feature beta",
            "desc",
        ));

        uc(&store, &git).execute().await.expect("pipeline runs");

        let state = store.load().await.expect("load state");
        let chore = state
            .tickets
            .iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains("Prefix"))
            .expect("chore created");
        let body = chore.description();

        // Both IDs must appear in the notes
        assert!(
            body.contains("CXA-PL001"),
            "changelog must include ticket CXA-PL001: \n{body}"
        );
        assert!(
            body.contains("CXA-PL002"),
            "changelog must include ticket CXA-PL002: \n{body}"
        );
    }

    /// AC1: Pipeline must not create a tag when goal is complete but version
    /// is below target by even 1 patch. Regression: versions like 0.4.999
    /// might incorrectly be considered equal to 0.5.0.
    #[tokio::test]
    async fn no_tag_when_version_just_below_target_by_patch() {
        // 0.4.999 is still below 0.5.0
        let (store, git) = seeded(
            "0.4.9",
            "BelowTarget",
            "0.5.0",
            "Below milestone",
            true,
            false,
            vec![],
        );

        let _ = uc(&store, &git).execute().await.expect("pipeline runs");

        assert!(
            git.tags.lock().expect("lock").is_empty(),
            "no tag when version is 0.4.9 against 0.5.0 target"
        );
    }

    /// AC4: Release chore created for version 0.1.0 (low minor version) works
    /// correctly — not confused with 1.0.0.
    #[tokio::test]
    async fn release_for_low_version_does_not_confuse_with_one_zero_zero() {
        let (store, git) = seeded(
            "0.1.0",
            "LowVersion",
            "0.1.0",
            "Low version milestone",
            true,
            false,
            vec![make_deploy(
                "0.1.0",
                "CXA-LV01",
                "Low version ticket",
                "2026-08-01",
            )],
        );
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-LV01",
            "Low version ticket",
            "desc",
        ));

        let released = uc(&store, &git)
            .execute()
            .await
            .expect("pipeline handles 0.1.0 correctly");

        assert!(
            released.contains(&"LowVersion".to_owned()),
            "release at 0.1.0 target works"
        );
        assert!(
            !git.tags.lock().expect("lock").is_empty(),
            "tag created for 0.1.0 release"
        );
    }

    /// AC2: Release notes format must be human-readable (not raw JSON or
    /// machine format). Consumers should parse them.
    #[tokio::test]
    async fn release_notes_are_human_readable_format() {
        let (store, git) = seeded(
            "0.5.0",
            "HumanReadable",
            "0.5.0",
            "Human readable milestone",
            true,
            false,
            vec![make_deploy(
                "0.5.0",
                "CXA-HR01",
                "Human ticket",
                "2026-08-01",
            )],
        );
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-HR01",
            "Human ticket",
            "desc",
        ));

        uc(&store, &git).execute().await.expect("pipeline runs");

        let state = store.load().await.expect("load state");
        let chore = state
            .tickets
            .iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains("Human"))
            .expect("chore created");
        let body = chore.description();

        // Should not be raw JSON
        assert!(
            serde_json::from_str::<serde_json::Value>(body).is_err(),
            "release notes should not be raw JSON: {}",
            body.chars().take(200).collect::<String>()
        );
        // Should contain the ticket title (readable text)
        assert!(
            body.contains("Human ticket"),
            "notes should be human-readable: {}",
            body.chars().take(200).collect::<String>()
        );
    }

    /// AC4: When the same milestone is retried (e.g., restart mid-pipeline),
    /// the fulfilled flag prevents duplicate work.
    #[tokio::test]
    async fn fulfilled_milestone_is_idempotent() {
        let milestones = vec![Milestone {
            name: "IdempotentTest".to_owned(),
            goal: "Idempotent".to_owned(),
            target_version: "0.5.0".to_owned(),
            goal_complete: true,
            fulfilled: true, // Already fulfilled
        }];

        let mut state = ProjectState {
            current_version: SemVer::new(0, 5, 0),
            milestones,
            history: vec![make_deploy("0.5.0", "CXA-IP01", "Ticket", "2026-08-01")],
            ..ProjectState::default()
        };
        state
            .tickets
            .push(make_ticket("CXA-IP01", "Ticket", "desc"));

        let store = Arc::new(MemStore {
            state: Mutex::new(state),
        });
        let git = Arc::new(SpyGit {
            tags: Mutex::new(vec!["IdempotentTest".to_owned()]),
            head_sha: Mutex::new(Some("sha".to_owned())),
            existing_tags: Mutex::new(BTreeSet::new()),
        });

        let released = uc(&store, &git)
            .execute()
            .await
            .expect("pipeline idempotent on already-fulfilled");

        // Should not re-release
        assert!(
            released.is_empty(),
            "already-fulfilled milestone should not re-release: {released:?}"
        );
    }

    /// AC2: Release notes must not contain security-sensitive information from
    /// ticket descriptions.
    #[tokio::test]
    async fn release_notes_do_not_leak_sensitive_ticket_content() {
        let sensitive_string = "aws_secret_key=AKIAIOSFFDUEFAHFHFHF";
        let (store, git) = seeded(
            "0.5.0",
            "Sensitive",
            "0.5.0",
            "Sensitive content milestone",
            true,
            false,
            vec![make_deploy(
                "0.5.0",
                "CXA-SN01",
                "Secure ticket",
                "2026-08-01",
            )],
        );
        // Ticket body contains sensitive text
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-SN01",
            "Secure ticket",
            sensitive_string,
        ));

        uc(&store, &git).execute().await.expect("pipeline runs");

        let state = store.load().await.expect("load state");
        let chore = state
            .tickets
            .iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains("Sensitive"))
            .expect("chore created");
        let body = chore.description();

        // Release notes should not contain the full ticket body with secrets
        // (they should only contain deploy history info like title, version, ID)
        assert!(
            !body.contains("aws_secret_key"),
            "secrets should not appear in release notes: {}",
            body.chars().take(200).collect::<String>()
        );
    }

    /// AC1: When milestone description contains special characters, tag name
    /// is still created correctly.
    #[tokio::test]
    async fn handles_special_characters_in_milestone_data() {
        // Test with special chars in milestone goal (NOT name — name is the tag)
        let (store, git) = seeded(
            "0.5.0",
            "SpecialChars",
            "0.5.0",
            "Goal with 'quotes', \"double quotes\", & special chars <brackets>",
            true,
            false,
            vec![make_deploy(
                "0.5.0",
                "CXA-SC01",
                "Special ticket",
                "2026-08-01",
            )],
        );
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-SC01",
            "Special ticket",
            "desc",
        ));

        let released = uc(&store, &git)
            .execute()
            .await
            .expect("pipeline handles special characters");

        assert!(
            released.contains(&"SpecialChars".to_owned()),
            "release with special chars in goal should work"
        );
    }

    /// AC3: Multiple tags existing — pipeline skips all found, logs all skips.
    #[tokio::test]
    async fn multiple_existing_tags_all_skipped_with_activity() {
        let milestones = vec![
            Milestone {
                name: "Exist-A".to_owned(),
                goal: "A".to_owned(),
                target_version: "0.5.0".to_owned(),
                goal_complete: true,
                fulfilled: false,
            },
            Milestone {
                name: "Exist-B".to_owned(),
                goal: "B".to_owned(),
                target_version: "0.5.0".to_owned(),
                goal_complete: true,
                fulfilled: false,
            },
        ];

        let state = ProjectState {
            current_version: SemVer::new(0, 5, 0),
            milestones,
            history: vec![],
            ..ProjectState::default()
        };

        let store = Arc::new(MemStore {
            state: Mutex::new(state),
        });
        let git = Arc::new(SpyGit {
            tags: Mutex::new(Vec::new()),
            head_sha: Mutex::new(Some("sha".to_owned())),
            existing_tags: Mutex::new({
                let mut s = BTreeSet::new();
                s.insert("Exist-A".to_owned());
                s.insert("Exist-B".to_owned());
                s
            }),
        });

        uc(&store, &git).execute().await.expect("pipeline runs");

        // Both should be skipped, no tags created
        assert!(
            git.tags.lock().expect("lock").is_empty(),
            "no new tags when all exist"
        );

        // Both should have activity entries
        let state = store.load().await.expect("load state");
        let activity_count = state
            .activity
            .iter()
            .filter(|e| e.action.contains("already exists"))
            .count();
        assert!(
            activity_count >= 2,
            "should have activity entries for each skipped release, got {activity_count}"
        );
    }

    /// AC4: Producing ticket dependencies include both Feature and Bug tickets.
    #[tokio::test]
    async fn release_chore_depends_on_features_and_bugs() {
        // Bug ticket
        let mut bug = make_ticket("CXA-BG01", "Bug fix ticket", "fix desc");
        bug.transition_to(
            coxagent_domain::Role::Sa,
            coxagent_domain::Status::InProgress,
        )
        .ok();

        let (store, git) = seeded(
            "0.5.0",
            "MixedDeps",
            "0.5.0",
            "Mixed dependencies milestone",
            true,
            false,
            vec![
                make_deploy("0.5.0", "CXA-MD01", "Feature ticket", "2026-08-01"),
                make_deploy("0.5.0", "CXA-BG01", "Bug fix ticket", "2026-08-02"),
            ],
        );

        // Seed both feature and bug into state
        {
            let mut state = store.state.lock().expect("lock");
            state
                .tickets
                .push(make_ticket("CXA-MD01", "Feature ticket", "desc"));
            state
                .tickets
                .push(make_ticket("CXA-BG01", "Bug fix ticket", "bug desc"));
        }

        uc(&store, &git).execute().await.expect("pipeline runs");

        let state = store.load().await.expect("load state");
        let chore = state
            .tickets
            .iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains("Mixed"))
            .expect("chore created");

        let deps = chore.depends_on();
        // Should depend on both producing tickets
        assert!(
            deps.iter().any(|d| d.to_string() == "CXA-MD01"),
            "chore must depend on feature producing ticket"
        );
        // Bug ticket is also a producing ticket
        assert!(
            deps.iter().any(|d| d.to_string() == "CXA-BG01"),
            "chore must depend on bug producing ticket"
        );
    }

    /// AC1: Release notes when deploy at EXACT same version as milestone
    /// target — inclusion, not exclusion.
    #[tokio::test]
    async fn release_notes_include_deploy_at_exact_target_version() {
        let (store, git) = seeded(
            "0.5.0",
            "ExactTarget",
            "0.5.0",
            "Exact target milestone",
            true,
            false,
            vec![
                // Deploy AT EXACTLY the target version
                make_deploy("0.5.0", "CXA-ET01", "At target ticket", "2026-08-01"),
            ],
        );
        store.state.lock().expect("lock").tickets.push(make_ticket(
            "CXA-ET01",
            "At target ticket",
            "desc",
        ));

        uc(&store, &git).execute().await.expect("pipeline runs");

        let state = store.load().await.expect("load state");
        let chore = state
            .tickets
            .iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains("Exact"))
            .expect("chore created");
        let body = chore.description();

        // Deploy at exact target version must be included
        assert!(
            body.contains("CXA-ET01"),
            "deploy at exact target version must be included: \n{body}"
        );
    }

    /// AC4: Release chore complexity should be appropriate (not too large for
    /// release coordination work).
    #[tokio::test]
    async fn release_chore_has_appropriate_complexity() {
        // Release chores should not be marked as Large complexity
        let (store, git) = seeded(
            "0.5.0",
            "ComplexityCheck",
            "0.5.0",
            "Complexity milestone",
            true,
            false,
            vec![make_deploy("0.5.0", "CXA-CX01", "Ticket", "2026-08-01")],
        );
        store
            .state
            .lock()
            .expect("lock")
            .tickets
            .push(make_ticket("CXA-CX01", "Ticket", "desc"));

        uc(&store, &git).execute().await.expect("pipeline runs");

        let state = store.load().await.expect("load state");
        let chore = state
            .tickets
            .iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains("Complexity"))
            .expect("chore created");

        // Release chores should not be marked Large
        assert!(
            chore.complexity() != Complexity::Large,
            "release chore should not be Large complexity"
        );
    }

    // ==================================================================
    // CXA-F008 TDD: FAILING ACCEPTANCE CRITERIA TESTS — Automated Release Pipeline
    // ==================================================================

    /// **AC1**: When the current_version meets or exceeds a milestone's target_version
    /// and the milestone goal is marked complete, the pipeline creates a git tag
    /// matching the milestone name and posts release notes sourced from deploy history.
    ///
    /// This test verifies the ATOMIC release action: both tag creation and notes
    /// posting must occur together as one pipeline execution.
    #[tokio::test]
    async fn ac1_creates_tag_and_posts_release_notes_atomically() {
        let producing_ticket_id = "CXA-AC1-A1";
        let producing_ticket_title = "Acceptor feature for AC1 verification";
        let milestone_name = "AC1-Verification";
        let target_version = "1.2.3";
        let release_notes_deploy_entry = "Shipped auth refactor for login";

        let (store, git) = seeded(
            target_version,
            milestone_name,
            target_version,
            "Verify AC1 atomically",
            true,
            false,
            vec![make_deploy(
                target_version,
                producing_ticket_id,
                release_notes_deploy_entry,
                "2026-01-15T10:00:00Z",
            )],
        );
        store.state.lock().expect("lock").tickets.push(make_ticket(
            producing_ticket_id,
            producing_ticket_title,
            "Feature implementation details",
        ));

        let released = uc(&store, &git)
            .execute()
            .await
            .expect("pipeline completes");

        // Verify the milestone was released
        assert!(
            released.contains(&milestone_name.to_owned()),
            "milestone should appear in released list: {released:?}",
        );

        // AC1 check 1: Git tag created matching milestone name. The lock is
        // taken and dropped inside a block so it is never held across the
        // `store.load().await` below.
        {
            let tags = git.tags.lock().expect("lock");
            assert_eq!(
                *tags,
                vec![milestone_name.to_owned()],
                "exactly one tag with milestone name"
            );
        }

        // AC1 check 2: Release notes posted, sourced from deploy history
        let state = store.load().await.expect("load state");
        let chore = state
            .tickets
            .iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains(milestone_name))
            .expect("release chore ticket created with milestone name in title");

        let notes = chore.description();
        assert!(
            notes.contains(producing_ticket_id),
            "release notes must include ticket ID from deploy history:\n{notes}"
        );
        assert!(
            notes.contains(release_notes_deploy_entry),
            "release notes must include changelog entry title from deploy history:\n{notes}"
        );
    }

    /// **AC2**: Release notes include all ticket IDs, titles, and the changelog entries produced
    /// during the version range from last shipped version to current.
    ///
    /// This test specifically verifies that:
    /// - All tickets from the version span are included (not partial)
    /// - Titles are present (not just IDs)
    /// - Only the version span from last shipped to current is included
    /// - Deployments before last shipped version are EXCLUDED
    #[tokio::test]
    async fn ac2_release_notes_include_complete_changelog_for_version_range() {
        let milestone_name = "AC2-Changelog";
        let target_ver = "1.1.0";
        // Create 3 deployments in range (at/above last shipped), 1 before last shipped
        // Implementation defines "last shipped version" as highest deploy below target
        let history = vec![
            make_deploy("0.9.0", "OLD-T001", "Pre-range ticket", "2026-01-01"), // EXCLUDED
            make_deploy("1.0.0", "RANGE-T001", "Last shipped ticket", "2026-02-01"), // "Last shipped"
            make_deploy("1.1.0", "RANGE-T003", "At target ticket", "2026-04-01"),    // INCLUDED
        ];

        let (store, git) = seeded(
            target_ver,
            milestone_name,
            target_ver,
            "Verify changelog completeness",
            true,
            false,
            history,
        );
        // Seed all producing tickets
        for id in &["OLD-T001", "RANGE-T001", "RANGE-T003"] {
            store
                .state
                .lock()
                .expect("lock")
                .tickets
                .push(make_ticket(id, id, "description"));
        }

        uc(&store, &git).execute().await.expect("pipeline runs");

        let state = store.load().await.expect("load state");
        let chore = state
            .tickets
            .iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains(milestone_name))
            .expect("release chore created");
        let notes = chore.description().to_string();

        // Verify tickets in range are included (from last shipped version: 1.0.0, then 1.1.0)
        // RANGE-T001 should be included because 1.0.0 is "last shipped version"
        // (highest deploy below target 1.1.0)
        assert!(
            notes.contains("RANGE-T001"),
            "notes must include ticket from last shipped version:\n{notes}"
        );

        // RANGE-T003 must be included
        assert!(
            notes.contains("RANGE-T003"),
            "notes must include ticket at target version:\n{notes}"
        );

        // Verify tickets from before last shipped are NOT included
        assert!(
            !notes.contains("OLD-T001"),
            "notes must EXCLUDE ticket before last shipped version:\n{notes}"
        );
    }

    /// **AC3**: If a tag for the milestone version already exists in the repository,
    /// the pipeline skips the release and logs a 'release already exists' activity
    /// entry rather than failing.
    ///
    /// This test verifies:
    /// - Pipeline does not fail (returns gracefully)
    /// - No duplicate tag is created
    /// - Activity entry with 'release already exists' is logged
    /// - Activity entry includes the milestone name for traceability
    /// - No new ticket/chore is created
    #[tokio::test]
    async fn ac3_existing_tag_logs_activity_and_skips_without_failing() {
        let milestone_name = "AC3-Existing";
        let (store, git) = seeded(
            "1.0.0",
            milestone_name,
            "1.0.0",
            "Skip verification",
            true,
            false,
            vec![],
        );

        // Pre-populate existing tag
        git.existing_tags
            .lock()
            .expect("lock")
            .insert(milestone_name.to_owned());

        let initial_ticket_count = store.state.lock().expect("lock").tickets.len();

        // Pipeline must NOT fail when tag exists
        let released = uc(&store, &git)
            .execute()
            .await
            .expect("pipeline must not fail when release already exists");

        // No new release should be created
        assert!(
            released.is_empty(),
            "no release created when tag exists: {released:?}"
        );

        // No new tag should be created
        assert!(
            git.tags.lock().expect("lock").is_empty(),
            "no duplicate tag created"
        );

        // Activity entry must be logged with specific text
        let state = store.load().await.expect("load state");

        // Find the activity entry
        let activity = state
            .activity
            .iter()
            .find(|e| e.action.contains("release already exists"))
            .expect("activity entry with 'release already exists' must be logged");

        // Activity must include milestone name
        assert!(
            activity.action.contains(milestone_name),
            "activity entry must include milestone name for traceability: {}",
            activity.action
        );

        // No new ticket should be created
        assert_eq!(
            state.tickets.len(),
            initial_ticket_count,
            "no new ticket created when release already exists"
        );
    }

    /// **AC4**: A Release chore ticket is created referencing the milestone, with all
    /// producing feature/bug ticket IDs as dependencies, and the milestone is marked
    /// as fulfilled.
    ///
    /// This test verifies:
    /// - Release chore ticket is created
    /// - Chore references the milestone name
    /// - Chore's depends_on contains all producing ticket IDs as formal dependencies
    /// - Milestone is marked as fulfilled
    /// - Dependencies are listed in the depends_on field, not just description text
    #[tokio::test]
    async fn ac4_creates_release_chore_with_dependencies_and_fulfills_milestone() {
        let milestone_name = "AC4-Fulfill";
        let producing_ids = vec![
            "CXA-AC4-P1", // Feature dependency
            "CXA-AC4-P2", // Another feature dependency
        ];

        let history = producing_ids
            .iter()
            .enumerate()
            .map(|(i, id)| {
                make_deploy(
                    "1.0.0",
                    id,
                    &format!("Producing ticket {} title", i + 1),
                    "2026-01-01",
                )
            })
            .collect();

        let (store, git) = seeded(
            "1.0.0",
            milestone_name,
            "1.0.0",
            "Verify milestone fulfillment",
            true,
            false,
            history,
        );

        // Seed all producing tickets
        for id in &producing_ids {
            store
                .state
                .lock()
                .expect("lock")
                .tickets
                .push(make_ticket(id, id, "description"));
        }

        uc(&store, &git).execute().await.expect("pipeline runs");

        let state = store.load().await.expect("load state");

        // Release chore must be created
        let chore = state
            .tickets
            .iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains(milestone_name))
            .expect("Release chore ticket must be created");

        // Milestone must be marked as fulfilled
        assert!(
            state.milestones[0].fulfilled,
            "milestone must be marked as fulfilled"
        );

        // All producing ticket IDs must be in depends_on field
        let deps = chore.depends_on();
        for id in &producing_ids {
            assert!(
                deps.iter().any(|d| d.to_string() == *id),
                "chore must have {} in depends_on field: {:?}",
                id,
                deps.iter().map(ToString::to_string).collect::<Vec<_>>()
            );
        }
    }
}
