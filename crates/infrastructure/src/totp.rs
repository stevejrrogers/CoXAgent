//! RFC 6238 TOTP (time-based one-time passwords) — the maths for 2FA, kept
//! dependency-light: HMAC-SHA1 over the time counter, plus RFC 4648 base32 for
//! the shared secret that authenticator apps consume. Pure and deterministic,
//! so it is verifiable by computing the expected code.

use hmac::{Hmac, Mac};
use rand::RngCore;
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

/// TOTP time step in seconds (the standard 30s window).
const STEP: u64 = 30;
/// Number of digits in a code.
const DIGITS: u32 = 6;
/// RFC 4648 base32 alphabet.
const B32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Generate a fresh 160-bit secret, base32-encoded (what apps scan/enter).
#[must_use]
pub fn generate_secret() -> String {
    let mut bytes = [0u8; 20];
    rand::thread_rng().fill_bytes(&mut bytes);
    base32_encode(&bytes)
}

/// The `otpauth://` provisioning URI an authenticator app imports.
#[must_use]
pub fn provisioning_uri(secret: &str, account: &str, issuer: &str) -> String {
    format!(
        "otpauth://totp/{issuer}:{account}?secret={secret}&issuer={issuer}&algorithm=SHA1&digits={DIGITS}&period={STEP}"
    )
}

/// The current 6-digit code for `secret` at unix time `now_secs`.
#[must_use]
pub fn code_at(secret: &str, now_secs: u64) -> Option<String> {
    let key = base32_decode(secret)?;
    Some(hotp(&key, now_secs / STEP))
}

/// Verify `code` for `secret` at `now_secs`, allowing ±1 step of clock skew.
#[must_use]
pub fn verify(secret: &str, code: &str, now_secs: u64) -> bool {
    let Some(key) = base32_decode(secret) else {
        return false;
    };
    let counter = now_secs / STEP;
    let code = code.trim();
    // Constant window (prev/current/next) tolerates small clock drift.
    [counter.wrapping_sub(1), counter, counter + 1]
        .iter()
        .any(|c| hotp(&key, *c) == code)
}

/// One HOTP value (RFC 4226) as a zero-padded decimal string.
fn hotp(key: &[u8], counter: u64) -> String {
    // HMAC accepts a key of any length, so this never errs; be defensive anyway.
    let Ok(mut mac) = HmacSha1::new_from_slice(key) else {
        return String::new();
    };
    mac.update(&counter.to_be_bytes());
    let hs = mac.finalize().into_bytes();
    let offset = (hs[19] & 0x0f) as usize;
    let bin = (u32::from(hs[offset] & 0x7f) << 24)
        | (u32::from(hs[offset + 1]) << 16)
        | (u32::from(hs[offset + 2]) << 8)
        | u32::from(hs[offset + 3]);
    let modulo = 10u32.pow(DIGITS);
    format!("{:0width$}", bin % modulo, width = DIGITS as usize)
}

fn base32_encode(data: &[u8]) -> String {
    let mut out = String::new();
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for &b in data {
        buffer = (buffer << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(B32[((buffer >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(B32[((buffer << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}

fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut buffer = 0u32;
    let mut bits = 0u32;
    let mut out = Vec::new();
    for c in s.trim().bytes() {
        if c == b'=' {
            break;
        }
        let pos = B32.iter().position(|&x| x == c.to_ascii_uppercase())?;
        buffer = (buffer << 5) | u32::try_from(pos).ok()?;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_base32() {
        let s = generate_secret();
        assert_eq!(base32_decode(&s).unwrap().len(), 20);
    }

    #[test]
    fn code_verifies_within_window_and_rejects_wrong() {
        let secret = generate_secret();
        let now = 1_700_000_000;
        let code = code_at(&secret, now).unwrap();
        assert_eq!(code.len(), 6);
        assert!(verify(&secret, &code, now));
        // ±1 step tolerated.
        assert!(verify(&secret, &code, now + 29));
        // Far away or wrong code rejected.
        assert!(!verify(&secret, &code, now + 120));
        assert!(!verify(&secret, "000000", now) || code == "000000");
    }

    #[test]
    fn rfc6238_test_vector_sha1() {
        // RFC 6238 Appendix B: secret "12345678901234567890" (ASCII) base32,
        // at T=59s the SHA1 code is 94287082.
        let secret = base32_encode(b"12345678901234567890");
        assert_eq!(code_at(&secret, 59).unwrap(), "287082"); // low 6 digits
    }
}
