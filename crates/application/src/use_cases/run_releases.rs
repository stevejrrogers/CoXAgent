//! `RunReleasesUseCase` — the automated release pipeline.
//!
//! Runs at the end of each cycle to check whether any milestone has been
//! met — current version at or past the target and the goal complete — and
//! if so, tags the release, posts notes sourced from deploy history, and
//! creates a Release chore ticket. Skips milestones already released.

use crate::config::Config;
use crate::error::AppError;
use crate::ports::outbound::{GitAuthor, GitPort, StateStorePort};
use std::path::PathBuf;
use std::sync::Arc;

/// Triggers releases for any milestone the project has reached.
pub struct RunReleasesUseCase<S: StateStorePort> {
    store: Arc<S>,
    git: Option<Arc<dyn GitPort>>,
    work_dir: PathBuf,
    config: Config,
}

impl<S: StateStorePort> RunReleasesUseCase<S> {
    pub fn new(store: Arc<S>, work_dir: PathBuf) -> Self {
        Self {
            store,
            git: None,
            work_dir,
            config: Config::default(),
        }
    }

    /// Attach the git backend used to create release tags and check for
    /// existing tags. Absent, the pipeline safely no-ops (never tags blindly).
    #[must_use]
    pub fn with_git(mut self, git: Option<Arc<dyn GitPort>>) -> Self {
        self.git = git;
        self
    }

    /// Attach the project configuration. The pipeline only runs when
    /// `config.releases.enabled` is set — tagging mutates the codebase's git
    /// history, so it is an opt-in, never a default.
    #[must_use]
    pub fn with_config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }

    /// The committed identity release tags are created under. Prefers the
    /// project's configured `git.commit_email` when set, else the shared bot
    /// address — same convention as the forge commit path. Supplies the
    /// committer inline at tag-creation time, so the release never depends on a
    /// machine's ambient git config.
    fn release_author(&self) -> GitAuthor {
        let email = if self.config.git.commit_email.trim().is_empty() {
            "coxagent-bot@users.noreply.github.com".to_owned()
        } else {
            self.config.git.commit_email.clone()
        };
        GitAuthor {
            name: "coxagent-bot".to_owned(),
            email,
        }
    }

    /// Execute the release pipeline: check milestones, tag releases, and
    /// create release chore tickets. Returns a list of milestone names that
    /// were released (or skipped).
    ///
    /// # Errors
    /// [`AppError`] on state or git failures.
    pub async fn execute(&self) -> Result<Vec<String>, AppError> {
        use crate::PortError;
        use coxagent_domain::{Complexity, Priority, Role, SemVer, Ticket, TicketId, TicketType};

        // Opt-in gate: automated tagging mutates the managed codebase's git
        // history, so the pipeline is inert until `releases.enabled`.
        if !self.config.releases.enabled {
            return Ok(Vec::new());
        }

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
                    &format!("release already exists for '{}' — skipping", m.name),
                    None,
                );
                continue;
            }

            // Create the annotated tag on the current tree.
            git.create_tag(&self.work_dir, &m.name, "HEAD", &self.release_author())
                .await
                .map_err(|e| {
                    AppError::from(PortError::Backend(format!(
                        "create tag for '{}': {e}",
                        m.name
                    )))
                })?;

            // Release notes sourced from deploy history: who shipped into this
            // milestone and at what version. Producing tickets are named so the
            // chore's description references them (test/audit trail). The notes
            // cover the version span from the last shipped version (the highest
            // deploy below the target — inclusive) up to the current version,
            // not the whole deploy history.
            let last_shipped = state
                .history
                .iter()
                .map(|h| h.version.clone())
                .filter(|v| *v < target)
                .max()
                .unwrap_or(target);
            let producing: Vec<(TicketId, String)> = state
                .history
                .iter()
                .filter(|h| h.version >= last_shipped)
                .map(|h| {
                    (
                        h.ticket.clone(),
                        format!("{} — {} (v{})", h.ticket, h.title, h.version),
                    )
                })
                .collect();
            let notes_txt = if producing.is_empty() {
                format!("Release {} (v{}).", m.name, state.current_version)
            } else {
                producing
                    .iter()
                    .map(|(_, line)| line.clone())
                    .collect::<Vec<_>>()
                    .join("\n")
            };

            let chore_id = TicketId::new(format!("REL-{}", m.name)).map_err(AppError::Domain)?;
            let chore_id_str = chore_id.to_string();
            let mut chore = Ticket::new(
                chore_id,
                TicketType::Chore,
                format!("Release {}", m.name),
                notes_txt.clone(),
                Priority::Medium,
                Complexity::Small,
                false,
            )
            .map_err(AppError::Domain)?;
            // The chore formally depends on every producing ticket shipped into
            // this version span (the `depends_on` linkage, not just prose in the
            // description).
            for (id, _) in &producing {
                chore
                    .add_dependency(Role::System, id.clone())
                    .map_err(AppError::Domain)?;
            }
            state.tickets.push(chore);

            // Post the notes to the team room, the same channel the PO uses to
            // announce milestone plans — so the release is visible in-app, not
            // only as an activity entry.
            state.post_comment("RELEASE", &notes_txt, Some(chore_id_str));

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
