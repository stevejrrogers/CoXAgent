// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! The docker janitor: the hourly host sweep that keeps agent deploy residue
//! from silting the machine (CXA-B143). Decisions live in the shared policy
//! `coxagent_infrastructure::deploy::reclaimable`; this file is the IO that
//! gathers evidence and acts on it.

use super::*;
use coxagent_infrastructure::deploy::{
    image_sweep_candidate, orphaned_compose_image, reclaimable_compose_project, reclaimable_volume,
};

/// Docker janitor: agents deploy a lot — the host must not silt up. Hourly,
/// in four sweeps:
///
/// 1. any reclaimable compose project whose containers are ALL stopped gets a
///    full `down -v --remove-orphans` (dead previews, stale deploys), and any
///    PR preview still running past [`PREVIEW_TTL`] is reclaimed — `-v` so a
///    project this pass destroys cannot leave its volumes dormant behind
///    (CXA-B143);
/// 2. dormant volumes are swept: volumes labelled for a reclaimable project
///    that has not a single container left are removed — teardowns from
///    before the `-v` above (and outright-killed runs) left exactly that
///    residue on the host (CXA-B143);
/// 3. orphaned tagged images are swept: compose's built `<project>-<service>`
///    tags outlive every teardown because `image prune -f` only reaps
///    DANGLING layers, so each worktree stack used to leave ~375 MB behind
///    (CXA-B143);
/// 4. dangling images are pruned (unchanged).
///
/// Which resources are reclaimable is decided by the single shared policy in
/// `coxagent_infrastructure::deploy::reclaimable`: it excludes our own live
/// hub and backing services, so a janitor tick can never take production down.
/// The sweep errs on the side of keeping and is FAIL-CLOSED: when an evidence
/// probe cannot be answered, the sweep it feeds is skipped for the tick rather
/// than run against an empty world.
pub(super) async fn docker_janitor() {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        // Phase 1 — reclaim dead / expired compose projects. Docker silent →
        // the whole tick is skipped; acting on an empty project list is how a
        // sweep convinced itself everything was dead.
        let Some(projects) = compose_projects_with_status().await else {
            continue;
        };
        for (name, status) in projects {
            if !reclaimable_compose_project(&name) {
                continue;
            }
            let mut reason = "dead";
            if status.contains("running") {
                if !name.starts_with(PREVIEW_PROJECT_PREFIX)
                    || !preview_is_stale(&name, PREVIEW_TTL).await
                {
                    continue;
                }
                reason = "expired preview";
            }
            run_docker(&["compose", "-p", &name, "down", "-v", "--remove-orphans"]).await;
            tracing::info!("docker janitor: removed {reason} compose project {name}");
        }
        sweep_dormant_volumes().await;
        sweep_orphaned_images().await;
        run_docker(&["image", "prune", "-f"]).await;
    }
}

/// One best-effort `docker` invocation; `None` when the CLI could not be
/// spawned (daemon gone, docker absent).
async fn run_docker(args: &[&str]) -> Option<std::process::Output> {
    tokio::process::Command::new("docker")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()
}

/// Every compose project the daemon knows (running, stopped, or config-only)
/// as `(name, status)` pairs. `None` when docker did not answer — the caller
/// must not mistake that for "no projects exist" (fail-closed, CXA-B143).
async fn compose_projects_with_status() -> Option<Vec<(String, String)>> {
    let out = run_docker(&["compose", "ls", "-a", "--format", "json"]).await?;
    let list = serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout).ok()?;
    Some(
        list.iter()
            .filter_map(|p| {
                let name = p.get("Name")?.as_str()?.to_owned();
                let status = p
                    .get("Status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                Some((name, status))
            })
            .collect(),
    )
}

/// Projects that still own at least one container — running OR stopped —
/// from one `docker ps -a` pass. The evidence the volume sweep's "no container
/// left behind" check needs: a stopped container can be restarted against its
/// volumes, so its project's volumes are not dormant. `None` when the probe
/// itself failed — the sweep must refuse to act (fail-closed, CXA-B143).
async fn projects_with_containers() -> Option<Vec<String>> {
    let out = run_docker(&[
        "ps",
        "-a",
        "--format",
        "{{.Label \"com.docker.compose.project\"}}",
    ])
    .await?;
    if !out.status.success() {
        return None;
    }
    let mut projects: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    projects.sort_unstable();
    projects.dedup();
    Some(projects)
}

/// Every named volume with its compose-project label, from one
/// `docker volume ls --format json` pass (JSONL: one object per line, with
/// `Labels` rendered as a comma-joined `KEY=VALUE` string). Volumes without a
/// compose project label — plain `docker volume create`, anonymous leftovers —
/// come back with `None`.
async fn volumes_with_project_labels() -> Vec<(String, Option<String>)> {
    let Some(out) = run_docker(&["volume", "ls", "--format", "json"]).await else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let volume = serde_json::from_str::<serde_json::Value>(line).ok()?;
            let name = volume.get("Name")?.as_str()?.to_owned();
            let labels = volume.get("Labels").and_then(serde_json::Value::as_str);
            Some((
                name,
                labels.and_then(|l| label_value(l, COMPOSE_PROJECT_LABEL)),
            ))
        })
        .collect()
}

/// The label docker compose stamps on every resource it creates.
const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";

/// The value of `key` in docker's comma-joined `KEY=VALUE` label rendering;
/// `None` when the key is absent. Pure, so the label grammar is testable
/// without a daemon.
fn label_value(labels: &str, key: &str) -> Option<String> {
    labels.split(',').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k.trim() == key).then(|| v.trim().to_owned())
    })
}

/// Sweep 2: remove every volume the shared policy admits is dormant —
/// labelled for a reclaimable project that has no container left. Fail-closed:
/// when the container probe cannot be answered, nothing is removed this tick.
/// Best-effort per volume: a volume that grew a container between the probe
/// and the removal simply fails to remove and stays for the next tick.
async fn sweep_dormant_volumes() {
    let Some(holders) = projects_with_containers().await else {
        tracing::debug!("docker janitor: container probe failed, keeping all volumes");
        return;
    };
    for (volume, project) in volumes_with_project_labels().await {
        if !reclaimable_volume(&volume, project.as_deref(), &holders) {
            continue;
        }
        match run_docker(&["volume", "rm", &volume]).await {
            Some(o) if o.status.success() => {
                tracing::info!("docker janitor: removed dormant volume {volume}");
            }
            Some(o) => tracing::debug!(
                "docker janitor: could not remove dormant volume {volume}: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ),
            None => tracing::debug!("docker janitor: docker unavailable, kept volume {volume}"),
        }
    }
}

/// Sweep 3: remove every tagged image the shared policy admits is orphaned —
/// a reclaimable `<project>-<service>` repository whose project is gone and
/// that no container (running or stopped) references. The ancestor probe is
/// the authoritative reference evidence: a repository tag can dangle even
/// while containers keep the image alive by id. Fail-closed: a failed probe
/// counts the image as referenced, and a failed project listing skips the
/// sweep entirely.
async fn sweep_orphaned_images() {
    let Some(existing) = compose_projects_with_status().await else {
        tracing::debug!("docker janitor: project listing failed, keeping all images");
        return;
    };
    let existing: Vec<String> = existing.into_iter().map(|(name, _)| name).collect();
    let Some(out) = run_docker(&["images", "--format", "{{.Repository}}"]).await else {
        return;
    };
    let mut repositories: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && *l != "<none>")
        .map(ToOwned::to_owned)
        .collect();
    repositories.sort_unstable();
    repositories.dedup();
    for repository in repositories {
        // Cheap namespace precheck first: base images and everything an
        // existing project could own are skipped without the probe.
        if !image_sweep_candidate(&repository, &existing) {
            continue;
        }
        let referenced = match run_docker(&[
            "ps",
            "-a",
            "-q",
            "--filter",
            &format!("ancestor={repository}"),
        ])
        .await
        {
            // Fail-closed: only a successful, empty answer proves no container
            // references the image — a probe that could not be answered (CLI
            // gone, daemon dying mid-sweep) counts as referenced.
            Some(o) if o.status.success() => !String::from_utf8_lossy(&o.stdout).trim().is_empty(),
            _ => true,
        };
        if !orphaned_compose_image(&repository, referenced, &existing) {
            continue;
        }
        match run_docker(&["rmi", &repository]).await {
            Some(o) if o.status.success() => {
                tracing::info!("docker janitor: removed orphaned image {repository}");
            }
            Some(o) => tracing::debug!(
                "docker janitor: could not remove orphaned image {repository}: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ),
            None => tracing::debug!("docker janitor: docker unavailable, kept image {repository}"),
        }
    }
}

#[cfg(test)]
mod janitor_label_tests {
    use super::label_value;

    /// The volume sweep reads the compose project off docker's comma-joined
    /// label rendering — the exact shape `docker volume ls --format json`
    /// emits (verified live: `"Labels":"com.docker.compose.project=oneteam,
    /// com.docker.compose.version=5.1.4"`).
    #[test]
    fn the_compose_project_label_is_read_from_the_joined_rendering() {
        let labels = "com.docker.compose.volume=pgdata,\
                      com.docker.compose.project=cox--dead-preview,\
                      com.docker.compose.version=5.1.4";
        assert_eq!(
            label_value(labels, "com.docker.compose.project"),
            Some("cox--dead-preview".to_owned())
        );
    }

    /// A label-less volume renders an empty or absent Labels string — the
    /// sweep must see no project, never a mistaken one.
    #[test]
    fn an_absent_or_empty_label_renders_no_project() {
        assert_eq!(label_value("", "com.docker.compose.project"), None);
        assert_eq!(
            label_value("com.docker.volume.anonymous=", "com.docker.compose.project"),
            None
        );
    }

    /// KEY=VALUE pairs are split on the FIRST '=' so a value containing '='
    /// survives, and a key only matches exactly — a prefix key must not steal
    /// another pair's value.
    #[test]
    fn keys_match_exactly_and_values_may_contain_equals() {
        assert_eq!(
            label_value("a=b=c,ab=c", "ab"),
            Some("c".to_owned()),
            "the first '=' splits the pair"
        );
        assert_eq!(label_value("abx=c", "ab"), None, "no prefix matching");
    }
}
