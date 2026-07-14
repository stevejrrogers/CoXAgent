//! `SystemGit` — a [`GitPort`] backed by the `git` CLI. Runs each operation as
//! a subprocess in the codebase directory. Commit identity is passed per-invocation
//! (`-c user.name/email`) so the machine's global git config is never mutated.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{GitAuthor, GitPort};
use coxagent_application::PortError;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

/// Runs git via the `git` CLI found on `PATH`.
#[derive(Default)]
pub struct SystemGit;

impl SystemGit {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// Run `git <args>` in `dir`, returning trimmed stdout on success.
async fn git(dir: &Path, args: &[&str]) -> Result<String, PortError> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| PortError::Backend(format!("git spawn: {e}")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(PortError::Backend(format!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

#[async_trait]
impl GitPort for SystemGit {
    async fn is_repo(&self, work_dir: &Path) -> bool {
        git(work_dir, &["rev-parse", "--is-inside-work-tree"])
            .await
            .is_ok_and(|s| s == "true")
    }

    async fn current_branch(&self, work_dir: &Path) -> Result<String, PortError> {
        let b = git(work_dir, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
        if b.is_empty() {
            return Err(PortError::Backend("git reported no branch".to_owned()));
        }
        Ok(b)
    }

    async fn checkout_branch(&self, work_dir: &Path, branch: &str) -> Result<(), PortError> {
        // Already on it? no-op. Exists? switch. Else create from HEAD.
        if self.current_branch(work_dir).await.ok().as_deref() == Some(branch) {
            return Ok(());
        }
        let exists = git(work_dir, &["rev-parse", "--verify", "--quiet", branch])
            .await
            .is_ok();
        let args: &[&str] = if exists {
            &["checkout", branch]
        } else {
            &["checkout", "-b", branch]
        };
        git(work_dir, args).await.map(|_| ())
    }

    async fn commit_all(
        &self,
        work_dir: &Path,
        message: &str,
        author: &GitAuthor,
    ) -> Result<Option<String>, PortError> {
        git(work_dir, &["add", "-A"]).await?;
        // Nothing staged → clean tree → nothing to commit.
        if git(work_dir, &["diff", "--cached", "--quiet"])
            .await
            .is_ok()
        {
            return Ok(None);
        }
        let name_cfg = format!("user.name={}", author.name);
        let email_cfg = format!("user.email={}", author.email);
        git(
            work_dir,
            &["-c", &name_cfg, "-c", &email_cfg, "commit", "-m", message],
        )
        .await?;
        let sha = git(work_dir, &["rev-parse", "--short", "HEAD"]).await?;
        Ok(Some(sha))
    }

    async fn push(&self, work_dir: &Path, branch: &str) -> Result<(), PortError> {
        git(work_dir, &["push", "-u", "origin", branch])
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    async fn init_repo(dir: &Path) {
        git(dir, &["init", "-b", "main"]).await.unwrap();
    }

    fn author() -> GitAuthor {
        GitAuthor {
            name: "coxagent-bot".to_owned(),
            email: "bot@users.noreply.github.com".to_owned(),
        }
    }

    #[tokio::test]
    async fn is_repo_true_only_after_init() {
        let tmp = tempfile::tempdir().unwrap();
        let git_impl = SystemGit::new();
        assert!(!git_impl.is_repo(tmp.path()).await);
        init_repo(tmp.path()).await;
        assert!(git_impl.is_repo(tmp.path()).await);
    }

    #[tokio::test]
    async fn commit_all_commits_then_reports_clean() {
        let tmp = tempfile::tempdir().unwrap();
        let g = SystemGit::new();
        init_repo(tmp.path()).await;
        fs::write(tmp.path().join("a.txt"), "hello").unwrap();

        let sha = g
            .commit_all(tmp.path(), "feat(X-1): add a", &author())
            .await
            .unwrap();
        assert!(sha.is_some(), "first commit should produce a sha");

        // A clean tree commits nothing.
        let again = g.commit_all(tmp.path(), "noop", &author()).await.unwrap();
        assert!(again.is_none(), "clean tree must not create a commit");

        // The commit used the passed identity, not the machine's global config.
        let email = git(tmp.path(), &["log", "-1", "--format=%ae"])
            .await
            .unwrap();
        assert_eq!(email, "bot@users.noreply.github.com");
    }

    #[tokio::test]
    async fn checkout_creates_then_switches_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let g = SystemGit::new();
        init_repo(tmp.path()).await;
        fs::write(tmp.path().join("a.txt"), "x").unwrap();
        g.commit_all(tmp.path(), "feat: base", &author())
            .await
            .unwrap();

        g.checkout_branch(tmp.path(), "feat/X-1").await.unwrap();
        assert_eq!(g.current_branch(tmp.path()).await.unwrap(), "feat/X-1");
        // Idempotent + switching back to an existing branch works.
        g.checkout_branch(tmp.path(), "feat/X-1").await.unwrap();
        g.checkout_branch(tmp.path(), "main").await.unwrap();
        assert_eq!(g.current_branch(tmp.path()).await.unwrap(), "main");
    }
}
