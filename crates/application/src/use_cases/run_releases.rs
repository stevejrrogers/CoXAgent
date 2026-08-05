//! `RunReleasesUseCase` — the automated release pipeline.
//!
//! Runs at the end of each cycle to check whether any milestone has been
//! met — current version at or past the target and the goal complete — and
//! if so, tags the release, posts notes sourced from deploy history, and
//! creates a Release chore ticket. Skips milestones already released.

use crate::error::AppError;
use crate::ports::outbound::{GitPort, StateStorePort};
use std::path::PathBuf;
use std::sync::Arc;

/// Triggers releases for any milestone the project has reached.
pub struct RunReleasesUseCase<S: StateStorePort> {
    store: Arc<S>,
    git: Option<Arc<dyn GitPort>>,
    work_dir: PathBuf,
}

impl<S: StateStorePort> RunReleasesUseCase<S> {
    pub fn new(store: Arc<S>, work_dir: PathBuf) -> Self {
        Self {
            store,
            git: None,
            work_dir,
        }
    }

    /// Attach the git backend used to create release tags and check for
    /// existing tags. Absent, the pipeline safely no-ops (never tags blindly).
    #[must_use]
    pub fn with_git(mut self, git: Option<Arc<dyn GitPort>>) -> Self {
        self.git = git;
        self
    }

    /// Execute the release pipeline: check milestones, tag releases, and
    /// create release chore tickets. Returns a list of milestone names that
    /// were released (or skipped).
    ///
    /// # Errors
    /// [`AppError`] on state or git failures.
    pub async fn execute(&self) -> Result<Vec<String>, AppError> {
        use crate::PortError;
        use coxagent_domain::{Complexity, Priority, SemVer, Ticket, TicketId, TicketType};

        // No git backend wired → cannot create tags; no-op safely rather than
        // guess. Milestones stay unfulfilled and will be released later.
        let Some(git) = self.git.as_ref() else {
            return Ok(Vec::new());
        };

        let mut state = self.store.load().await?;
        let mut released = Vec::new();
        let milestones = state.milestones.clone();
        for m in milestones {
            // Already released this milestone on a prior run? Idempotent skip.
            if m.fulfilled {
                continue;
            }
            // Gate 1: scope ready to ship (set explicitly — not derived).
            if !m.goal_complete {
                continue;
            }
            // Gate 2: version reached the target for this milestone.
            let Ok(target) = SemVer::parse(&m.target_version) else {
                continue;
            };
            if state.current_version < target {
                continue;
            }
            // Gate 3: a tag for this milestone already exists (e.g. it was
            // released out of band, or a prior run tagged it). Log and skip —
            // never duplicate the tag or the release chore.
            if git.tag_exists(&self.work_dir, &m.name).await {
                state.log_activity(
                    "RELEASE",
                    &format!("release for '{}' already exists", m.name),
                    None,
                );
                continue;
            }

            // Create the annotated tag on the current tree.
            git.create_tag(&self.work_dir, &m.name, "HEAD")
                .await
                .map_err(|e| {
                    AppError::from(PortError::Backend(format!(
                        "create tag for '{}': {e}",
                        m.name
                    )))
                })?;

            // Release notes sourced from deploy history: who shipped into this
            // milestone and at what version. Producing tickets are named so the
            // chore's description references them (test/audit trail).
            let notes: Vec<String> = state
                .history
                .iter()
                .filter(|h| h.version >= target)
                .map(|h| format!("{} — {} (v{})", h.ticket, h.title, h.version))
                .collect();
            let notes_txt = if notes.is_empty() {
                format!("Release {} (v{}).", m.name, state.current_version)
            } else {
                notes.join("\n")
            };

            let chore_id = TicketId::new(format!("REL-{}", &m.name))
                .map_err(|e| AppError::Domain(coxagent_domain::error::DomainError::from(e)));
            let chore_id = chore_id?;
            let chore = Ticket::new(
                chore_id,
                TicketType::Chore,
                format!("Release {}", m.name),
                notes_txt,
                Priority::Medium,
                Complexity::Small,
                false,
            )
            .map_err(AppError::Domain)?;
            state.tickets.push(chore);

            // Mark the milestone fulfilled so restarts/retries don't re-release.
            if let Some(mm) = state.milestones.iter_mut().find(|mm| mm.name == m.name) {
                mm.fulfilled = true;
            }
            released.push(m.name.clone());
        }

        // Always persist: the tag-exists path logs activity (and skip paths may
        // have logged) even when nothing new is released — an in-memory-only
        // activity entry would be lost and the "already exists" audit trail
        // never recorded.
        self.store.save(&state).await?;
        Ok(released)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::outbound::GitAuthor;
    use crate::state::{DeployRecord, Milestone, ProjectState};
    use crate::PortError;
    use async_trait::async_trait;
    use coxagent_domain::{Complexity, Priority, SemVer, TicketId, TicketType};
    use std::collections::BTreeSet;
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
            .with_git(Some(git.clone() as Arc<dyn GitPort>))
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
}
