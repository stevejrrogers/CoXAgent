//! The live reproduction URL for a project's fixed work (CXA-F242 / CXA-F244).
//!
//! One source of truth for "where does this project's fix run live": the same
//! `http://127.0.0.1:{host_port}/` base [`crate::use_cases::cycle::qa_evidence`]
//! captures its evidence against, derived purely from `config.deploy.host_port`.
//! A project with no `host_port` has no resolvable reproduction URL — callers
//! surface the field as null rather than inventing a link.

/// The live reproduction URL for a project deployed on `host_port`.
///
/// `Some("http://127.0.0.1:{port}/")` when the deploy port is configured —
/// the exact base `qa_evidence` captures against — and `None` otherwise, so
/// every verify-facing surface decides "resolvable or not" from one pure
/// function over the config, with no IO and no business rules of its own.
#[must_use]
pub fn compute_live_repro_url(host_port: Option<u16>) -> Option<String> {
    host_port.map(|port| format!("http://127.0.0.1:{port}/"))
}

#[cfg(test)]
mod tests {
    use super::compute_live_repro_url;

    #[test]
    fn a_configured_host_port_resolves_the_capture_base_url() {
        // The exact base qa_evidence captures screenshots against.
        assert_eq!(
            compute_live_repro_url(Some(8080)),
            Some("http://127.0.0.1:8080/".to_owned())
        );
    }

    #[test]
    fn no_host_port_is_not_resolvable() {
        assert_eq!(compute_live_repro_url(None), None);
    }
}
