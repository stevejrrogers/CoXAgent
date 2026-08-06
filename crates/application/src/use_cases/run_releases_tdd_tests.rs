//! TDD tests for CXA-F008: Automated Release Pipeline
//!
//! These failing tests encode the EXACT acceptance criteria and will fail
//! until the implementation satisfies each one.

#[cfg(test)]
mod tdd_tests {
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
        state: Arc<Mutex<ProjectState>>,
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
        tags_created: Arc<Mutex<Vec<String>>>,
        head_sha: String,
        existing_tags: Arc<Mutex<BTreeSet<String>>>,
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
        async fn sync_base(&self, _: &std::path::Path, _: &str) -> Result<crate::ports::outbound::SyncBase, PortError> {
            Ok(crate::ports::outbound::SyncBase::UpToDate)
        }
        async fn abort_merge(&self, _: &std::path::Path) -> Result<(), PortError> {
            Ok(())
        }
        async fn head_sha(&self, _: &std::path::Path) -> Result<String, PortError> {
            Ok(self.head_sha.clone())
        }
        async fn update_ref(&self, _: &std::path::Path, _refname: &str, _sha: &str) -> Result<(), PortError> {
            Ok(())
        }
        async fn worktree_add(&self, _: &std::path::Path, _path: &std::path::Path, _sha: &str) -> Result<(), PortError> {
            Ok(())
        }
        async fn worktree_remove(&self, _: &std::path::Path, _path: &std::path::Path) -> Result<(), PortError> {
            Ok(())
        }
        async fn changed_paths(&self, _: &std::path::Path, _from: &str, _to: &str) -> Result<Vec<String>, PortError> {
            Ok(Vec::new())
        }
        async fn create_tag(&self, _work_dir: &std::path::Path, name: &str, _ref_target: &str, _author: &GitAuthor) -> Result<(), PortError> {
            self.tags_created.lock().expect("lock").push(name.to_owned());
            Ok(())
        }
        async fn tag_exists(&self, _work_dir: &std::path::Path, name: &str) -> bool {
            self.existing_tags.lock().expect("lock").contains(name)
        }
    }

    fn uk(store: Arc<MemStore>, git: Arc<SpyGit>) -> RunReleasesUseCase<MemStore> {
        RunReleasesUseCase::new(Arc::clone(&store), PathBuf::from("/tmp"))
            .with_git(Some(git as Arc<dyn GitPort>))
            .with_config(Config {
                releases: ReleasesConfig { enabled: true },
                ..Config::default()
            })
    }

    fn mk_ticket(id: &str, title: &str, desc: &str) -> coxagent_domain::Ticket {
        coxagent_domain::Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Feature,
            title,
            desc,
            Priority::Medium,
            Complexity::Medium,
            false,
        )
        .expect("ticket")
    }

    fn mk_deploy(ver: &str, tid: &str, title: &str, ts: &str) -> DeployRecord {
        DeployRecord {
            version: SemVer::parse(ver).unwrap(),
            ticket: TicketId::new(tid).unwrap(),
            title: title.to_owned(),
            at: ts.to_owned(),
        }
    }

    fn setup(ver: &str, m: &Milestone, hs: Vec<DeployRecord>, ts: Vec<coxagent_domain::Ticket>) -> (Arc<MemStore>, Arc<SpyGit>) {
        let state = ProjectState {
            current_version: SemVer::parse(ver).unwrap(),
            milestones: vec![m.clone()],
            history: hs,
            tickets: ts,
            ..ProjectState::default()
        };
        (
            Arc::new(MemStore { state: Arc::new(Mutex::new(state)) }),
            Arc::new(SpyGit {
                tags_created: Arc::new(Mutex::new(Vec::new())),
                head_sha: "def456".to_owned(),
                existing_tags: Arc::new(Mutex::new(BTreeSet::new())),
            }),
        )
    }

    fn milestone(v: &str, name: &str, goal: &str, complete: bool, fulfilled: bool) -> Milestone {
        Milestone {
            name: name.to_owned(),
            goal: goal.to_owned(),
            target_version: v.to_owned(),
            goal_complete: complete,
            fulfilled,
        }
    }

    // =========================================================================
    // AC1: Tag creation when milestone reached, goal complete
    // =========================================================================

    #[tokio::test]
    async fn ac1_creates_tag_at_exact_target() {
        let ver = "1.0.0";
        let name = "AC1Target";
        let (store, git) = setup(ver, &milestone(ver, name, "Goal", true, false),
            vec![mk_deploy(ver, "CXA-A1", "Ticket", "2026-08-01")],
            vec![mk_ticket("CXA-A1", "Ticket", "desc")]
        );

        let released = uk(store.clone(), git.clone())
            .execute().await.expect("pipeline runs");

        assert!(released.contains(&name.to_string()),
            "milestone released: {released:?}");
        assert_eq!(*git.tags_created.lock().expect("lock"),
            vec![name.to_string()], "tag created");
    }

    #[tokio::test]
    async fn ac1_creates_tag_when_version_exceeds_target() {
        let (store, git) = setup("1.2.0",
            &milestone("1.0.0", "AC1Exceeds", "Goal", true, false),
            vec![mk_deploy("1.2.0", "CXA-A2", "Feature", "2026-08-01")],
            vec![mk_ticket("CXA-A2", "Feature", "desc")]
        );

        let released = uk(store.clone(), git.clone())
            .execute().await.expect("pipeline runs");

        assert!(released.contains(&"AC1Exceeds".to_string()));
        assert!(!git.tags_created.lock().expect("lock").is_empty(),
            "tag created above target");
    }

    #[tokio::test]
    async fn ac1_skips_when_goal_not_complete() {
        let (store, git) = setup("1.0.0",
            &milestone("1.0.0", "NoGoal", "Goal", false, false),
            vec![], vec![]
        );

        let released = uk(store.clone(), git.clone())
            .execute().await.expect("pipeline runs");

        assert!(released.is_empty(), "no release when goal incomplete");
        assert!(git.tags_created.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn ac1_skips_when_version_below_target() {
        let (store, git) = setup("0.9.0",
            &milestone("1.0.0", "BelowVer", "Goal", true, false),
            vec![], vec![]
        );

        let released = uk(store.clone(), git.clone())
            .execute().await.expect("pipeline runs");

        assert!(released.is_empty(), "no release below target");
        assert!(git.tags_created.lock().expect("lock").is_empty());
    }

    // =========================================================================
    // AC2: Release notes completeness  
    // =========================================================================

    #[tokio::test]
    async fn ac2_notes_include_all_tickets_from_last_shipped_to_current() {
        let (store, git) = setup("1.1.0",
            &milestone("1.1.0", "AC2Range", "Goal", true, false),
            vec![
                mk_deploy("1.0.0", "CXA-LS", "Last ticket", "2026-06-01T00:00:00Z"),
                mk_deploy("1.1.0", "CXA-CC", "Current ticket", "2026-08-01T00:00:00Z"),
            ],
            vec![
                mk_ticket("CXA-LS", "Last ticket", "desc"),
                mk_ticket("CXA-CC", "Current ticket", "desc"),
            ]
        );

        uk(store.clone(), git.clone())
            .execute().await.expect("pipeline runs");

        let state = store.load().await.expect("load state");
        let chore = state.tickets.iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains("AC2Range"))
            .expect("chore created");

        let notes = chore.description();
        assert!(notes.contains("CXA-LS"), "include last shipped: {notes}");
        assert!(notes.contains("CXA-CC"), "include current: {notes}");
    }

    #[tokio::test]
    async fn ac2_notes_include_ticket_titles_not_just_ids() {
        let deploy_title = "The exact deploy title to appear";
        let (store, git) = setup("1.0.0",
            &milestone("1.0.0", "AC2Titles", "Goal", true, false),
            vec![mk_deploy("1.0.0", "CXA-T1", deploy_title, "2026-08-01")],
            vec![mk_ticket("CXA-T1", deploy_title, "different desc")]
        );

        uk(store.clone(), git.clone())
            .execute().await.expect("pipeline runs");

        let state = store.load().await.expect("load state");
        let chore = state.tickets.iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains("AC2Titles"))
            .expect("chore created");

        assert!(chore.description().contains(deploy_title),
            "notes must include deploy title: {}", chore.description());
    }

    // =========================================================================
    // AC3: Skip existing tag with activity entry
    // =========================================================================

    #[tokio::test]
    async fn ac3_skips_existing_tag_and_logs_activity() {
        let ver = "1.0.0";
        let name = "AC3Existing";
        let (store, git) = setup(ver,
            &milestone(ver, name, "Exists", true, false),
            vec![], vec![]
        );

        git.existing_tags.lock().expect("lock")
            .insert(name.to_string());

        let released = uk(store.clone(), git.clone())
            .execute().await.expect("pipeline must not fail");

        assert!(released.is_empty(), "no release created");

        let state = store.load().await.expect("load state");
        let ae = state.activity.iter()
            .find(|e| e.action.contains("release already exists"));
        assert!(ae.is_some(), "activity must be logged with 'release already exists'");
        assert_eq!(
            state.tickets.len(), 0,
            "no new ticket when tag exists"
        );
    }

    // =========================================================================
    // AC4: Release chore with dependencies, milestone fulfilled
    // =========================================================================

    #[tokio::test]
    async fn ac4_creates_chore_with_dependencies_and_fulfills_milestone() {
        let history = vec![
            mk_deploy("1.0.0", "CXA-P1", "Feature 1", "2026-08-01"),
            mk_deploy("1.0.0", "CXA-P2", "Feature 2", "2026-08-02"),
        ];
        let tickets = vec![
            mk_ticket("CXA-P1", "Feature 1", "desc"),
            mk_ticket("CXA-P2", "Feature 2", "desc"),
        ];
        let (store, git) = setup("1.0.0",
            &milestone("1.0.0", "AC4Fulfill", "Fulfill", true, false),
            history, tickets
        );

        uk(store.clone(), git.clone())
            .execute().await.expect("pipeline runs");

        let state = store.load().await.expect("load state");
        let chore = state.tickets.iter()
            .find(|t| t.ticket_type() == TicketType::Chore && t.title().contains("AC4Fulfill"))
            .expect("release chore created");

        assert!(state.milestones[0].fulfilled,
            "milestone must be fulfilled");

        let deps = chore.depends_on();
        assert!(deps.iter().any(|d| d.to_string() == "CXA-P1"),
            "chore depends on CXA-P1");
        assert!(deps.iter().any(|d| d.to_string() == "CXA-P2"),
            "chore depends on CXA-P2");
    }

    // =========================================================================
    // Additional edge cases
    // =========================================================================

    #[tokio::test]
    async fn ac_handles_empty_deploy_history() {
        let (store, git) = setup("1.0.0",
            &milestone("1.0.0", "NoHistory", "Empty", true, false),
            vec![], vec![]
        );

        let _ = uk(store.clone(), git.clone())
            .execute().await.expect("must handle empty history");

        assert!(
            git.tags_created.lock().expect("lock")
                .contains(&"NoHistory".to_string()),
            "tag created with empty history"
        );
    }

    #[tokio::test]
    async fn ac_releases_multiple_milestones_in_one_pass() {
        let state = ProjectState {
            current_version: SemVer::new(1, 2, 0),
            milestones: vec![
                milestone("1.0.0", "M1", "G1", true, false),
                milestone("1.1.0", "M2", "G2", true, false),
            ],
            history: vec![
                mk_deploy("1.0.0", "CXA-M1", "Ticket1", "2026-08-01"),
                mk_deploy("1.1.0", "CXA-M2", "Ticket2", "2026-08-02"),
            ],
            tickets: vec![
                mk_ticket("CXA-M1", "Ticket1", "desc"),
                mk_ticket("CXA-M2", "Ticket2", "desc"),
            ],
            ..ProjectState::default()
        };

        let store = Arc::new(MemStore {
            state: Arc::new(Mutex::new(state.clone())),
        });
        let git = Arc::new(SpyGit {
            tags_created: Arc::new(Mutex::new(Vec::new())),
            head_sha: "abc".to_owned(),
            existing_tags: Arc::new(Mutex::new(BTreeSet::new())),
        });

        let released = uk(store.clone(), git.clone())
            .execute().await.expect("pipeline releases all");

        assert!(released.contains(&"M1".to_string()),
            "first milestone released: {released:?}");
        assert!(released.contains(&"M2".to_string()),
            "second milestone released: {released:?}");

        let tags = git.tags_created.lock().expect("lock");
        assert!(tags.contains(&"M1".to_string()), "M1 tag present");
        assert!(tags.contains(&"M2".to_string()), "M2 tag present");
    }
}
