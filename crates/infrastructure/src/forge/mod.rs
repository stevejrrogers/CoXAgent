//! Forge (code host) adapters.

pub mod gh_api_forge;
pub mod gh_forge;
pub mod gl_forge;

pub use gh_api_forge::GhApiForge;
pub use gh_forge::GhForge;
pub use gl_forge::GlForge;

/// Probe, from THIS machine, what git and forge credentials can actually do for
/// `repo`: push a branch, and open a pull request.
///
/// Those are two different credentials — a push rides an ssh key, a PR is an API
/// call as whoever the CLI is logged in as — and one commonly works while the
/// other does not. Only the machine holding them can answer, which is why the
/// result travels to the hub through the worker registry instead of being
/// probed there: a container serving the dashboard has no key, no CLI and no
/// checkout, so asking it always reports failure.
pub async fn probe_git_access(
    repo: &str,
    account: &str,
    work_dir: &std::path::Path,
) -> coxagent_application::ports::outbound::GitCheck {
    use coxagent_application::ports::outbound::GitCheck;
    let mut out = GitCheck::default();

    // --- push half: does a dry-run push get accepted? ---
    if work_dir.join(".git").exists() {
        let dry = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            tokio::process::Command::new("git")
                .current_dir(work_dir)
                .args([
                    "push",
                    "--dry-run",
                    "origin",
                    "HEAD:refs/heads/coxagent-connection-test",
                ])
                .stdin(std::process::Stdio::null())
                .output(),
        )
        .await;
        match dry {
            Ok(Ok(o)) if o.status.success() => out.push_ok = true,
            Ok(Ok(o)) => {
                out.detail = String::from_utf8_lossy(&o.stderr)
                    .lines()
                    .last()
                    .unwrap_or("")
                    .chars()
                    .take(200)
                    .collect();
            }
            _ => "push --dry-run timed out".clone_into(&mut out.detail),
        }
    } else {
        "not a git checkout".clone_into(&mut out.detail);
    }

    out.merge_with(probe_forge_pr_access(repo, account).await);
    out
}

/// Whether the forge credential can OPEN a pull request for `repo`, acting as
/// the named stored login (empty = the CLI's active one).
///
/// Kept apart from the push check because they are different credentials: a push
/// rides an ssh key, a PR is an API call. One works without the other far more
/// often than not.
async fn probe_forge_pr_access(
    repo: &str,
    account: &str,
) -> coxagent_application::ports::outbound::GitCheck {
    use coxagent_application::ports::outbound::GitCheck;
    let mut out = GitCheck::default();
    let token = if account.is_empty() {
        None
    } else {
        tokio::process::Command::new("gh")
            .args(["auth", "token", "--user", account])
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .filter(|t| !t.is_empty())
    };
    if !account.is_empty() && token.is_none() {
        out.account = account.to_owned();
        out.remedy =
            format!("gh has no stored login for '{account}' — run `gh auth login` as that account");
        return out;
    }
    let mut who = tokio::process::Command::new("gh");
    who.args(["api", "user", "--jq", ".login"])
        .stdin(std::process::Stdio::null());
    if let Some(t) = &token {
        who.env("GH_TOKEN", t);
    }
    out.account = match who.output().await {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_owned(),
        _ => String::new(),
    };
    if repo.is_empty() {
        return out;
    }
    // Probe the operation that actually matters: CREATING a pull request.
    //
    // Reading `repos/{repo}` only proves the account can SEE the repository, and
    // a token scoped `pull_requests: read` passes that happily — then the first
    // finished ticket fails at delivery. A POST with deliberately empty fields
    // separates the two without creating anything: 403 means the credential may
    // not open PRs at all, while 422 means it may and only this input was
    // invalid, which is the answer we want.
    let mut api = tokio::process::Command::new("gh");
    api.args([
        "api",
        &format!("repos/{repo}/pulls"),
        "-X",
        "POST",
        "-f",
        "head=",
        "-f",
        "base=",
    ])
    .stdin(std::process::Stdio::null());
    if let Some(t) = &token {
        api.env("GH_TOKEN", t);
    }
    let (allowed, stderr) = match api.output().await {
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr).to_string();
            // Anything that is not a permission refusal means the credential is
            // accepted for this operation.
            (
                !err.contains("Resource not accessible") && !err.contains("(HTTP 403)"),
                err,
            )
        }
        Err(e) => {
            out.remedy = format!("gh not runnable here: {e}");
            return out;
        }
    };
    out.api_ok = allowed;
    if !allowed {
        out.remedy = if out.account.is_empty() {
            "gh is not signed in — run `gh auth login`. Pull requests will fail.".to_owned()
        } else {
            format!(
                "'{}' cannot OPEN pull requests on {repo} — it can read them, which is why a \
                 repo-visibility check passes and the first finished ticket still fails to \
                 deliver. Grant the token `Pull requests: Read and write` (fine-grained PAT: \
                 Repository permissions), or sign in with one that has it.",
                out.account
            )
        };
        if !stderr.trim().is_empty() {
            out.detail = stderr
                .lines()
                .last()
                .unwrap_or("")
                .chars()
                .take(160)
                .collect();
        }
    }
    out
}

/// Pick the GitHub forge for a repo: the `gh` CLI when it's on the host (uses
/// its stored logins, per-account), otherwise a token-only REST adapter for a
/// box that has a PAT but no CLI (a container, CI). Falls back to `GhForge`
/// when neither a CLI nor a token is available — it will error clearly at call
/// time rather than silently doing nothing.
#[must_use]
pub fn github_forge(
    repo: impl Into<String>,
    base_url: impl Into<String>,
    work_dir: std::path::PathBuf,
    account: impl Into<String>,
) -> std::sync::Arc<dyn coxagent_application::ports::outbound::ForgePort> {
    let (repo, base_url, account) = (repo.into(), base_url.into(), account.into());
    if crate::engine::registry::resolve_binary("gh").is_some() {
        return std::sync::Arc::new(GhForge::with_account(repo, base_url, work_dir, account));
    }
    if let Some(tok) = GhApiForge::token_from_env() {
        if let Some(f) = GhApiForge::new(repo.clone(), &base_url, tok) {
            tracing::info!("forge: no gh CLI — using token REST API for {repo}");
            return std::sync::Arc::new(f);
        }
    }
    std::sync::Arc::new(GhForge::with_account(repo, base_url, work_dir, account))
}
