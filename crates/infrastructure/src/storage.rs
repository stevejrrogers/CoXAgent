//! Blob storage adapters behind `StoragePort`:
//! - [`LocalStorage`] writes files under a root directory (the default).
//! - [`S3Storage`] stores objects in an S3-compatible store (MinIO), signing
//!   requests with AWS Signature V4 over `reqwest` (no SDK dependency).

use async_trait::async_trait;
use coxagent_application::ports::outbound::StoragePort;
use coxagent_application::PortError;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

type HmacSha256 = Hmac<Sha256>;

/// Reject keys that could escape the storage root / bucket.
fn safe_key(key: &str) -> Result<&str, PortError> {
    if key.is_empty() || key.contains("..") || key.starts_with('/') {
        return Err(PortError::Backend(format!("bad storage key: {key}")));
    }
    Ok(key)
}

/// Local-disk storage rooted at a directory. Keys map to relative paths.
pub struct LocalStorage {
    root: PathBuf,
}

impl LocalStorage {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

#[async_trait]
impl StoragePort for LocalStorage {
    async fn put(&self, key: &str, data: &[u8], _mime: &str) -> Result<(), PortError> {
        let path = self.root.join(safe_key(key)?);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| PortError::Backend(e.to_string()))?;
        }
        std::fs::write(&path, data).map_err(|e| PortError::Backend(e.to_string()))
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, PortError> {
        let path = self.root.join(safe_key(key)?);
        std::fs::read(&path).map_err(|e| PortError::Backend(e.to_string()))
    }
}

/// S3-compatible object storage (MinIO), path-style addressing + SigV4.
pub struct S3Storage {
    client: reqwest::Client,
    /// Base endpoint, e.g. `http://127.0.0.1:9000` (no trailing slash).
    endpoint: String,
    bucket: String,
    region: String,
    access_key: String,
    secret_key: String,
}

impl S3Storage {
    /// Build from explicit settings. Reads nothing from the environment.
    #[must_use]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(
        endpoint: String,
        bucket: String,
        region: String,
        access_key: String,
        secret_key: String,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            bucket,
            region,
            access_key,
            secret_key,
        }
    }

    /// Construct from `COXAGENT_S3_*` env vars, or `None` if unconfigured.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let endpoint = std::env::var("COXAGENT_S3_ENDPOINT").ok()?;
        let bucket = std::env::var("COXAGENT_S3_BUCKET").unwrap_or_else(|_| "coxagent".to_owned());
        let region = std::env::var("COXAGENT_S3_REGION").unwrap_or_else(|_| "us-east-1".to_owned());
        let access_key = std::env::var("COXAGENT_S3_ACCESS_KEY").ok()?;
        let secret_key = std::env::var("COXAGENT_S3_SECRET_KEY").ok()?;
        Some(Self::new(endpoint, bucket, region, access_key, secret_key))
    }

    fn host(&self) -> String {
        self.endpoint
            .split("://")
            .nth(1)
            .unwrap_or(&self.endpoint)
            .to_owned()
    }

    /// Create the bucket if it doesn't exist (idempotent). Call once at startup.
    ///
    /// # Errors
    /// [`PortError`] on a transport error (an already-exists response is fine).
    pub async fn ensure_bucket(&self) -> Result<(), PortError> {
        let url = format!("{}/{}", self.endpoint, self.bucket);
        let resp = self
            .signed(
                reqwest::Method::PUT,
                &format!("/{}", self.bucket),
                &[],
                "application/xml",
                &url,
            )
            .await?;
        // 200 = created, 409 = already owned by us — both are success.
        let status = resp.status();
        if status.is_success() || status.as_u16() == 409 {
            Ok(())
        } else {
            Err(PortError::Backend(format!(
                "create bucket failed: {status}"
            )))
        }
    }

    /// Sign and send a request with an empty or byte body against `canonical_uri`.
    async fn signed(
        &self,
        method: reqwest::Method,
        canonical_uri: &str,
        body: &[u8],
        content_type: &str,
        url: &str,
    ) -> Result<reqwest::Response, PortError> {
        let amz_date = fmt_amz_datetime();
        let date_stamp = &amz_date[..8];
        let payload_hash = hex(&sha256(body));
        let host = self.host();

        // Canonical request.
        let canonical_headers =
            format!("host:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n");
        let signed_headers = "host;x-amz-content-sha256;x-amz-date";
        let canonical_request = format!(
            "{method}\n{canonical_uri}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
        );

        // String to sign.
        let scope = format!("{date_stamp}/{}/s3/aws4_request", self.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            hex(&sha256(canonical_request.as_bytes()))
        );

        // Signing key + signature.
        let k_date = hmac(
            format!("AWS4{}", self.secret_key).as_bytes(),
            date_stamp.as_bytes(),
        );
        let k_region = hmac(&k_date, self.region.as_bytes());
        let k_service = hmac(&k_region, b"s3");
        let k_signing = hmac(&k_service, b"aws4_request");
        let signature = hex(&hmac(&k_signing, string_to_sign.as_bytes()));

        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            self.access_key
        );

        let req = self
            .client
            .request(method, url)
            .header("Host", host)
            .header("x-amz-content-sha256", &payload_hash)
            .header("x-amz-date", &amz_date)
            .header("Authorization", authorization)
            .header("Content-Type", content_type)
            .body(body.to_vec());
        req.send()
            .await
            .map_err(|e| PortError::Backend(format!("s3 request: {e}")))
    }
}

#[async_trait]
impl StoragePort for S3Storage {
    async fn put(&self, key: &str, data: &[u8], mime: &str) -> Result<(), PortError> {
        let key = safe_key(key)?;
        let uri = format!("/{}/{}", self.bucket, uri_encode(key, false));
        let url = format!("{}{uri}", self.endpoint);
        let resp = self
            .signed(reqwest::Method::PUT, &uri, data, mime, &url)
            .await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(PortError::Backend(format!(
                "s3 put {key} failed: {}",
                resp.status()
            )))
        }
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, PortError> {
        let key = safe_key(key)?;
        let uri = format!("/{}/{}", self.bucket, uri_encode(key, false));
        let url = format!("{}{uri}", self.endpoint);
        let resp = self
            .signed(
                reqwest::Method::GET,
                &uri,
                &[],
                "application/octet-stream",
                &url,
            )
            .await?;
        if !resp.status().is_success() {
            return Err(PortError::Backend(format!(
                "s3 get {key} failed: {}",
                resp.status()
            )));
        }
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| PortError::Backend(e.to_string()))
    }
}

// ── SigV4 helpers ───────────────────────────────────────────────────────────

fn sha256(data: &[u8]) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().to_vec()
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    // HMAC accepts a key of any length, so this never errors in practice.
    let Ok(mut mac) = HmacSha256::new_from_slice(key) else {
        return Vec::new();
    };
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

const HEX_LOWER: &[u8; 16] = b"0123456789abcdef";
const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX_LOWER[(b >> 4) as usize] as char);
        s.push(HEX_LOWER[(b & 0xf) as usize] as char);
    }
    s
}

/// RFC 3986 URI-encoding as required by SigV4. When `encode_slash` is false,
/// `/` is left as-is (for object keys with path segments).
fn uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            b'/' if !encode_slash => out.push('/'),
            _ => {
                out.push('%');
                out.push(HEX_UPPER[(b >> 4) as usize] as char);
                out.push(HEX_UPPER[(b & 0xf) as usize] as char);
            }
        }
    }
    out
}

/// Current UTC time as `YYYYMMDDTHHMMSSZ` for the `x-amz-date` header.
#[allow(clippy::many_single_char_names, clippy::cast_possible_wrap)]
fn fmt_amz_datetime() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    // Civil-from-days (Howard Hinnant's algorithm) — no chrono dependency.
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (h, mi, se) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}{m:02}{d:02}T{h:02}{mi:02}{se:02}Z")
}
