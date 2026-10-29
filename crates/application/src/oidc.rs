//! SSO / OpenID Connect — configuration and IdP-group → [`AuthRole`] mapping.
//!
//! Self-hosted enterprise deployments gate on federated login (Okta / Entra /
//! Google Workspace), which the username/password `AuthPort` did not offer.
//! This module is the pure, IO-free foundation: how an operator turns SSO on,
//! and how an identity provider's group claims become a hub role. The
//! authorization-code/PKCE exchange and the JWKS id-token verification (both
//! IO) live in an infrastructure adapter built on top of this.
//!
//! SSO is OFF unless fully configured — [`OidcConfig::from_env`] returns `None`
//! when any required field is absent, so the live username/password flow is
//! untouched until an operator sets the issuer + client credentials.

use crate::auth::AuthRole;

/// An OIDC relying-party configuration, read from the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcConfig {
    /// The IdP issuer URL (its OIDC discovery document lives at
    /// `<issuer>/.well-known/openid-configuration`).
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    /// The hub's OIDC callback URL registered with the IdP.
    pub redirect_uri: String,
    /// The id-token claim that carries the user's groups (default `groups`).
    pub group_claim: String,
}

impl OidcConfig {
    /// Build from `COXAGENT_OIDC_*` env vars. Returns `None` — SSO disabled —
    /// unless issuer, client id, client secret and redirect URI are all set,
    /// so configuring nothing leaves the password flow in place.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let get = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        Some(Self {
            issuer: get("COXAGENT_OIDC_ISSUER")?,
            client_id: get("COXAGENT_OIDC_CLIENT_ID")?,
            client_secret: get("COXAGENT_OIDC_CLIENT_SECRET")?,
            redirect_uri: get("COXAGENT_OIDC_REDIRECT_URI")?,
            group_claim: get("COXAGENT_OIDC_GROUP_CLAIM").unwrap_or_else(|| "groups".to_owned()),
        })
    }
}

/// Privilege rank of a role — higher wins when an IdP user is in several
/// groups. Mirrors the capability tiers in [`AuthRole`] (super > admin > lead >
/// coordinator > reviewer/qa > member IC > viewer) so a group grant never
/// *lowers* a user who also holds a higher one.
#[must_use]
fn role_rank(role: AuthRole) -> u8 {
    match role {
        AuthRole::Super => 100,
        AuthRole::Admin => 90,
        AuthRole::Director | AuthRole::Manager => 80,
        AuthRole::TechLead | AuthRole::DsLead | AuthRole::DaLead => 70,
        AuthRole::Ba | AuthRole::Po | AuthRole::Sa | AuthRole::Sm => 60,
        AuthRole::Qa | AuthRole::Reviewer => 50,
        AuthRole::Fe
        | AuthRole::Be
        | AuthRole::Aie
        | AuthRole::Ds
        | AuthRole::Da
        | AuthRole::De => 40,
        AuthRole::Viewer => 10,
    }
}

/// Map an IdP user's group claims to a hub [`AuthRole`]: the most-privileged
/// role any group names (via [`AuthRole::from_str_lenient`], which already
/// accepts dotted/spaced labels). Unknown groups resolve to `Viewer`, so a
/// user with no recognized group gets the least privilege — never more.
#[must_use]
pub fn role_from_groups(groups: &[String]) -> AuthRole {
    groups
        .iter()
        .map(|g| AuthRole::from_str_lenient(g))
        .max_by_key(|r| role_rank(*r))
        .unwrap_or(AuthRole::Viewer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_groups_is_least_privilege() {
        assert_eq!(role_from_groups(&[]), AuthRole::Viewer);
    }

    #[test]
    fn unknown_groups_resolve_to_viewer() {
        let groups = vec!["engineering".to_owned(), "all-staff".to_owned()];
        assert_eq!(role_from_groups(&groups), AuthRole::Viewer);
    }

    #[test]
    fn the_most_privileged_group_wins_regardless_of_order() {
        let groups = vec!["viewer".to_owned(), "admin".to_owned(), "qa".to_owned()];
        assert_eq!(role_from_groups(&groups), AuthRole::Admin);
    }

    #[test]
    fn dotted_and_spaced_group_labels_are_accepted() {
        assert_eq!(
            role_from_groups(&["tech.lead".to_owned()]),
            AuthRole::TechLead
        );
        assert_eq!(
            role_from_groups(&["Super Admin".to_owned()]),
            AuthRole::Super
        );
    }

    #[test]
    fn from_env_is_none_without_full_config() {
        // No env set in this test process → disabled.
        // (Set/clear is process-global and racy under parallel tests, so we
        // only assert the "absent → None" contract here.)
        // SAFETY net: clear the required keys first in case the host has them.
        for k in [
            "COXAGENT_OIDC_ISSUER",
            "COXAGENT_OIDC_CLIENT_ID",
            "COXAGENT_OIDC_CLIENT_SECRET",
            "COXAGENT_OIDC_REDIRECT_URI",
        ] {
            std::env::remove_var(k);
        }
        assert!(OidcConfig::from_env().is_none());
    }
}
