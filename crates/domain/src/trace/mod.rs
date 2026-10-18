//! Minimal W3C Trace Context — a pure span-propagation core.
//!
//! CoXAgent historically logged structured lines but carried no correlation id
//! across the hub → runner → engine boundary, so one request spanning several
//! services was untraceable end-to-end (CXA-C002). This module is the smallest
//! self-contained piece that fixes that: W3C [`TraceParent`] parse/serialize,
//! child-span derivation and fresh-root minting, with **zero IO** and **no
//! framework** dependency.
//!
//! # Layering
//! Per the hexagonal rules this lives in `crates/domain`: it depends only on
//! `std`, `serde`, `uuid` and `time`. Propagation over an actual HTTP transport
//! is an inbound-adapter concern (`crates/presentation/src/middleware/
//! trace_layer.rs`) built on top of these types; nothing here knows what axum or
//! reqwest are.
//!
//! # The decision rule
//! A middleware asks: *is there an incoming valid `traceparent`?* If yes it
//! continues that trace with a fresh child span; if no it mints a new root. That
//! decision is a pure function of a header value or its absence — see
//! [`TraceParent::parse`], [`child_of`] and [`mint_root`].

use serde::{Deserialize, Serialize};

/// Standard HTTP header carrying the active trace context.
pub const TRACEPARENT_HEADER: &str = "traceparent";
/// CORS header to expose when emitting our id behind CORS (see middleware).
pub const TRACEPARENT_EXPOSE_HEADERS: &str = "Access-Control-Expose-Headers";

const TRACE_ID_LEN: usize = 16;
const SPAN_ID_LEN: usize = 8;
const HEX_PER_BYTE: usize = 2;
const VERSION_LEN: usize = HEX_PER_BYTE; // "00"
const FLAGS_LEN: usize = HEX_PER_BYTE;

fn hex_char(v: u8) -> char {
    match v {
        0..=9 => (b'0' + v) as char,
        _ => (b'a' + v - 10) as char,
    }
}

fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * HEX_PER_BYTE);
    for &b in bytes {
        out.push(hex_char(b >> 4));
        out.push(hex_char(b & 0x0F));
    }
    out
}

fn decode_hex(src: &str) -> Option<Vec<u8>> {
    if src.len() % HEX_PER_BYTE != 0 {
        return None;
    }
    let bytes = src.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / HEX_PER_BYTE);
    let mut i = 0;
    while i < bytes.len() {
        let hi = hex_digit(bytes[i])?;
        let lo = hex_digit(bytes[i + 1])?;
        out.push((hi << 4) | lo);
        i += HEX_PER_BYTE;
    }
    Some(out)
}

fn all_zero(bytes: &[u8]) -> bool {
    bytes.iter().all(|&b| b == 0)
}

/// A globally-unique trace id — one per logical request tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraceId([u8; TRACE_ID_LEN]);

impl TraceId {
    /// Build from raw bytes. Zeroed ids are rejected via [`TryFrom`]/[`parse_wire`];
    /// this constructor trusts the caller (used by minting and tests).
    #[must_use]
    pub fn from_bytes(bytes: [u8; TRACE_ID_LEN]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn as_bytes(&self) -> [u8; TRACE_ID_LEN] {
        self.0
   }

}
