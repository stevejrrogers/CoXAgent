//! `SystemGit` — a [`GitPort`] backed by the `git` CLI. Runs each operation as
//! a subprocess in the codebase directory. Commit identity is passed per-invocation
//! (`-c user.name/email`) so the machine's global git config is never mutated.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{GitAuthor, GitPort, SyncBase};
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
/// A PAT from the environment (`COXAGENT_GH_TOKEN` wins, then `GITHUB_TOKEN` /
/// `GH_TOKEN`), or `None`.
fn git_token() -> Option<String> {
    for k in ["COXAGENT_GH_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(v) = std::env::var(k) {
            if !v.trim().is_empty() {
                return Some(v.trim().to_owned());
            }
        }
    }
    None
}

/// `GIT_CONFIG_*` env pairs that inject an `http.extraheader` Basic-auth header
/// for the token — the git-native way to auth HTTPS without a credential helper
/// and without the token touching argv. Empty when no token is set.
fn git_token_env() -> Option<Vec<(String, String)>> {
    let token = git_token()?;
    let basic = base64_encode(format!("x-access-token:{token}").as_bytes());
    Some(vec![
        ("GIT_CONFIG_COUNT".to_owned(), "1".to_owned()),
        ("GIT_CONFIG_KEY_0".to_owned(), "http.extraheader".to_owned()),
        (
            "GIT_CONFIG_VALUE_0".to_owned(),
            format!("Authorization: Basic {basic}"),
        ),
    ])
}

/// Standard base64 (no external crate) for the Basic-auth credential.
fn base64_encode(input: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(A[((n >> 18) & 63) as usize] as char);
        out.push(A[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            A[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// `git` with extra environment (for token-authenticated HTTPS).
async fn git_with_env(dir: &Path, args: &[&str], env: &[(String, String)]) -> Result<String, PortError> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir).stdin(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd
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

    async fn raw(&self, work_dir: &Path, args: &[&str]) -> (bool, String) {
        match tokio::process::Command::new("git")
            .args(args)
            .current_dir(work_dir)
            .stdin(std::process::Stdio::null())
            .output()
            .await
        {
            Ok(o) => (
                o.status.success(),
                String::from_utf8_lossy(&o.stdout).into_owned(),
            ),
            Err(_) => (false, String::new()),
        }
    }

    async fn working_tree(
        &self,
        work_dir: &Path,
    ) -> Result<coxagent_application::ports::outbound::WorkingTreeDiff, PortError> {
        // Three reads, one snapshot: the gates decide on all of them together,
        // and half a snapshot (paths from one moment, diff from another) is how
        // a gate mis-blames a change.
        let tracked = git(work_dir, &["diff", "HEAD", "--name-only"])
            .await
            .unwrap_or_default();
        let untracked = git(work_dir, &["ls-files", "--others", "--exclude-standard"])
            .await
            .unwrap_or_default();
        let changed_paths = tracked
            .lines()
            .chain(untracked.lines())
            .map(|l| l.trim().to_owned())
            .filter(|l| !l.is_empty())
            .collect();
        let changed_paths: Vec<String> = changed_paths;
        let mut cfg_test_line = std::collections::BTreeMap::new();
        for p in &changed_paths {
            if let Ok(text) = std::fs::read_to_string(work_dir.join(p)) {
                if let Some(line) = text
                    .lines()
                    .position(|l| l.trim_start().starts_with("#[cfg(test)]"))
                {
                    cfg_test_line.insert(p.clone(), line);
                }
            }
        }
        Ok(coxagent_application::ports::outbound::WorkingTreeDiff {
            changed_paths,
            full_diff: git(work_dir, &["diff", "HEAD"]).await.unwrap_or_default(),
            unified0_diff: git(work_dir, &["diff", "HEAD", "-U0"])
                .await
                .unwrap_or_default(),
            cfg_test_line,
        })
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
        // gh-less token push: when a PAT is in the environment, hand it to git
        // as an HTTP auth header so a host with a token but no ssh key / gh can
        // still push. The secret rides GIT_CONFIG_* env (git 2.31+), never the
        // argv — so it can't leak through `ps`, matching the forge's stance.
        // Harmless when origin is ssh (git ignores the http header there).
        match git_token_env() {
            Some(env) => git_with_env(work_dir, &["push", "-u", "origin", branch], &env)
                .await
                .map(|_| ()),
            None => git(work_dir, &["push", "-u", "origin", branch]).await.map(|_| ()),
        }
    }

    async fn sync_base(&self, work_dir: &Path, base: &str) -> Result<SyncBase, PortError> {
        git(work_dir, &["fetch", "origin", base]).await?;
        let target = format!("origin/{base}");
        // Fast path: branch already contains the base tip.
        if git(work_dir, &["merge-base", "--is-ancestor", &target, "HEAD"])
            .await
            .is_ok()
        {
            return Ok(SyncBase::UpToDate);
        }
        if git(
            work_dir,
            &[
                "-c",
                "user.name=coxagent-bot",
                "-c",
                "user.email=coxagent-bot@users.noreply.github.com",
                "merge",
                "--no-edit",
                &target,
            ],
        )
        .await
        .is_ok()
        {
            return Ok(SyncBase::Merged);
        }
        // Merge stopped — list the files left with conflict markers. If there
        // are none, the merge failed for another reason: bubble up.
        let unmerged = git(work_dir, &["diff", "--name-only", "--diff-filter=U"]).await?;
        let files: Vec<String> = unmerged.lines().map(str::to_owned).collect();
        if files.is_empty() {
            let _ = git(work_dir, &["merge", "--abort"]).await;
            return Err(PortError::Backend(format!(
                "merge of {target} failed without conflicts"
            )));
        }
        Ok(SyncBase::Conflicts(files))
    }

    async fn abort_merge(&self, work_dir: &Path) -> Result<(), PortError> {
        git(work_dir, &["merge", "--abort"]).await.map(|_| ())
    }

    async fn head_sha(&self, work_dir: &Path) -> Result<String, PortError> {
        git(work_dir, &["rev-parse", "HEAD"]).await
    }

    async fn update_ref(&self, work_dir: &Path, refname: &str, sha: &str) -> Result<(), PortError> {
        git(work_dir, &["update-ref", refname, sha])
            .await
            .map(|_| ())
    }

    async fn worktree_add(&self, work_dir: &Path, path: &Path, sha: &str) -> Result<(), PortError> {
        let path = path.to_string_lossy();
        git(work_dir, &["worktree", "add", "--detach", &path, sha])
            .await
            .map(|_| ())
    }

    async fn worktree_remove(&self, work_dir: &Path, path: &Path) -> Result<(), PortError> {
        let path = path.to_string_lossy();
        // Best-effort: `path` may not exist yet (first-ever rollback) or its
        // registration may be stale (directory removed out-of-band) — either
        // way, `prune` leaves the repo clean for the next `worktree_add`.
        let _ = git(work_dir, &["worktree", "remove", "--force", &path]).await;
        git(work_dir, &["worktree", "prune"]).await.map(|_| ())
    }

    async fn changed_paths(
        &self,
        work_dir: &Path,
        from_sha: &str,
        to_sha: &str,
    ) -> Result<Vec<String>, PortError> {
        let range = format!("{from_sha}..{to_sha}");
        let out = git(work_dir, &["diff", "--name-only", &range]).await?;
        Ok(out.lines().map(str::to_owned).collect())
    }

    async fn create_tag(
        &self,
        work_dir: &Path,
        name: &str,
        ref_target: &str,
        author: &GitAuthor,
    ) -> Result<(), PortError> {
        // Annotated tag (carries a message) marking the release point. The
        // identity is supplied inline (`git -c user.name/email`) like
        // `commit_all`, so an annotated tag never depends on ambient git
        // config — a fresh CI runner or unconfigured repo must not block the
        // release tag.
        let name_cfg = format!("user.name={}", author.name);
        let email_cfg = format!("user.email={}", author.email);
        git(
            work_dir,
            &[
                "-c", &name_cfg, "-c", &email_cfg, "tag", "-a", name, ref_target, "-m", name,
            ],
        )
        .await
        .map(|_| ())
    }

    async fn tag_exists(&self, work_dir: &Path, name: &str) -> bool {
        let refname = format!("refs/tags/{name}");
        // `rev-parse --verify --quiet` exits 0 iff the ref resolves, so the
        // `git` helper's Ok-on-success is exactly "tag exists".
        git(work_dir, &["rev-parse", "--verify", "--quiet", &refname])
            .await
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        // The exact shape used for the git Basic-auth header.
        assert_eq!(base64_encode(b"x-access-token:t"), "eC1hY2Nlc3MtdG9rZW46dA==");
    }

    #[test]
    fn token_env_injects_an_auth_header_out_of_argv() {
        // Guard: the secret must ride GIT_CONFIG_VALUE_0, never a CLI arg.
        std::env::set_var("COXAGENT_GH_TOKEN", "secret123");
        let env = git_token_env().expect("token env");
        std::env::remove_var("COXAGENT_GH_TOKEN");
        assert!(env.iter().any(|(k, _)| k == "GIT_CONFIG_KEY_0"));
        let val = &env.iter().find(|(k, _)| k == "GIT_CONFIG_VALUE_0").unwrap().1;
        assert!(val.starts_with("Authorization: Basic "));
        assert!(!val.contains("secret123"), "raw token must not appear verbatim");
    }

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

    #[tokio::test]
    async fn head_sha_and_update_ref_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let g = SystemGit::new();
        init_repo(tmp.path()).await;
        fs::write(tmp.path().join("a.txt"), "x").unwrap();
        let sha = g
            .commit_all(tmp.path(), "feat: base", &author())
            .await
            .unwrap()
            .unwrap();

        let head = g.head_sha(tmp.path()).await.unwrap();
        assert!(head.starts_with(&sha), "head_sha should resolve to HEAD");

        g.update_ref(tmp.path(), "refs/coxagent/last-good", &head)
            .await
            .unwrap();
        let resolved = git(tmp.path(), &["rev-parse", "refs/coxagent/last-good"])
            .await
            .unwrap();
        assert_eq!(resolved, head, "ref now points at the given sha");
    }

    #[tokio::test]
    async fn create_tag_then_tag_exists_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let g = SystemGit::new();
        init_repo(tmp.path()).await;
        fs::write(tmp.path().join("a.txt"), "x").unwrap();
        let sha = g
            .commit_all(tmp.path(), "feat: base", &author())
            .await
            .unwrap()
            .unwrap();

        assert!(
            !g.tag_exists(tmp.path(), "v1.0.0").await,
            "no tag before creation"
        );
        g.create_tag(tmp.path(), "v1.0.0", &sha, &author())
            .await
            .unwrap();
        assert!(
            g.tag_exists(tmp.path(), "v1.0.0").await,
            "tag exists after creation"
        );
        assert!(
            !g.tag_exists(tmp.path(), "v9.9.9").await,
            "unknown tag stays absent"
        );
    }

    #[tokio::test]
    async fn worktree_add_checks_out_a_detached_copy_without_touching_the_live_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let g = SystemGit::new();
        init_repo(tmp.path()).await;
        fs::write(tmp.path().join("a.txt"), "v1").unwrap();
        g.commit_all(tmp.path(), "feat: v1", &author())
            .await
            .unwrap();
        let good_sha = g.head_sha(tmp.path()).await.unwrap();
        fs::write(tmp.path().join("a.txt"), "v2 (broken)").unwrap();
        g.commit_all(tmp.path(), "feat: v2", &author())
            .await
            .unwrap();

        let rollback_dir = tmp.path().parent().unwrap().join(format!(
            "{}-rollback",
            tmp.path().file_name().unwrap().to_string_lossy()
        ));
        g.worktree_add(tmp.path(), &rollback_dir, &good_sha)
            .await
            .unwrap();

        // The rollback worktree holds the OLD content...
        assert_eq!(
            fs::read_to_string(rollback_dir.join("a.txt")).unwrap(),
            "v1"
        );
        // ...while the live tree is untouched (still on the new commit).
        assert_eq!(
            fs::read_to_string(tmp.path().join("a.txt")).unwrap(),
            "v2 (broken)"
        );
        assert_eq!(g.current_branch(tmp.path()).await.unwrap(), "main");

        g.worktree_remove(tmp.path(), &rollback_dir).await.unwrap();
        assert!(!rollback_dir.exists());
        // Re-adding after remove (the "always freshly created" contract) works.
        g.worktree_add(tmp.path(), &rollback_dir, &good_sha)
            .await
            .unwrap();
        assert!(rollback_dir.exists());
    }

    #[tokio::test]
    async fn changed_paths_lists_files_that_differ_between_two_commits() {
        let tmp = tempfile::tempdir().unwrap();
        let g = SystemGit::new();
        init_repo(tmp.path()).await;
        fs::write(tmp.path().join("a.txt"), "x").unwrap();
        let from = g
            .commit_all(tmp.path(), "feat: a", &author())
            .await
            .unwrap()
            .unwrap();
        fs::create_dir_all(tmp.path().join("migrations")).unwrap();
        fs::write(tmp.path().join("migrations/001.sql"), "create table t").unwrap();
        let to = g
            .commit_all(tmp.path(), "feat: migration", &author())
            .await
            .unwrap()
            .unwrap();

        let paths = g.changed_paths(tmp.path(), &from, &to).await.unwrap();
        assert_eq!(paths, vec!["migrations/001.sql".to_owned()]);
    }
}
