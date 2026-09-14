//! How the hub builds a Postgres pool (CXA-C018) — the one place that turns a
//! libpq DSN into a [`deadpool_postgres::Pool`] with the right TLS connector.
//!
//! TLS is configured with the standard libpq query params on the DSN itself —
//! no env vars, no stored config, so the DSN stays the sole configuration
//! carrier and rollback is "remove `sslmode` and restart":
//!
//! | `sslmode`     | connector                                          |
//! |---------------|----------------------------------------------------|
//! | (absent)      | plaintext — byte-for-byte today's behavior          |
//! | `disable`     | plaintext                                           |
//! | `prefer`      | plaintext + warn (opportunistic TLS not attempted)  |
//! | `require`     | rustls, encrypt-only (no server verification) + warn |
//! | `verify-ca`   | rustls, chain + hostname verification               |
//! | `verify-full` | rustls, chain + hostname verification               |
//!
//! `sslrootcert=<path>` names a PEM root-CA bundle. It is read at pool-BUILD
//! time (fail-fast: an unreadable or empty bundle refuses the boot instead of
//! failing the first query). `require` with a bundle verifies the chain —
//! libpq's documented require+rootcert == verify-ca. verify-ca and verify-full
//! both use rustls's full verification, which also checks the hostname:
//! slightly stricter than libpq's verify-ca, never weaker. Without
//! `sslrootcert`, verify modes trust the compiled-in Mozilla root store, so
//! `sslmode=verify-full` works out of the box against managed Postgres whose
//! certs chain to a public CA.
//!
//! Keyword-form DSNs (`host=db sslmode=require`) pass through untouched with
//! the legacy plaintext connector: this repo only ever passes URL-form DSNs,
//! and refusing to override what we did not parse beats silently downgrading
//! an explicit `sslmode=require` (which then fails loudly at connect, exactly
//! as it does today).

use std::path::PathBuf;
use std::sync::Arc;

use deadpool_postgres::{Config, Pool, Runtime, SslMode};
use percent_encoding::percent_decode_str;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use tokio_postgres::NoTls;
use tokio_postgres_rustls::MakeRustlsConnect;

/// The five libpq `sslmode` levels a DSN can request. tokio-postgres 0.7's
/// `SslMode` only models the TLS *negotiation* gate (Disable/Prefer/Require —
/// it has no verify variants), so the verify levels are carried here and
/// collapse to `SslMode::Require` on the wire; the extra strictness is
/// enforced by the connector, not the negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TlsMode {
    Disable,
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

impl From<TlsMode> for SslMode {
    fn from(mode: TlsMode) -> Self {
        match mode {
            TlsMode::Disable => SslMode::Disable,
            TlsMode::Prefer => SslMode::Prefer,
            TlsMode::Require | TlsMode::VerifyCa | TlsMode::VerifyFull => SslMode::Require,
        }
    }
}

/// The pure TLS decision for one DSN: which sslmode level was requested and,
/// for verify modes, which PEM root bundle to verify against.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TlsPlan {
    mode: TlsMode,
    root_cert: Option<PathBuf>,
}

/// Build the deadpool pool for `dsn`, choosing the TLS connector from the
/// DSN's own `sslmode` / `sslrootcert` params (see the module docs).
///
/// # Errors
/// Labeled `{label} pool: ...` when pool construction fails, `sslmode` is not
/// a libpq-defined value, or a configured `sslrootcert` bundle is unreadable
/// or contains no certificates — a verify DSN must fail the boot, not quietly
/// come up plaintext or unverifiable.
#[allow(clippy::unused_async)] // design-pinned async surface; stays await-compatible
pub(crate) async fn pool(dsn: &str, label: &'static str) -> Result<Pool, String> {
    let plan = tls_plan(dsn).map_err(|e| format!("{label} pool: {e}"))?;
    let mut cfg = Config::new();
    if dsn.contains('?') {
        // URL form: tokio-postgres must never see the TLS params — it rejects
        // verify-* sslmode values outright and errors on the unknown
        // `sslrootcert` key. The connector below owns the whole decision, and
        // the explicit ssl_mode override stops a stripped `verify-*`/`require`
        // from silently decaying to tokio's `Prefer` default, which falls back
        // to plaintext when the connector cannot negotiate TLS.
        cfg.url = Some(strip_tls_params(dsn));
        cfg.ssl_mode = Some(SslMode::from(plan.mode));
        if plan.mode == TlsMode::Prefer {
            tracing::warn!(
                pool = label,
                "sslmode=prefer: opportunistic TLS is not attempted; the connection is \
                 plaintext — use sslmode=require (or verify-full) for encryption"
            );
        }
        if plan.mode == TlsMode::Require && plan.root_cert.is_none() {
            tracing::warn!(
                pool = label,
                "sslmode=require without sslrootcert: the connection is encrypted but the \
                 server identity is NOT verified — add sslrootcert (or use verify-full) to \
                 pin the server"
            );
        }
    } else {
        // Keyword-form DSN (or a bare URL): handed through byte-for-byte with
        // the legacy plaintext connector — never override what we did not
        // parse, so the DSN's own sslmode (if any) still governs at connect.
        cfg.url = Some(dsn.to_owned());
    }
    match connector(&plan).map_err(|e| format!("{label} pool: {e}"))? {
        Connector::Plain => cfg.create_pool(Some(Runtime::Tokio1), NoTls),
        Connector::Rustls(make) => cfg.create_pool(Some(Runtime::Tokio1), make),
    }
    .map_err(|e| format!("{label} pool: {e}"))
}

/// Pure: parse `sslmode` / `sslrootcert` out of a URL-form DSN's query string.
/// No IO — a root bundle is only read at pool-build time. Keys and values are
/// percent-decoded before matching, mirroring tokio-postgres's own URL parser;
/// a repeated param follows libpq's last-one-wins.
///
/// # Errors
/// An `sslmode` value libpq does not define is refused here so a typo can
/// never degrade to plaintext (`sslmode=requre` must fail the boot).
fn tls_plan(dsn: &str) -> Result<TlsPlan, String> {
    let mut plan = TlsPlan {
        mode: TlsMode::Disable,
        root_cert: None,
    };
    let Some((_, query)) = dsn.split_once('?') else {
        // Keyword-form DSN (or a bare URL): nothing this module acts on.
        return Ok(plan);
    };
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decoded(key);
        match key.as_str() {
            "sslmode" => {
                let value = percent_decoded(value);
                plan.mode = match value.as_str() {
                    "disable" => TlsMode::Disable,
                    "prefer" => TlsMode::Prefer,
                    "require" => TlsMode::Require,
                    "verify-ca" => TlsMode::VerifyCa,
                    "verify-full" => TlsMode::VerifyFull,
                    other => {
                        return Err(format!(
                            "invalid sslmode '{other}' (expected \
                             disable|prefer|require|verify-ca|verify-full)"
                        ));
                    }
                };
            }
            "sslrootcert" => plan.root_cert = Some(PathBuf::from(percent_decoded(value))),
            _ => {}
        }
    }
    Ok(plan)
}

/// Pure: drop `sslmode` / `sslrootcert` from a URL-form DSN so the rest can be
/// handed to tokio-postgres untouched (byte-for-byte, order preserved).
fn strip_tls_params(dsn: &str) -> String {
    let Some((head, query)) = dsn.split_once('?') else {
        return dsn.to_owned();
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let key = pair.split('=').next().unwrap_or(pair);
            !matches!(percent_decoded(key).as_str(), "sslmode" | "sslrootcert")
        })
        .collect();
    if kept.is_empty() {
        head.to_owned()
    } else {
        format!("{head}?{}", kept.join("&"))
    }
}

/// Percent-decode a DSN key/value the way tokio-postgres does before matching.
fn percent_decoded(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

/// The two connector shapes a plan can call for, so `pool` has one
/// `create_pool` per arm (deadpool's `Manager` type-erases the connector, so
/// `Pool` itself never genericizes).
enum Connector {
    Plain,
    Rustls(MakeRustlsConnect),
}

/// Choose the connector the plan calls for. The only IO is reading a
/// configured root bundle — at build time, so a bad path fails the boot.
fn connector(plan: &TlsPlan) -> Result<Connector, String> {
    match plan.mode {
        TlsMode::Disable | TlsMode::Prefer => Ok(Connector::Plain),
        TlsMode::Require if plan.root_cert.is_none() => Ok(Connector::Rustls(
            MakeRustlsConnect::new(encrypt_only_config()?),
        )),
        TlsMode::Require | TlsMode::VerifyCa | TlsMode::VerifyFull => Ok(Connector::Rustls(
            MakeRustlsConnect::new(verified_config(plan.root_cert.as_ref())?),
        )),
    }
}

/// The workspace's rustls crypto provider — ring, pinned because the rustls
/// default (aws-lc-rs) needs cmake/nasm to build and must not enter the tree.
fn ring_provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// rustls client config builder over [`ring_provider`], with protocol
/// versions resolved (the only step that can fail).
fn client_config_builder(
    provider: &Arc<CryptoProvider>,
) -> Result<rustls::ConfigBuilder<rustls::ClientConfig, rustls::WantsVerifier>, String> {
    rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(rustls::ALL_VERSIONS)
        .map_err(|e| format!("rustls protocol versions: {e}"))
}

/// `sslmode=require` without a bundle: encrypt the wire, do not verify the
/// server — libpq's documented require semantics. The `dangerous` verifier is
/// scoped to exactly this mode.
fn encrypt_only_config() -> Result<rustls::ClientConfig, String> {
    let provider = ring_provider();
    Ok(client_config_builder(&provider)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert { provider }))
        .with_no_client_auth())
}

/// Verifying config: chain verification against the configured bundle, or the
/// compiled-in Mozilla roots when the DSN names none.
fn verified_config(root_cert: Option<&PathBuf>) -> Result<rustls::ClientConfig, String> {
    let mut roots = rustls::RootCertStore::empty();
    match root_cert {
        Some(path) => {
            let pem =
                std::fs::read(path).map_err(|e| format!("sslrootcert {}: {e}", path.display()))?;
            let mut pem = std::io::BufReader::new(pem.as_slice());
            for cert in rustls_pemfile::certs(&mut pem) {
                let cert = cert.map_err(|e| format!("sslrootcert {}: {e}", path.display()))?;
                roots
                    .add(cert)
                    .map_err(|e| format!("sslrootcert {}: {e}", path.display()))?;
            }
            if roots.is_empty() {
                return Err(format!(
                    "sslrootcert {}: the bundle contains no certificates",
                    path.display()
                ));
            }
        }
        None => roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
    }
    Ok(client_config_builder(&ring_provider())?
        .with_root_certificates(roots)
        .with_no_client_auth())
}

/// `sslmode=require` verifier: accept any server certificate. TLS stays
/// mandatory (the negotiation gate is `SslMode::Require`); only the identity
/// check is skipped, mirroring libpq.
#[derive(Debug)]
struct AcceptAnyServerCert {
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    // --- tls_plan: the pure decision matrix ---------------------------------

    #[test]
    fn an_absent_sslmode_plans_plaintext_like_today() {
        let plan = tls_plan("postgres://u:p@db:5432/coxagent").unwrap();
        assert_eq!(
            plan,
            TlsPlan {
                mode: TlsMode::Disable,
                root_cert: None
            }
        );
    }

    #[test]
    fn disable_plans_plaintext() {
        let plan = tls_plan("postgres://u:p@db:5432/coxagent?sslmode=disable").unwrap();
        assert_eq!(plan.mode, TlsMode::Disable);
        assert_eq!(plan.root_cert, None);
    }

    #[test]
    fn prefer_plans_the_prefer_mode() {
        let plan = tls_plan("postgres://u:p@db:5432/coxagent?sslmode=prefer").unwrap();
        assert_eq!(plan.mode, TlsMode::Prefer);
    }

    #[test]
    fn require_plans_require() {
        let plan = tls_plan("postgres://u:p@db:5432/coxagent?sslmode=require").unwrap();
        assert_eq!(plan.mode, TlsMode::Require);
    }

    #[test]
    fn verify_modes_plan_their_own_level() {
        let ca = tls_plan("postgres://u:p@db:5432/coxagent?sslmode=verify-ca").unwrap();
        assert_eq!(ca.mode, TlsMode::VerifyCa);
        let full = tls_plan("postgres://u:p@db:5432/coxagent?sslmode=verify-full").unwrap();
        assert_eq!(full.mode, TlsMode::VerifyFull);
    }

    #[test]
    fn sslrootcert_is_extracted_bare_and_url_encoded() {
        let bare = tls_plan("postgres://u:p@db/db?sslmode=verify-full&sslrootcert=/etc/cox/ca.crt")
            .unwrap();
        assert_eq!(
            bare.root_cert.as_deref(),
            Some(std::path::Path::new("/etc/cox/ca.crt"))
        );
        let encoded =
            tls_plan("postgres://u:p@db/db?sslmode=verify-full&sslrootcert=%2Fetc%2Fcox%2Fca.crt")
                .unwrap();
        assert_eq!(encoded.root_cert, bare.root_cert);
    }

    #[test]
    fn an_unknown_sslmode_is_refused_not_downgraded_to_plaintext() {
        let err = tls_plan("postgres://u:p@db/db?sslmode=requre").unwrap_err();
        assert!(err.contains("sslmode"), "unclear failure: {err}");
    }

    #[test]
    fn a_repeated_sslmode_follows_libpq_last_one_wins() {
        let plan = tls_plan("postgres://u:p@db/db?sslmode=disable&sslmode=require").unwrap();
        assert_eq!(plan.mode, TlsMode::Require);
    }

    #[test]
    fn a_keyword_form_dsn_plans_nothing_and_is_passed_through() {
        // The pass-through contract: pool() must not override what this did
        // not parse (an override would silently downgrade the DSN's own
        // sslmode=require to plaintext).
        let plan = tls_plan("host=db port=5432 user=u password=p dbname=db sslmode=require");
        assert_eq!(
            plan.unwrap(),
            TlsPlan {
                mode: TlsMode::Disable,
                root_cert: None
            }
        );
    }

    // --- strip_tls_params: the sanitized URL --------------------------------

    #[test]
    fn only_the_tls_params_are_stripped() {
        let out = strip_tls_params(
            "postgres://u:p@db:5432/cox?sslmode=verify-full&sslrootcert=/ca.crt&application_name=hub",
        );
        assert_eq!(out, "postgres://u:p@db:5432/cox?application_name=hub");
    }

    #[test]
    fn a_dsn_without_a_query_string_is_untouched() {
        let dsn = "postgres://u:p@db:5432/coxagent";
        assert_eq!(strip_tls_params(dsn), dsn);
    }

    #[test]
    fn a_tls_only_query_leaves_no_stray_question_mark() {
        let out = strip_tls_params("postgres://u:p@db:5432/cox?sslmode=require");
        assert_eq!(out, "postgres://u:p@db:5432/cox");
    }

    #[test]
    fn the_sanitized_dsn_parses_cleanly_into_tokio_postgres_config() {
        // tokio-postgres 0.7 rejects verify-* sslmode values and the unknown
        // sslrootcert key outright, so the stripped form is what it must parse.
        let dsn = "postgres://u:p@127.0.0.1:5432/cox?sslmode=verify-full&sslrootcert=/ca.crt&application_name=hub";
        let cfg: tokio_postgres::Config = strip_tls_params(dsn).parse().unwrap();
        assert_eq!(cfg.get_dbname(), Some("cox"));
        assert_eq!(cfg.get_application_name(), Some("hub"));
        // And a DSN whose sslmode tokio does know parses with the param intact.
        let plain: tokio_postgres::Config = "postgres://u:p@127.0.0.1:5432/cox?sslmode=require"
            .parse()
            .unwrap();
        assert_eq!(
            plain.get_ssl_mode(),
            tokio_postgres::config::SslMode::Require
        );
    }

    // --- pool: the build-time contract ---------------------------------------
    // deadpool connects lazily, so an unroutable host exercises the full
    // build path with no server, no harness, no network.

    #[tokio::test]
    async fn a_disable_pool_builds_without_a_server() {
        pool("postgres://u:p@127.0.0.1:1/db?sslmode=disable", "test")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_prefer_pool_builds_plaintext_and_the_warn_documents_it() {
        pool("postgres://u:p@127.0.0.1:1/db?sslmode=prefer", "test")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_require_pool_builds_with_the_encrypt_only_connector_and_warns_it_is_unverified() {
        // No bundle: the connection is encrypted but the server is not pinned —
        // pool() emits a warn so the posture is never silent.
        pool("postgres://u:p@127.0.0.1:1/db?sslmode=require", "test")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn an_invalid_sslmode_fails_the_build_with_the_pool_label() {
        let err = pool("postgres://u:p@127.0.0.1:1/db?sslmode=requre", "state")
            .await
            .unwrap_err();
        assert!(
            err.starts_with("state pool: "),
            "failure is not labeled: {err}"
        );
        assert!(err.contains("requre"), "unclear failure: {err}");
    }

    #[tokio::test]
    async fn a_verify_full_pool_builds_against_the_bundled_roots() {
        pool("postgres://u:p@127.0.0.1:1/db?sslmode=verify-full", "test")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_missing_root_cert_fails_the_build_not_the_first_query() {
        let err = pool(
            "postgres://u:p@127.0.0.1:1/db?sslmode=verify-full&sslrootcert=/nonexistent/ca.crt",
            "test",
        )
        .await
        .unwrap_err();
        assert!(err.contains("test pool"), "failure is not labeled: {err}");
        assert!(err.contains("sslrootcert"), "unclear failure: {err}");
    }

    #[tokio::test]
    async fn an_empty_root_cert_bundle_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.crt");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"")
            .unwrap();
        let err = pool(
            &format!(
                "postgres://u:p@127.0.0.1:1/db?sslmode=verify-ca&sslrootcert={}",
                path.display()
            ),
            "test",
        )
        .await
        .unwrap_err();
        assert!(err.contains("no certificates"), "unclear failure: {err}");
    }

    #[tokio::test]
    async fn a_keyword_form_dsn_still_builds_the_legacy_pool() {
        // Passed through untouched; the DSN's own sslmode=require then fails
        // loudly at connect with the legacy plaintext connector — a refusal,
        // never a silent downgrade.
        pool(
            "host=127.0.0.1 port=1 user=u password=p dbname=db sslmode=require",
            "test",
        )
        .await
        .unwrap();
    }
}
