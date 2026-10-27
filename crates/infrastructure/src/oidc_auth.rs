//! OIDC id-token verification (SSO slice 2).
//!
//! Verifies an RS256 id-token against the IdP's published JWKS and validates
//! the standard claims, then hands back the identity the hub needs (subject,
//! email, groups). The signature check uses `ring`'s vetted
//! `RSA_PKCS1_2048_8192_SHA256` primitive — this module only parses the JWT
//! envelope and enforces the claim rules; it does not hand-roll RSA math.
//!
//! The only IO is [`fetch_jwks`] (an HTTP GET of the discovery doc + JWKS);
//! [`verify_id_token`] is pure given the keys, so the security-critical path is
//! unit-tested end to end with a real RS256 vector.

use base64::{decode_config, encode_config};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{RsaPublicKeyComponents, RSA_PKCS1_2048_8192_SHA256};
use ring::digest;
use serde::Deserialize;

use coxagent_application::oidc::OidcConfig;

/// Clock-skew tolerance for exp/nbf, in seconds.
const LEEWAY_SECS: i64 = 60;

/// The verified identity extracted from a valid id-token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedIdentity {
    pub subject: String,
    pub email: Option<String>,
    pub groups: Vec<String>,
}

/// One RSA signing key from the IdP JWKS (base64url `n`/`e`).
#[derive(Debug, Clone, Deserialize)]
pub struct Jwk {
    #[serde(default)]
    pub kid: String,
    pub n: String,
    pub e: String,
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Discovery {
    jwks_uri: String,
    #[serde(default)]
    authorization_endpoint: String,
    #[serde(default)]
    token_endpoint: String,
}

/// The IdP endpoints the login flow needs, from the discovery document.
#[derive(Debug, Clone)]
pub struct OidcEndpoints {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
}

fn b64u(s: &str) -> Result<Vec<u8>, String> {
    decode_config(s, base64::URL_SAFE_NO_PAD).map_err(|e| format!("base64url: {e}"))
}

/// Verify an RS256 id-token and return the identity. Pure given `jwks` and
/// `now_unix`. `expected_nonce` (when `Some`) must equal the token's `nonce`.
///
/// # Errors
/// A string naming the first failed check (bad shape, unknown alg/kid, invalid
/// signature, wrong issuer/audience, expired/not-yet-valid, nonce mismatch,
/// missing subject). The errors are deliberately coarse — never echo claim
/// contents to the caller.
pub fn verify_id_token(
    token: &str,
    cfg: &OidcConfig,
    jwks: &[Jwk],
    now_unix: i64,
    expected_nonce: Option<&str>,
) -> Result<VerifiedIdentity, String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err("id-token: expected three segments".to_owned());
    }
    let (enc_header, enc_payload, enc_sig) = (parts[0], parts[1], parts[2]);

    // Header: RS256 only; pick the key by kid when the header names one.
    let header: serde_json::Value = serde_json::from_slice(&b64u(enc_header)?)
        .map_err(|_| "id-token: bad header json".to_owned())?;
    if header.get("alg").and_then(serde_json::Value::as_str) != Some("RS256") {
        return Err("id-token: unsupported alg (RS256 only)".to_owned());
    }
    let kid = header.get("kid").and_then(serde_json::Value::as_str);
    let jwk = match kid {
        Some(k) => jwks.iter().find(|j| j.kid == k),
        None => jwks.first(),
    }
    .ok_or_else(|| "id-token: no matching JWKS key".to_owned())?;

    // Signature over "<header>.<payload>" with the JWK's RSA public key.
    let signing_input = format!("{enc_header}.{enc_payload}");
    let sig = b64u(enc_sig)?;
    let pubkey = RsaPublicKeyComponents {
        n: b64u(&jwk.n)?,
        e: b64u(&jwk.e)?,
    };
    pubkey
        .verify(
            &RSA_PKCS1_2048_8192_SHA256,
            signing_input.as_bytes(),
            &sig,
        )
        .map_err(|_| "id-token: invalid signature".to_owned())?;

    // Claims — only after the signature is proven.
    let claims: serde_json::Value = serde_json::from_slice(&b64u(enc_payload)?)
        .map_err(|_| "id-token: bad claims json".to_owned())?;

    if claims.get("iss").and_then(serde_json::Value::as_str) != Some(cfg.issuer.as_str()) {
        return Err("id-token: issuer mismatch".to_owned());
    }
    if !audience_matches(claims.get("aud"), &cfg.client_id) {
        return Err("id-token: audience mismatch".to_owned());
    }
    let exp = claims
        .get("exp")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| "id-token: missing exp".to_owned())?;
    if now_unix > exp + LEEWAY_SECS {
        return Err("id-token: expired".to_owned());
    }
    if let Some(nbf) = claims.get("nbf").and_then(serde_json::Value::as_i64) {
        if now_unix + LEEWAY_SECS < nbf {
            return Err("id-token: not yet valid".to_owned());
        }
    }
    if let Some(want) = expected_nonce {
        if claims.get("nonce").and_then(serde_json::Value::as_str) != Some(want) {
            return Err("id-token: nonce mismatch".to_owned());
        }
    }

    let subject = claims
        .get("sub")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "id-token: missing subject".to_owned())?
        .to_owned();
    let email = claims
        .get("email")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    let groups = claims
        .get(&cfg.group_claim)
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default();

    Ok(VerifiedIdentity {
        subject,
        email,
        groups,
    })
}

/// `aud` may be a single string or an array; the client id must be present.
fn audience_matches(aud: Option<&serde_json::Value>, client_id: &str) -> bool {
    match aud {
        Some(serde_json::Value::String(s)) => s == client_id,
        Some(serde_json::Value::Array(a)) => {
            a.iter().any(|v| v.as_str() == Some(client_id))
        }
        _ => false,
    }
}

/// Fetch the IdP's signing keys: discovery doc → `jwks_uri` → JWKS. The only
/// IO in this module.
///
/// # Errors
/// A string when the HTTP fetch or JSON parse fails.
pub async fn fetch_jwks(issuer: &str) -> Result<Vec<Jwk>, String> {
    let disco_url = format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("oidc http client: {e}"))?;
    let disco: Discovery = client
        .get(&disco_url)
        .send()
        .await
        .map_err(|e| format!("oidc discovery: {e}"))?
        .json()
        .await
        .map_err(|e| format!("oidc discovery json: {e}"))?;
    let jwks: Jwks = client
        .get(&disco.jwks_uri)
        .send()
        .await
        .map_err(|e| format!("oidc jwks: {e}"))?
        .json()
        .await
        .map_err(|e| format!("oidc jwks json: {e}"))?;
    Ok(jwks.keys)
}

/// Fetch the IdP's authorize/token/jwks endpoints from its discovery document.
///
/// # Errors
/// A string when the HTTP fetch or JSON parse fails.
pub async fn fetch_endpoints(issuer: &str) -> Result<OidcEndpoints, String> {
    let disco_url = format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("oidc http client: {e}"))?;
    let disco: Discovery = client
        .get(&disco_url)
        .send()
        .await
        .map_err(|e| format!("oidc discovery: {e}"))?
        .json()
        .await
        .map_err(|e| format!("oidc discovery json: {e}"))?;
    Ok(OidcEndpoints {
        authorization_endpoint: disco.authorization_endpoint,
        token_endpoint: disco.token_endpoint,
        jwks_uri: disco.jwks_uri,
    })
}

/// A URL-safe random token (256 bits, base64url) for `state`, `nonce`, and the
/// PKCE verifier.
///
/// # Errors
/// A string if the system RNG fails (effectively never).
pub fn random_token() -> Result<String, String> {
    let mut buf = [0u8; 32];
    SystemRandom::new()
        .fill(&mut buf)
        .map_err(|_| "oidc: system RNG failure".to_owned())?;
    Ok(encode_config(buf, base64::URL_SAFE_NO_PAD))
}

/// The PKCE S256 code challenge for a verifier: base64url(SHA256(verifier)).
#[must_use]
pub fn pkce_challenge(verifier: &str) -> String {
    let d = digest::digest(&digest::SHA256, verifier.as_bytes());
    encode_config(d.as_ref(), base64::URL_SAFE_NO_PAD)
}

/// Percent-encode a query-parameter value (RFC 3986 unreserved pass through).
fn pct(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// Build the IdP authorization-code + PKCE redirect URL.
#[must_use]
pub fn authorize_url(
    authorization_endpoint: &str,
    cfg: &OidcConfig,
    state: &str,
    nonce: &str,
    code_challenge: &str,
) -> String {
    let params = [
        ("response_type", "code"),
        ("client_id", cfg.client_id.as_str()),
        ("redirect_uri", cfg.redirect_uri.as_str()),
        ("scope", "openid email profile groups"),
        ("state", state),
        ("nonce", nonce),
        ("code_challenge", code_challenge),
        ("code_challenge_method", "S256"),
    ];
    let qs = params
        .iter()
        .map(|(k, v)| format!("{k}={}", pct(v)))
        .collect::<Vec<_>>()
        .join("&");
    let sep = if authorization_endpoint.contains('?') {
        "&"
    } else {
        "?"
    };
    format!("{authorization_endpoint}{sep}{qs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    // A real RS256 vector minted with openssl (2048-bit key). Claims:
    // iss=https://idp.test aud=client123 sub=user-1 email=u@test.example
    // groups=[viewer,admin] exp=4102444800 nbf/iat=1700000000 nonce=nonce-1.
    const TOKEN: &str = "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCIsImtpZCI6InRlc3RrZXkifQ.eyJpc3MiOiJodHRwczovL2lkcC50ZXN0IiwiYXVkIjoiY2xpZW50MTIzIiwic3ViIjoidXNlci0xIiwiZW1haWwiOiJ1QHRlc3QuZXhhbXBsZSIsImdyb3VwcyI6WyJ2aWV3ZXIiLCJhZG1pbiJdLCJleHAiOjQxMDI0NDQ4MDAsImlhdCI6MTcwMDAwMDAwMCwibmJmIjoxNzAwMDAwMDAwLCJub25jZSI6Im5vbmNlLTEifQ.KQi-x5x7ri9l12HypqdK6mfTcyJlyKa9cchyg0kIoBu_qyz5HPifXaxjr3i5JWJY3Aq6M-bGhRtrXGvlQt27lJmxv9M1GfJnMqx3ogfh2Vm4Fisq021RMR7wPnw6ldgyfI2zJLCGmAkKvW5La2zKCIB9AfKA0cZ6yrSrCpaY-OzCQhNGXvF-4HEzNdOoITzL-MO2VjwteFzTqJXp82twsC7TKbxl5XfwXbPaDpeHO-KjYX2cNIkDxVhPqZ5G7XmLuiECQ0qtGKAOp8CA5sBMcAjBSHqMOFvYB5emb2ofBkp1xaarP3sXeRra8trKrPRhJaGuBhU6xaKh-VaIgbd-oQ";
    const N_B64U: &str = "p1Yf_wdjbii58GyLYgn9Us243OIC0UGYXx3Yt5Nds8vAFNzLDkQMVgvKC5jX1oDi6AgzOEWTb8y-70xWm3NiD6XJ61ApdHTk19dL9kkoPxoerRRlu4vcfMX-eZuYKGwBqZXnPqRAScvt3jsnCrqE7yHlXTj83msuUJV15f2xaGnseUBjiZn9IsNQsNurACGCEE26BjqQOxdlN7gzND1_Zm67eo2hYHEgjERk0p5cX0J5t-kX7IKbwmITmOx2aGgk-k_14IuBuogDgJ7HGB5BROOxFz5PmJfImJp23uYq98QNIqh7RokFEOB24WrATLdcy8rC3IwQWyO9ONXg-n2inQ";

    fn jwks() -> Vec<Jwk> {
        vec![Jwk {
            kid: "testkey".to_owned(),
            n: N_B64U.to_owned(),
            e: "AQAB".to_owned(),
        }]
    }

    fn cfg() -> OidcConfig {
        OidcConfig {
            issuer: "https://idp.test".to_owned(),
            client_id: "client123".to_owned(),
            client_secret: "secret".to_owned(),
            redirect_uri: "https://hub.test/cb".to_owned(),
            group_claim: "groups".to_owned(),
        }
    }

    const NOW: i64 = 1_700_000_100; // just after iat, long before exp

    #[test]
    fn accepts_a_valid_token_and_extracts_identity() {
        let id = verify_id_token(TOKEN, &cfg(), &jwks(), NOW, Some("nonce-1")).unwrap();
        assert_eq!(id.subject, "user-1");
        assert_eq!(id.email.as_deref(), Some("u@test.example"));
        assert_eq!(id.groups, vec!["viewer".to_owned(), "admin".to_owned()]);
    }

    #[test]
    fn rejects_a_tampered_signature() {
        // Flip the last char of the signature segment.
        let mut t = TOKEN.to_owned();
        let last = t.pop().unwrap();
        t.push(if last == 'Q' { 'R' } else { 'Q' });
        assert!(verify_id_token(&t, &cfg(), &jwks(), NOW, None).is_err());
    }

    #[test]
    fn rejects_a_tampered_payload() {
        // Re-sign is impossible without the key: changing a claim byte breaks
        // the signature. Swap a char in the payload segment.
        let parts: Vec<&str> = TOKEN.split('.').collect();
        let mut payload = parts[1].to_owned();
        payload.replace_range(0..1, "X");
        let forged = format!("{}.{}.{}", parts[0], payload, parts[2]);
        assert!(verify_id_token(&forged, &cfg(), &jwks(), NOW, None).is_err());
    }

    #[test]
    fn rejects_wrong_audience() {
        let mut c = cfg();
        c.client_id = "someone-else".to_owned();
        assert_eq!(
            verify_id_token(TOKEN, &c, &jwks(), NOW, None).unwrap_err(),
            "id-token: audience mismatch"
        );
    }

    #[test]
    fn rejects_wrong_issuer() {
        let mut c = cfg();
        c.issuer = "https://evil.test".to_owned();
        assert_eq!(
            verify_id_token(TOKEN, &c, &jwks(), NOW, None).unwrap_err(),
            "id-token: issuer mismatch"
        );
    }

    #[test]
    fn rejects_an_expired_token() {
        let err = verify_id_token(TOKEN, &cfg(), &jwks(), 4_200_000_000, None).unwrap_err();
        assert_eq!(err, "id-token: expired");
    }

    #[test]
    fn rejects_a_nonce_mismatch() {
        assert_eq!(
            verify_id_token(TOKEN, &cfg(), &jwks(), NOW, Some("other")).unwrap_err(),
            "id-token: nonce mismatch"
        );
    }

    #[test]
    fn rejects_an_unknown_kid() {
        let other = vec![Jwk {
            kid: "different".to_owned(),
            n: N_B64U.to_owned(),
            e: "AQAB".to_owned(),
        }];
        assert_eq!(
            verify_id_token(TOKEN, &cfg(), &other, NOW, None).unwrap_err(),
            "id-token: no matching JWKS key"
        );
    }

    #[test]
    fn pkce_challenge_matches_the_rfc7636_vector() {
        // RFC 7636 Appendix B.
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn random_token_is_unique_and_url_safe() {
        let a = random_token().unwrap();
        let b = random_token().unwrap();
        assert_ne!(a, b);
        // base64url(32 bytes) with no padding = 43 chars, all URL-safe.
        assert_eq!(a.len(), 43);
        assert!(a.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
    }

    #[test]
    fn authorize_url_carries_the_pkce_and_oidc_params() {
        let url = authorize_url(
            "https://idp.test/authorize",
            &cfg(),
            "state123",
            "nonce123",
            "chal123",
        );
        assert!(url.starts_with("https://idp.test/authorize?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=client123"));
        assert!(url.contains("code_challenge=chal123"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=state123"));
        // redirect_uri is percent-encoded (its ':' and '/').
        assert!(url.contains("redirect_uri=https%3A%2F%2Fhub.test%2Fcb"));
    }

    #[test]
    fn authorize_url_appends_with_ampersand_when_endpoint_has_a_query() {
        let url = authorize_url("https://idp.test/authorize?foo=1", &cfg(), "s", "n", "c");
        assert!(url.contains("/authorize?foo=1&response_type=code"));
    }
}
