//! The single source of truth for which docker resources an automated pass
//! may tear down: compose projects via [`reclaimable_compose_project`] and
//! label-less raw containers via [`reclaimable_raw_container`].
//!
//! Both the deploy port-eviction self-heal (see [`docker_compose`]) and the
//! hourly docker janitor (see `crates/presentation/src/server/docs.rs`) decide
//! what they may reclaim. They used to each carry their own private copy of
//! that policy, and drift between them was a standing foot-gun: if one side
//! ever started treating the live hub or shared infra as reclaimable, an agent
//! deploy or a janitor tick could take production down to free a resource. This
//! module is that policy, in exactly one place.

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
    let lower = project.to_ascii_lowercase();
    if lower == "cox-infra"
        || lower == "coxagent"
        || lower.starts_with("coxagent")
        || lower.starts_with("cox-infra")
    {
        return false;
    }
    lower.starts_with("cox-")
}

/// Whether a label-less ("raw `docker run`") container may be safely stopped
/// by id by an automated pass.
///
/// A raw container carries no compose-project label, so its NAME is the only
/// ownership signal left. Agent-managed deploys always name their containers
/// after their `cox-<parent>-<dir>` project (see
/// `docker_compose::compose_project_name`), so the same namespace rules as
/// [`reclaimable_compose_project`] apply verbatim: a `cox-`-prefixed name is
/// an agent preview we may stop, while the live hub, shared infra, and
/// anything outside our namespace — including docker's anonymous generated
/// names — are NEVER touched. CXA-B085: the deploy self-heal once force-stopped
/// an unrelated `nginx` squatting :8101 because it looked only at the port.
#[must_use]
pub fn reclaimable_raw_container(name: &str) -> bool {
    reclaimable_compose_project(name)
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

    #[test]
    fn agent_named_raw_containers_are_reclaimable() {
        for name in ["cox--slot-b-hub", "cox-my-project-web-1", "cox-cxa-codebase-app-1"] {
            assert!(
                reclaimable_raw_container(name),
                "{name} is an agent-managed raw container and must be reclaimable"
            );
        }
    }

    /// The CXA-B085 repro: a bare `docker run -p 8101:80 nginx` yields a holder
    /// with no compose label and a name (docker-generated or image-derived)
    /// that carries no ownership signal — never ours to stop.
    #[test]
    fn anonymous_raw_containers_are_never_reclaimable() {
        for name in ["nginx", "sharp_poincare", "quirky_turing"] {
            assert!(
                !reclaimable_raw_container(name),
                "{name} is an anonymous raw container and must never be stopped"
            );
        }
    }

    #[test]
    fn protected_and_foreign_raw_names_are_never_reclaimable() {
        for name in [
            "coxagent",
            "cox-infra-db",
            "COXAGENT",
            "Cox-Infra-Db",
            "someone-elses-stack",
        ] {
            assert!(
                !reclaimable_raw_container(name),
                "{name} must never be stopped by id regardless of case"
            );
        }
    }
}
