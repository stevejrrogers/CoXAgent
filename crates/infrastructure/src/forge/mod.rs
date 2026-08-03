//! Forge (code host) adapters.

pub mod gh_forge;
pub mod gl_forge;

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

    // --- API half: can we see the repo as the account we will act as? ---
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
    let mut api = tokio::process::Command::new("gh");
    api.args(["api", &format!("repos/{repo}")])
        .stdin(std::process::Stdio::null());
    if let Some(t) = &token {
        api.env("GH_TOKEN", t);
    }
    match api.output().await {
        Ok(o) if o.status.success() => out.api_ok = true,
        Ok(_) => {
            out.remedy = if out.account.is_empty() {
                "gh is not signed in — run `gh auth login`. Pull requests will fail.".to_owned()
            } else {
                format!(
                    "gh is signed in as '{}', which cannot see {repo}. Pull requests will fail — \
                     run `gh auth login` as an account with access, then set it as this \
                     project's git account.",
                    out.account
                )
            };
        }
        Err(e) => out.remedy = format!("gh not runnable here: {e}"),
    }
    out
}
