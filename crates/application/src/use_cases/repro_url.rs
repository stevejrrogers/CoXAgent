//! Resolve the live reproduction URL for a verify-pending ticket (CXA-F243):
//! where a human can open the fix RUNNING, instead of judging static evidence
//! (a screenshot, a captured request/response) at the verify gate.
//!
//! The infra already knows the live target: every project is assigned a
//! `deploy.host_port`, and whatever occupies that port — the merged-main build
//! or a PR preview — is what a reviewer would click. The decision here is a
//! pure function over a [`ReproUrlSnapshot`] the caller gathers through ports
//! (config parse, store load, preview tracking); the only IO the use case
//! performs is the health probe, judged through the injected
//! [`DeployPort`](crate::ports::outbound::DeployPort). A port that is
//! configured but dead resolves to `None` — a verify dialog must never hand
//! the reviewer a link to nothing.

use crate::ports::outbound::{is_publishable_host_port, DeployPort};
use std::sync::Arc;

/// What currently occupies the project's app port — the `reason` kind the
/// reproduction-url endpoint carries next to the URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReproSource {
    /// An open-PR preview occupies the app port: the reviewer sees the PR's
    /// branch running, not main.
    Preview,
    /// The merged-main deployment occupies the app port.
    Main,
}

impl ReproSource {
    /// Stable snake_case wire key, matching the endpoint's `reason` contract.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Main => "main",
        }
    }
}

/// Snapshot of everything the resolver decides over, as plain data. The caller
/// assembles it through ports: `deploy.host_port` parsed from the project's
/// raw config (the shared `parse_deploy_host_port` — a corrupt port must not
/// drift into a dead link), the ticket's status from the store, and the
/// preview state. Struct-literal-buildable, which is what keeps the decision
/// unit-testable with no host, no filesystem and no process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReproUrlSnapshot {
    /// `deploy.host_port`. `None` = nothing published, nothing to reproduce.
    pub host_port: Option<u16>,
    /// Whether an open-PR preview (rather than the main build) currently
    /// occupies the app port. Nothing durably tracks preview state yet, so
    /// today's wiring reports `false`; the field stays the design's seam — a
    /// preview tracker feeds `true` without touching the resolver again.
    pub preview_live: bool,
    /// Verify-pending predicate: the ticket is `Fixed` and not yet `Verified`.
    /// There is no separate status — the bug transition table's
    /// `Fixed -> Verified` edge IS the human verify gate, so "verify-pending"
    /// is exactly `status == Fixed`.
    pub ticket_verify_pending: bool,
}

/// A resolved reproduction target: the base URL a reviewer opens, plus what is
/// running behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReproUrl {
    /// Base URL of the running app, e.g. `http://localhost:8101/`.
    pub url: String,
    /// What occupies the port behind [`ReproUrl::url`].
    pub source: ReproSource,
}

/// The pure decision: a verify-pending ticket on a configured, publishable
/// port that actually answered resolves to its live URL, labeled by what runs
/// there (an open-PR preview outranks the main build). Nothing configured, an
/// unpublishable port (`0` is the kernel's "any free port" sentinel, never an
/// address — COX-B042, same rule every reader of `host_port` honors), a dead
/// port, or a ticket not awaiting verification all resolve to `None` — no
/// fabricated links, no panic.
#[must_use]
pub fn resolve_repro_url(snapshot: &ReproUrlSnapshot, healthy: bool) -> Option<ReproUrl> {
    if !snapshot.ticket_verify_pending {
        return None;
    }
    let port = snapshot.host_port?;
    if !is_publishable_host_port(port) || !healthy {
        return None;
    }
    let source = if snapshot.preview_live {
        ReproSource::Preview
    } else {
        ReproSource::Main
    };
    Some(ReproUrl {
        url: format!("http://localhost:{port}/"),
        source,
    })
}

/// The application entry point over the pure decision: probes the configured
/// port through the injected deploy adapter, then resolves.
pub struct ResolveReproUrlUseCase {
    deploy: Arc<dyn DeployPort>,
}

impl ResolveReproUrlUseCase {
    #[must_use]
    pub fn new(deploy: Arc<dyn DeployPort>) -> Self {
        Self { deploy }
    }

    /// Resolve the reproduction URL for one verify-pending ticket, or `None`
    /// when there is nothing honest to link.
    pub async fn execute(&self, snapshot: &ReproUrlSnapshot) -> Option<ReproUrl> {
        // One probe, no polling: this answers a reviewer's read, not a deploy
        // gate. A probe that cannot run is NOT evidence the app is up — same
        // posture as `verify_deploy_health` — so it must resolve to `None`.
        // An unpublishable port (0) is corrupt config, not a dead app: it is
        // rejected without probing at all (COX-B042).
        let healthy = match snapshot.host_port {
            Some(port) if is_publishable_host_port(port) => {
                self.deploy.health(port).await.unwrap_or(false)
            }
            _ => false,
        };
        resolve_repro_url(snapshot, healthy)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        resolve_repro_url, ReproSource, ReproUrl, ReproUrlSnapshot, ResolveReproUrlUseCase,
    };
    use crate::error::PortError;
    use crate::ports::outbound::{DeployPort, DeployReport};
    use coxagent_domain::{Role, Status, TicketType};
    use std::path::Path;
    use std::sync::Arc;

    /// Probe double whose liveness is scripted; nothing else can ever run —
    /// resolution must never deploy.
    struct ScriptedProbe(bool);

    #[async_trait::async_trait]
    impl DeployPort for ScriptedProbe {
        async fn deploy(&self, _work_dir: &Path) -> Result<DeployReport, PortError> {
            unreachable!("resolution never deploys")
        }
        async fn health(&self, _port: u16) -> Result<bool, PortError> {
            Ok(self.0)
        }
    }

    /// A probe that cannot run at all must read as unhealthy, never as a pass.
    struct ErroringProbe;

    #[async_trait::async_trait]
    impl DeployPort for ErroringProbe {
        async fn deploy(&self, _work_dir: &Path) -> Result<DeployReport, PortError> {
            unreachable!("resolution never deploys")
        }
        async fn health(&self, _port: u16) -> Result<bool, PortError> {
            Err(PortError::Backend("probe could not run".to_owned()))
        }
    }

    /// A probe that counts how often it was asked, so a test can prove an
    /// unpublishable port is never probed at all (COX-B042).
    struct CountingProbe {
        healthy: bool,
        probes: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl DeployPort for CountingProbe {
        async fn deploy(&self, _work_dir: &Path) -> Result<DeployReport, PortError> {
            unreachable!("resolution never deploys")
        }
        async fn health(&self, _port: u16) -> Result<bool, PortError> {
            self.probes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(self.healthy)
        }
    }

    /// A verify-pending ticket on the project port, as the endpoint assembles
    /// it from real config/store data.
    fn verify_pending(host_port: Option<u16>, preview_live: bool) -> ReproUrlSnapshot {
        ReproUrlSnapshot {
            host_port,
            preview_live,
            ticket_verify_pending: true,
        }
    }

    /// t1: an open-PR preview occupying a healthy port resolves the port URL,
    /// labeled `preview` — the reviewer must know they are looking at the PR.
    #[tokio::test]
    async fn a_live_preview_resolves_the_port_url_labeled_as_preview() {
        let uc = ResolveReproUrlUseCase::new(Arc::new(ScriptedProbe(true)));

        let resolved = uc.execute(&verify_pending(Some(8101), true)).await;

        assert_eq!(
            resolved,
            Some(ReproUrl {
                url: "http://localhost:8101/".to_owned(),
                source: ReproSource::Preview,
            })
        );
    }

    /// t2: no preview, merged main deployed and healthy — same port, labeled
    /// `main` so the reviewer knows what build they are reproducing against.
    #[tokio::test]
    async fn a_healthy_main_deployment_resolves_labeled_as_main() {
        let uc = ResolveReproUrlUseCase::new(Arc::new(ScriptedProbe(true)));

        let resolved = uc.execute(&verify_pending(Some(8101), false)).await;

        assert_eq!(
            resolved,
            Some(ReproUrl {
                url: "http://localhost:8101/".to_owned(),
                source: ReproSource::Main,
            })
        );
    }

    /// t3: nothing configured — no port, no URL, no panic. The common case
    /// for a project that never opted into publishing.
    #[tokio::test]
    async fn an_unconfigured_port_resolves_to_none_without_panicking() {
        let uc = ResolveReproUrlUseCase::new(Arc::new(ScriptedProbe(true)));

        let resolved = uc.execute(&verify_pending(None, false)).await;

        assert_eq!(resolved, None);
    }

    /// AC2: a configured port with nothing bound behind it must resolve to
    /// `None` — a verify dialog that fabricates a dead link sends the
    /// reviewer to a connection-refused page and calls it reproduction.
    #[tokio::test]
    async fn a_dead_port_resolves_to_none_instead_of_a_dead_link() {
        let uc = ResolveReproUrlUseCase::new(Arc::new(ScriptedProbe(false)));

        assert_eq!(uc.execute(&verify_pending(Some(8101), false)).await, None);
    }

    /// A probe that errors is not evidence the app is up — same posture as
    /// the shared deploy health gate; it must never read as a pass.
    #[tokio::test]
    async fn an_erroring_probe_resolves_to_none() {
        let uc = ResolveReproUrlUseCase::new(Arc::new(ErroringProbe));

        assert_eq!(uc.execute(&verify_pending(Some(8101), false)).await, None);
    }

    /// The resolver exists for the verify gate only: a ticket that is not
    /// `Fixed` (here: already `Verified`) gets no URL even on a healthy port.
    #[tokio::test]
    async fn a_ticket_not_awaiting_verification_resolves_to_none() {
        let uc = ResolveReproUrlUseCase::new(Arc::new(ScriptedProbe(true)));
        let verified = ReproUrlSnapshot {
            host_port: Some(8101),
            preview_live: false,
            ticket_verify_pending: false,
        };

        assert_eq!(uc.execute(&verified).await, None);
    }

    /// COX-B042 parity: `host_port` 0 is the kernel's "any free port"
    /// sentinel — never a connectable address. A resolver handed it (a
    /// defaulted `Config` can carry it) must reject it as corrupt config
    /// without probing, exactly like the shared deploy health gate.
    #[tokio::test]
    async fn a_zero_host_port_is_rejected_without_probing() {
        let probe = Arc::new(CountingProbe {
            healthy: true,
            probes: std::sync::atomic::AtomicUsize::new(0),
        });
        let uc = ResolveReproUrlUseCase::new(Arc::clone(&probe) as Arc<dyn DeployPort>);

        let resolved = uc.execute(&verify_pending(Some(0), false)).await;

        assert_eq!(resolved, None);
        assert_eq!(
            probe.probes.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a port nothing can bind must not be probed at all"
        );
    }

    /// Defense in depth in the pure decision itself: even a caller that lies
    /// about health cannot turn the unpublishable sentinel into a link.
    #[test]
    fn the_pure_decision_rejects_an_unpublishable_port_even_if_told_healthy() {
        assert_eq!(
            resolve_repro_url(&verify_pending(Some(0), false), true),
            None
        );
    }

    /// The pure decision itself, over a struct literal alone (no probe): the
    /// preview candidate outranks main when both signals are present.
    #[test]
    fn the_pure_decision_prefers_the_preview_candidate() {
        let resolved = resolve_repro_url(&verify_pending(Some(8101), true), true);

        assert_eq!(resolved.map(|r| r.source), Some(ReproSource::Preview));
    }

    /// Regression (no enum drift): the verify-pending predicate leans on the
    /// bug lifecycle's `Fixed -> Verified` edge staying exactly where it is.
    /// If the transition table ever moves that edge, `Status::Fixed` stops
    /// meaning "awaiting human verification" and this resolver's predicate —
    /// and the inbox verify gate that shares it — silently rots.
    #[test]
    fn the_fixed_to_verified_edge_the_predicate_depends_on_is_unchanged() {
        let bug = TicketType::Bug;
        assert!(coxagent_domain::transitions::transition_allowed(
            bug,
            Status::Fixed,
            Status::Verified
        ));
        // A human (or QA) renders the verdict at the verify gate.
        assert!(coxagent_domain::transitions::can_transition(
            Role::Test,
            Status::Fixed,
            Status::Verified
        ));
        assert!(coxagent_domain::transitions::can_transition(
            Role::User,
            Status::Fixed,
            Status::Verified
        ));
        // Mid-life statuses never satisfy the verify-pending predicate.
        assert_ne!(Status::InProgress, Status::Fixed);
        assert_ne!(Status::Verified, Status::Fixed);
    }
}
