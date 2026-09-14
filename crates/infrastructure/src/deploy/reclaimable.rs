//! The single source of truth for which docker resources an automated pass
//! may tear down: compose projects via [`reclaimable_compose_project`], their
//! dormant volumes via [`reclaimable_volume`], their orphaned tagged images
//! via [`image_sweep_candidate`] / [`orphaned_compose_image`], and raw,
//! label-less containers via [`reclaimable_raw_container`].
//!
//! Both the deploy port-eviction self-heal (see [`docker_compose`]) and the
//! hourly docker janitor (see
//! `crates/presentation/src/server/docker_janitor.rs`) decide
//! what they may reclaim. They used to each carry their own private copy of
//! that policy, and drift between them was a standing foot-gun: if one side
//! ever started treating the live hub or shared infra as reclaimable, an agent
//! deploy or a janitor tick could take production down to free a resource. This
//! module is that policy, in exactly one place.

/// The one namespace rule behind every reclaimability decision: only
/// agent-managed `cox-` names are ours; the live hub (`coxagent*`) and shared
/// backing infra (`cox-infra*`) are protected no matter how they were launched,
/// and case normalisation keeps the protection spoof-proof (CXA-F026).
fn reclaimable_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower == "cox-infra"
        || lower == "coxagent"
        || lower.starts_with("coxagent")
        || lower.starts_with("cox-infra")
    {
        return false;
    }
    lower.starts_with("cox-")
}

/// Whether a docker compose project may be safely reclaimed (torn down /
/// evicted) by an automated pass.
///
/// Only agent-managed preview deployments (`cox-<parent>-<dir>`, always named
/// with a `cox-` prefix) are reclaimable. The live hub (`coxagent`, plus every
/// service container sharing its prefix) and shared backing infrastructure
/// (`cox-infra`) are NEVER reclaimable — tearing either down is a self-inflicted
/// control-plane outage, exactly the bug where an automated deploy took the whole
/// system down trying to free resources it thought it owned. Anything outside our
/// own namespace is foreign and left alone.
#[must_use]
pub fn reclaimable_compose_project(project: &str) -> bool {
    reclaimable_name(project)
}

/// Whether a named docker volume may be pruned by an automated host sweep
/// (CXA-B143). A volume is reclaimable only when ALL of these hold:
///
/// * it carries a compose-project label, and that project is reclaimable by
///   [`reclaimable_compose_project`] (no label → ownership cannot be proven →
///   never touched);
/// * its NAME agrees with the label — compose names project volumes
///   `<project>_<key>`, so a relabelled or hand-made volume can never pass on
///   a label alone;
/// * NO container on the daemon (running or stopped) still belongs to that
///   project — the callers gather that evidence with
///   `docker ps -a --filter label=com.docker.compose.project=<project>`.
///
/// The second condition is what keeps the sweep honest: the janitor's phase-1
/// `down` may only destroy what the shared policy admits, and the same admission
/// test therefore governs the volumes it leaves behind.
#[must_use]
pub fn reclaimable_volume(
    volume_name: &str,
    project_label: Option<&str>,
    projects_with_containers: &[String],
) -> bool {
    let Some(project) = project_label else {
        return false;
    };
    if !reclaimable_compose_project(project) {
        return false;
    }
    if !volume_name.starts_with(format!("{project}_").as_str()) {
        return false;
    }
    !projects_with_containers.iter().any(|p| p == project)
}

/// Whether a TAGGED image abandoned by a torn-down compose project may be
/// removed by an automated host sweep (CXA-B143). `docker image prune -f`
/// only reaps DANGLING images, so compose's built `<project>-<service>` tags
/// used to survive every teardown and silt the host (~375 MB per worktree
/// stack). An image is orphaned — and removable — only when:
///
/// * no container (running or stopped) still references it — the caller
///   gathers that evidence with `docker ps -a --filter ancestor=<repo>`;
/// * its repository is reclaimable by the same namespace rule as everything
///   else here (`cox-` prefix, never the live hub or shared infra);
/// * NO project that still exists could own it. Compose tags built images
///   `<project>-<service>`, so if any existing project's name prefixes the
///   repository the image is kept — a shorter alive project shadowing a
///   longer dead one errs on the side of keeping, never destroying.
#[must_use]
pub fn orphaned_compose_image(
    repository: &str,
    referenced_by_container: bool,
    existing_projects: &[String],
) -> bool {
    !referenced_by_container && image_sweep_candidate(repository, existing_projects)
}

/// The cheap namespace precheck in front of the orphaned-image sweep
/// (CXA-B149): whether a repository is even worth the expensive
/// `docker ps -a --filter ancestor=` probe. Base images (`rust`, `postgres`),
/// the protected hub/infra names, and anything a still-existing project could
/// own are rejected here without a probe; everything else goes to
/// [`orphaned_compose_image`], which makes the final call with the reference
/// evidence. Pure so the candidate grammar is testable without a daemon.
#[must_use]
pub fn image_sweep_candidate(repository: &str, existing_projects: &[String]) -> bool {
    reclaimable_name(repository)
        && !existing_projects
            .iter()
            .any(|p| repository.starts_with(format!("{p}-").as_str()))
}

/// Whether an automated pass may stop a raw (non-compose) container by id.
///
/// A label-less container carries no compose-project label, so its NAME is the
/// only ownership signal left — and docker names compose containers
/// `<project>-<service>-<n>`, so the same namespace rule still reads through
/// it (CXA-B083: raw squatters used to be force-stopped with no check at all).
/// Only a demonstrably ours `cox-`-named container may be stopped; the live
/// hub or shared infra launched via plain `docker run`, and any foreign
/// container, are NEVER touched — an empty or unrecognisable name is treated
/// as foreign (cannot prove ownership → do not destroy).
/// CXA-B085: the deploy self-heal once force-stopped an unrelated `nginx`
/// squatting :8101 because it looked only at the port.
#[must_use]
pub fn reclaimable_raw_container(container_name: &str) -> bool {
    reclaimable_name(container_name)
}

#[cfg(test)]
mod tests {
    use super::{
        image_sweep_candidate, orphaned_compose_image, reclaimable_compose_project,
        reclaimable_raw_container, reclaimable_volume,
    };

    #[test]
    fn agent_preview_projects_are_reclaimable() {
        for preview in [
            "cox-cxa-codebase",
            "cox-my-project-preview",
            "cox--preview-42",
        ] {
            assert!(
                reclaimable_compose_project(preview),
                "{preview} is an agent-managed preview and must be reclaimable"
            );
        }
    }

    #[test]
    fn live_hub_and_prefixed_services_are_never_reclaimable() {
        for name in ["coxagent", "coxagent-gateway", "coxagent-db"] {
            assert!(
                !reclaimable_compose_project(name),
                "{name} is the live hub or one of its services and must never be reclaimed"
            );
        }
    }

    #[test]
    fn shared_infra_and_its_children_are_never_reclaimable() {
        for name in ["cox-infra", "cox-infra-db", "cox-infra-minio"] {
            assert!(
                !reclaimable_compose_project(name),
                "{name} is shared backing infra and must never be reclaimed"
            );
        }
    }

    #[test]
    fn protected_names_cannot_be_spoofed_by_case() {
        for spoof in [
            "COXAGENT",
            "CoxAgent",
            "CoxAgent-Gateway",
            "COXAGENT-GATEWAY",
            "COX-INFRA",
            "COX-Infra-DB",
            "Cox-Infra-Db",
        ] {
            assert!(
                !reclaimable_compose_project(spoof),
                "{spoof} must be treated as protected regardless of case"
            );
        }
    }

    #[test]
    fn foreign_non_preview_projects_are_not_reclaimable() {
        for foreign in ["someone-elses-stack", "another-app-prod"] {
            assert!(
                !reclaimable_compose_project(foreign),
                "{foreign} is outside our namespace and must never be reclaimed"
            );
        }
    }

    /// CXA-B083: a label-less container is only stoppable when its name shows
    /// it is ours — docker names compose containers `<project>-<service>-<n>`,
    /// so a `cox-` name is a leftover of our own preview (B080's raw fallback).
    #[test]
    fn our_cox_named_raw_container_is_reclaimable() {
        for name in [
            "cox-cxa-codebase-coxagent-1",
            "cox--stale-preview-hub-1",
            "cox--slot-b-hub",
            "cox-my-project-web-1",
        ] {
            assert!(
                reclaimable_raw_container(name),
                "{name} is demonstrably our own leftover and may be stopped by id"
            );
        }
    }

    /// CXA-B083: the live hub or shared infra launched via plain `docker run`
    /// (no compose label to read) must never be stopped by id either.
    #[test]
    fn protected_named_raw_containers_are_never_reclaimable() {
        for name in ["coxagent-hub-1", "cox-infra-redis-1", "coxagent"] {
            assert!(
                !reclaimable_raw_container(name),
                "{name} is protected infrastructure and must never be stopped"
            );
        }
    }

    /// CXA-B083 repro: an unlabeled foreign container squatting :8101 (plain
    /// `docker run -p 8101:80 nginx`) has no ownership evidence in its name —
    /// it must be left alone, never silently force-stopped during a deploy.
    #[test]
    fn foreign_or_unprovable_raw_containers_are_never_reclaimable() {
        for name in ["nginx", "bold_curie", "my-live-hub", ""] {
            assert!(
                !reclaimable_raw_container(name),
                "`{name}` cannot prove it is ours and must never be stopped"
            );
        }
    }

    /// The protected-name rule is case-normalised for raw containers too — a
    /// case-spoofed name must not sneak past the check.
    #[test]
    fn raw_container_protection_cannot_be_spoofed_by_case() {
        for spoof in ["COXAGENT-HUB", "CoxAgent-Hub", "COX-INFRA-REDIS"] {
            assert!(
                !reclaimable_raw_container(spoof),
                "{spoof} must be treated as protected regardless of case"
            );
        }
    }

    // --- CXA-B143: the host sweep's volume/image policy ---

    fn containers(projects: &[&str]) -> Vec<String> {
        projects.iter().map(|p| (*p).to_owned()).collect()
    }

    /// A dormant volume of a dead agent-managed project is exactly what the
    /// sweep exists to reap (the B143 residue shape).
    #[test]
    fn dormant_volume_of_a_dead_cox_project_is_reclaimable() {
        assert!(reclaimable_volume(
            "cox--coxagent-worktrees-cxa-slot-2-88a7821f_pgdata",
            Some("cox--coxagent-worktrees-cxa-slot-2-88a7821f"),
            &containers(&["cox-cxa-codebase"]),
        ));
    }

    /// A volume whose project still has containers (running or stopped) may
    /// never be pruned — even stopped containers can be restarted against it.
    #[test]
    fn volume_of_a_project_that_still_has_containers_is_kept() {
        assert!(!reclaimable_volume(
            "cox-cxa-codebase_pgdata",
            Some("cox-cxa-codebase"),
            &containers(&["cox-cxa-codebase"]),
        ));
    }

    /// The label alone never proves ownership: the volume NAME must carry the
    /// same project as its prefix, so a foreign or anonymous volume cannot be
    /// smuggled through with a copied label.
    #[test]
    fn a_volume_whose_name_disagrees_with_its_label_is_never_reclaimed() {
        assert!(!reclaimable_volume(
            "cox-cxa-codebase_pgdata",
            Some("cox--dead-preview"),
            &containers(&[]),
        ));
        assert!(!reclaimable_volume(
            "some_hash_id",
            Some("cox--dead-preview"),
            &containers(&[]),
        ));
    }

    /// Label-less volumes (plain `docker volume create`) have no ownership
    /// evidence at all — always kept, like every unprovable resource here.
    #[test]
    fn a_labelless_volume_is_never_reclaimed() {
        assert!(!reclaimable_volume(
            "cox--dead-preview_pgdata",
            None,
            &containers(&[]),
        ));
    }

    /// The namespace rule reads through volumes too: the live hub's and shared
    /// infra's volumes are protected no matter how dead they look, and case
    /// spoofs change nothing.
    #[test]
    fn protected_or_foreign_volume_projects_are_never_reclaimed() {
        for (name, label) in [
            ("coxagent_db", Some("coxagent")),
            ("cox-infra-redis_data", Some("cox-infra-redis")),
            ("cxa-backend_db", Some("cxa-backend")),
            ("COXAGENT_DB", Some("COXAGENT")),
        ] {
            assert!(
                !reclaimable_volume(name, label, &containers(&[])),
                "{name} (label {label:?}) must never be swept"
            );
        }
    }

    /// The B143 residue class: a tagged compose-built image whose project is
    /// gone and that no container references is orphaned and may be removed.
    #[test]
    fn tagged_image_of_a_gone_cox_project_is_orphaned() {
        assert!(orphaned_compose_image(
            "cox--coxagent-worktrees-cxa-slot-2-88a7821f-coxagent",
            false,
            &containers(&["cox-cxa-codebase", "cxa-backend"]),
        ));
    }

    /// A referenced image is never orphaned, no matter how dead its project
    /// looks — the ancestor evidence wins.
    #[test]
    fn a_referenced_image_is_never_orphaned() {
        assert!(!orphaned_compose_image(
            "cox--gone-preview-coxagent",
            true,
            &containers(&[]),
        ));
    }

    /// If ANY still-existing project could own the repository, the image is
    /// kept — the alive project's own build, and a longer dead project's tag
    /// shadowed by a shorter alive one, both err on the side of keeping.
    #[test]
    fn an_image_an_existing_project_could_own_is_kept() {
        assert!(!orphaned_compose_image(
            "cox-cxa-codebase-coxagent",
            false,
            &containers(&["cox-cxa-codebase"]),
        ));
        assert!(
            !orphaned_compose_image(
                "cox--gone-preview-hub-svc",
                false,
                &containers(&["cox--gone-preview"])
            ),
            "a longer dead project's image shadowed by a shorter alive one is kept too"
        );
    }

    /// The cheap precheck admits exactly what the sweep may ever probe: our
    /// namespace, nothing an existing project could own. Base images and the
    /// protected names are rejected without a probe.
    #[test]
    fn image_sweep_candidates_are_namespace_and_ownership_checked() {
        // The B143 residue shape: a dead worktree stack's built tag.
        assert!(image_sweep_candidate(
            "cox--coxagent-worktrees-cxa-slot-2-88a7821f-coxagent",
            &containers(&["cox-cxa-codebase"]),
        ));
        // Base images and foreign/protected names never reach the probe.
        for repo in [
            "rust",
            "postgres",
            "nginx",
            "coxagent-hub",
            "cox-infra-redis",
            "",
        ] {
            assert!(
                !image_sweep_candidate(repo, &containers(&[])),
                "`{repo}` is not a sweep candidate"
            );
        }
        // A repo an existing project could own is skipped without a probe.
        assert!(!image_sweep_candidate(
            "cox-cxa-codebase-coxagent",
            &containers(&["cox-cxa-codebase"]),
        ));
    }

    /// The namespace rule reads through image repositories as well: the live
    /// hub, shared infra, and anything foreign (base images, operator tags,
    /// our own `cxa-`-prefixed manual verification builds) are never swept —
    /// those are cleaned by hand, deliberately.
    #[test]
    fn protected_or_foreign_image_repositories_are_never_orphaned() {
        for repo in [
            "coxagent-hub",
            "cox-infra-redis",
            "cxa-f029-linux-gate",
            "rust",
            "postgres",
            "nginx",
            "",
        ] {
            assert!(
                !orphaned_compose_image(repo, false, &containers(&[])),
                "`{repo}` must never be swept by the automated pass"
            );
        }
    }
}
