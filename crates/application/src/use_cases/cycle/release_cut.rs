// Part of the cycle module split by concern — see cycle/mod.rs.
//! The release cut: the ONLY place the product version moves.
//!
//! On a configurable cadence the leader scans conventional-commit subjects
//! since the last `v*` tag, decides the SemVer bump (any `feat` → minor, else
//! fixes/chores → patch; MAJOR is never automated — breaking with users is a
//! human decision), and opens a release PR that edits nothing but the version
//! in `Cargo.toml`/`Cargo.lock` plus a changelog note. `Cargo.toml` is on the
//! human-eyes sensitive list, so a person lands the release from the Inbox —
//! one deliberate click. After it merges, the merged-PR sync tags `vX.Y.Z`.
//!
//! Everything here is orchestration over ports (git raw, workspace files);
//! the pure decisions live at the bottom with tests.

use super::RunCycleUseCase;
use crate::ports::outbound::{AgentEnginePort, StateStorePort};
use coxagent_domain::{Bump, SemVer};

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// One release-cut consideration. Leader-only; gated by
    /// `releases.cut_every_days` (0 = off) and a date-stamped claim so a
    /// faster loop never cuts twice.
    pub(super) async fn maybe_cut_release(&self) {
        let every = self.config.releases.cut_every_days;
        if !self.config.releases.enabled || every == 0 || !self.config.git.enabled {
            return;
        }
        let (Some(git), Some(forge), Some(files)) =
            (self.git.clone(), self.forge.clone(), self.files.clone())
        else {
            return;
        };
        // Date-gate through daily_jobs: "release_cut" holds the last cut date.
        let today = crate::state::now_rfc3339()[..10].to_owned();
        let Ok(state) = self.store.load().await else {
            return;
        };
        if let Some(last) = state.daily_jobs.get("release_cut") {
            if days_between(last, &today) < every {
                return;
            }
        }
        // An open release PR means the last cut still waits on its human.
        if let Ok(prs) = forge.list_open_prs().await {
            if prs.iter().any(|p| p.title.starts_with("release: v")) {
                return;
            }
        }
        let wd = &self.work_dir;
        let base = self.flow_base().to_owned();
        // Commits since the last v* tag (or everything, first release).
        let Some(subjects) = Self::subjects_since_last_tag(&git, wd, &base).await else {
            return;
        };
        // CXA-F231: the pure manifest gate between raw subjects and
        // bump + changelog. With `cut_only_verified` (default) only subjects
        // whose ticket references are ALL Verified-complete drive the
        // release — ref-less subjects included in that exclusion,
        // honest-by-default. Knob off preserves the pre-F231 all-subjects
        // behavior during migration.
        let (subjects, rc_note) = if self.config.releases.cut_only_verified {
            let manifest = crate::use_cases::release_assembly::cut_manifest(&state, &subjects);
            (manifest.included, manifest.note)
        } else {
            (subjects, String::new())
        };
        let Some(bump) = classify_bump(&subjects) else {
            return; // nothing releasable since the last tag
        };
        let Some(cargo_text) = files.read(&wd.join("Cargo.toml")).await else {
            return;
        };
        let Some(cur) = super::parse_cargo_version(&cargo_text)
            .as_deref()
            .and_then(|v| SemVer::parse(v).ok())
        else {
            return;
        };
        let next = cur.bumped(bump);
        let branch = format!("release/v{next}");
        if !self
            .push_release_branch(&git, &files, &base, &cur, &next, &branch, &cargo_text)
            .await
        {
            return;
        }
        let msg = format!("release: v{next}");
        let body = changelog(&next.to_string(), &subjects);
        match forge.open_pr(&branch, &base, &msg, &body).await {
            Ok(pr) => {
                let pr = pr.number;
                let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                    s.daily_jobs.insert("release_cut".to_owned(), today.clone());
                    s.post_chat_in(
                        "SM",
                        &format!(
                            "🏷️ Release PR opened: v{cur} → v{next} (#{pr}) — lands from the \
                             Inbox (Cargo.toml is human-gated); the merge tags v{next}.{rc_note}"
                        ),
                        crate::state::AGENTS_CHANNEL,
                        Vec::new(),
                    );
                    Ok(())
                })
                .await;
                self.notify("release_cut", format!("release PR v{next} opened (#{pr})"))
                    .await;
            }
            Err(e) => tracing::warn!("release cut: open PR failed: {e}"),
        }
    }

    /// Conventional-commit subjects on the flow base since its last `v*` tag
    /// (everything, on the first release). `None` when git fails — a cut that
    /// cannot read history must not guess.
    async fn subjects_since_last_tag(
        git: &std::sync::Arc<dyn crate::ports::outbound::GitPort>,
        wd: &std::path::Path,
        base: &str,
    ) -> Option<Vec<String>> {
        let _ = git.raw(wd, &["fetch", "origin", base, "--tags"]).await;
        let (has_tag, tag) = git
            .raw(
                wd,
                &[
                    "describe",
                    "--tags",
                    "--abbrev=0",
                    "--match",
                    "v*",
                    &format!("origin/{base}"),
                ],
            )
            .await;
        let range = if has_tag {
            format!("{}..origin/{base}", tag.trim())
        } else {
            format!("origin/{base}")
        };
        let (ok, subjects) = git.raw(wd, &["log", &range, "--pretty=%s"]).await;
        if !ok {
            return None;
        }
        Some(subjects.lines().map(str::to_owned).collect())
    }

    /// Build and push the release branch in a scratch worktree (the live tree
    /// belongs to the agents): bump the manifest, mirror the bump into the
    /// lockfile's own workspace entries (so the first post-merge build doesn't
    /// dirty the leader tree with a lock update the release forgot), commit,
    /// push. Returns whether the branch is on origin.
    #[allow(clippy::too_many_arguments)] // one linear build recipe, private
    async fn push_release_branch(
        &self,
        git: &std::sync::Arc<dyn crate::ports::outbound::GitPort>,
        files: &std::sync::Arc<dyn crate::ports::outbound::WorkspaceFilesPort>,
        base: &str,
        cur: &SemVer,
        next: &SemVer,
        branch: &str,
        cargo_text: &str,
    ) -> bool {
        let wd = &self.work_dir;
        let wt = wd
            .parent()
            .unwrap_or(wd)
            .join(".coxagent-worktrees")
            .join(format!("release-cut-{next}"));
        let _ = git
            .raw(
                wd,
                &["worktree", "remove", "--force", &wt.to_string_lossy()],
            )
            .await;
        // Retry safety: a cut that COMMITTED but failed to push (network/auth
        // down that minute) leaves the local branch behind, and `worktree add
        // -b` then fails silently forever after — v2.27.0 sat orphaned for a
        // day. The branch is re-derived from origin on every cut, so deleting
        // a leftover loses nothing.
        let _ = git.raw(wd, &["branch", "-D", branch]).await;
        let (ok, _) = git
            .raw(
                wd,
                &[
                    "worktree",
                    "add",
                    "-b",
                    branch,
                    &wt.to_string_lossy(),
                    &format!("origin/{base}"),
                ],
            )
            .await;
        if !ok {
            return false;
        }
        let new_manifest = cargo_text.replacen(
            &format!("version = \"{cur}\""),
            &format!("version = \"{next}\""),
            1,
        );
        files.write(&wt.join("Cargo.toml"), &new_manifest).await;
        if let Some(lock) = files.read(&wt.join("Cargo.lock")).await {
            let updated = update_workspace_lock(&lock, &cur.to_string(), &next.to_string());
            files.write(&wt.join("Cargo.lock"), &updated).await;
        }
        let msg = format!("release: v{next}");
        let (committed, out) = git.raw(&wt, &["commit", "-am", &msg]).await;
        let pushed = if committed {
            let (p, pout) = git.raw(&wt, &["push", "-u", "origin", branch]).await;
            if !p {
                tracing::warn!("release cut: push failed: {pout}");
            }
            p
        } else {
            tracing::warn!("release cut: commit failed: {out}");
            false
        };
        let _ = git
            .raw(
                wd,
                &["worktree", "remove", "--force", &wt.to_string_lossy()],
            )
            .await;
        pushed
    }

    /// Tag `vX.Y.Z` once a release PR merges — called from the merged-PR sync
    /// with the merged HEAD branch (`release/vX.Y.Z`). Best-effort; an
    /// existing tag is left alone.
    pub(super) async fn tag_merged_release(&self, head: &str) {
        let Some(version) = head.trim().strip_prefix("release/v") else {
            return;
        };
        if SemVer::parse(version).is_err() {
            return;
        }
        let Some(git) = &self.git else { return };
        let wd = &self.work_dir;
        let base = self.flow_base().to_owned();
        let tag = format!("v{version}");
        let _ = git.raw(wd, &["fetch", "origin", &base]).await;
        if git.raw(wd, &["rev-parse", &tag]).await.0 {
            return; // already tagged
        }
        let (ok, _) = git
            .raw(
                wd,
                &["tag", "-a", &tag, "-m", &tag, &format!("origin/{base}")],
            )
            .await;
        if ok && git.raw(wd, &["push", "origin", &tag]).await.0 {
            let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                s.log_activity("SM", &format!("tagged release {tag}"), None);
                s.post_chat_in(
                    "SM",
                    &format!("🏷️ {tag} tagged and published."),
                    crate::state::AGENTS_CHANNEL,
                    Vec::new(),
                );
                Ok(())
            })
            .await;
            self.notify("release_tagged", format!("{tag} tagged")).await;
        }
    }
}

/// The SemVer bump the commit subjects since the last tag call for. Any
/// `feat` → Minor; else any releasable change (`fix`/`chore`/`docs`/
/// `refactor`/`perf`) → Patch; merge commits and `release:` markers prove
/// nothing. `None` = nothing to release. MAJOR is deliberately absent —
/// breaking users is a human decision, never an inference.
fn classify_bump(subjects: &[String]) -> Option<Bump> {
    let mut patch = false;
    for s in subjects {
        let s = s.trim();
        if s.starts_with("Merge ") || s.starts_with("release:") {
            continue;
        }
        let kind = s.split([':', '(', '!']).next().unwrap_or("");
        match kind {
            "feat" => return Some(Bump::Minor),
            "fix" | "chore" | "docs" | "refactor" | "perf" | "test" => patch = true,
            _ => {}
        }
    }
    patch.then_some(Bump::Patch)
}

/// Rewrite the lockfile's own workspace-member entries (`name = "coxagent*"`)
/// from `old` to `new`, leaving every dependency untouched.
fn update_workspace_lock(lock: &str, old: &str, new: &str) -> String {
    let mut out = String::with_capacity(lock.len());
    let mut in_workspace_pkg = false;
    for line in lock.lines() {
        if let Some(name) = line.trim().strip_prefix("name = \"") {
            in_workspace_pkg = name.starts_with("coxagent");
        }
        if in_workspace_pkg && line.trim() == format!("version = \"{old}\"") {
            use std::fmt::Write as _;
            let indent = &line[..line.len() - line.trim_start().len()];
            let _ = writeln!(out, "{indent}version = \"{new}\"");
            in_workspace_pkg = false;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The release PR body: subjects grouped the way a human reads a changelog.
fn changelog(next: &str, subjects: &[String]) -> String {
    let mut feats = Vec::new();
    let mut fixes = Vec::new();
    let mut other = Vec::new();
    for s in subjects {
        let s = s.trim();
        if s.starts_with("Merge ") || s.starts_with("release:") || s.is_empty() {
            continue;
        }
        if s.starts_with("feat") {
            feats.push(s);
        } else if s.starts_with("fix") {
            fixes.push(s);
        } else {
            other.push(s);
        }
    }
    let mut body = format!("## v{next}\n");
    for (title, list) in [
        ("### Features", feats),
        ("### Fixes", fixes),
        ("### Other", other),
    ] {
        if list.is_empty() {
            continue;
        }
        body.push_str(title);
        body.push('\n');
        for s in list {
            body.push_str("- ");
            body.push_str(s);
            body.push('\n');
        }
    }
    body.push_str("\nMerging this PR IS the release: the merged-PR sync tags it.\n");
    body
}

/// Whole days between two `YYYY-MM-DD` stamps (0 on parse trouble — which
/// blocks a re-cut rather than spamming one).
pub(super) fn days_between(a: &str, b: &str) -> u64 {
    let parse = |s: &str| {
        let fmt = time::macros::format_description!("[year]-[month]-[day]");
        time::Date::parse(s, &fmt).ok()
    };
    match (parse(a), parse(b)) {
        (Some(x), Some(y)) => u64::try_from((y - x).whole_days().max(0)).unwrap_or(0),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::use_cases::release_assembly::cut_manifest;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn any_feat_makes_a_minor_and_fixes_alone_a_patch() {
        assert_eq!(
            classify_bump(&v(&["fix(A): x", "feat(B): y", "chore: z"])),
            Some(Bump::Minor)
        );
        assert_eq!(
            classify_bump(&v(&["fix(A): x", "docs: y"])),
            Some(Bump::Patch)
        );
        // Merge noise and old release markers prove nothing.
        assert_eq!(
            classify_bump(&v(&["Merge branch 'x'", "release: v2.0.0"])),
            None
        );
        assert_eq!(classify_bump(&[]), None);
    }

    #[test]
    fn lock_update_touches_only_workspace_members() {
        let lock = "[[package]]\nname = \"serde\"\nversion = \"2.26.1\"\n\n\
                    [[package]]\nname = \"coxagent-app\"\nversion = \"2.26.1\"\n";
        let out = update_workspace_lock(lock, "2.26.1", "2.27.0");
        assert!(out.contains("name = \"serde\"\nversion = \"2.26.1\""));
        assert!(out.contains("name = \"coxagent-app\"\nversion = \"2.27.0\""));
    }

    #[test]
    fn changelog_groups_by_kind() {
        let body = changelog("2.27.0", &v(&["feat: a", "fix: b", "chore: c"]));
        assert!(body.contains("### Features\n- feat: a"));
        assert!(body.contains("### Fixes\n- fix: b"));
        assert!(body.contains("### Other\n- chore: c"));
    }

    #[test]
    fn day_gate_math() {
        assert_eq!(days_between("2026-08-10", "2026-08-17"), 7);
        assert_eq!(days_between("2026-08-17", "2026-08-17"), 0);
        assert_eq!(days_between("garbage", "2026-08-17"), 0);
    }

    /// A state with one Verified bug (CXA-B001) and one Fixed bug (CXA-B002),
    /// each walked along the bug lifecycle's only legal route.
    fn state_with_verified_and_fixed() -> crate::state::ProjectState {
        use coxagent_domain::{Complexity, Priority, Role, Status, Ticket, TicketId, TicketType};
        let bug = |id: &str| {
            Ticket::new(
                TicketId::new(id).expect("id"),
                TicketType::Bug,
                format!("defect {id}"),
                "repro",
                Priority::High,
                Complexity::Small,
                false,
            )
            .expect("ticket")
        };
        let mut fixed = bug("CXA-B002");
        fixed.claim(Role::DevBug, "dev", "t").expect("claim");
        fixed
            .transition_to(Role::DevBug, Status::Fixed)
            .expect("fix");
        let mut verified = bug("CXA-B001");
        verified.claim(Role::DevBug, "dev", "t").expect("claim");
        verified
            .transition_to(Role::DevBug, Status::Fixed)
            .expect("fix");
        verified
            .transition_to(Role::Test, Status::Verified)
            .expect("verify");
        crate::state::ProjectState {
            tickets: vec![verified, fixed],
            ..crate::state::ProjectState::default()
        }
    }

    #[test]
    fn manifest_gate_computes_bump_and_changelog_only_over_included_subjects() {
        let state = state_with_verified_and_fixed();
        let subjects = v(&[
            "feat(cxa): ship the flow #CXA-B001",  // verified → in
            "fix(cxa): partial support #CXA-B002", // Fixed-only → excluded
            "chore: no ticket ref",                // ref-less → excluded
        ]);
        let m = cut_manifest(&state, &subjects);

        // The bump is computed ONLY over the included set: the excluded fix
        // cannot sneak a patch in, the included feat still makes a minor.
        assert_eq!(classify_bump(&m.included), Some(Bump::Minor));
        // The changelog body corresponds one-to-one to manifest inclusions.
        let body = changelog("2.27.0", &m.included);
        assert!(body.contains("#CXA-B001"));
        assert!(!body.contains("partial support"));
        assert!(!body.contains("no ticket ref"));
        // The SM note surfaces bundles plus included/excluded ticket ids.
        assert!(m.note.contains("included: CXA-B001"), "{}", m.note);
        assert!(m.note.contains("excluded: CXA-B002"), "{}", m.note);
        assert!(m.note.contains("Unattributed [CXA-B001]"), "{}", m.note);
    }

    #[test]
    fn manifest_gate_yields_no_note_and_no_subjects_from_an_empty_verified_set() {
        // AC5 at the cut: zero Verified-complete tickets → no included
        // subjects (so no bump, no PR) and a note with nothing to list.
        let state = crate::state::ProjectState::default();
        let m = cut_manifest(&state, &v(&["feat: orphaned work"]));
        assert!(m.included.is_empty());
        assert_eq!(m.note, "");
        assert_eq!(classify_bump(&m.included), None, "no candidate is produced");
    }
}
