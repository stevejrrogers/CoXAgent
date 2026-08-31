//! The single source of truth for which docker resources an automated pass
//! may tear down: compose projects via [`reclaimable_compose_project`] and
//! raw, label-less containers via [`reclaimable_raw_container`].
//!
//! Both the deploy port-eviction self-heal (see [`docker_compose`]) and the
//! hourly docker janitor (see `crates/presentation/src/server/docs.rs`) decide
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
    use super::{reclaimable_compose_project, reclaimable_raw_container};

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
}
