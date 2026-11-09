//! Tamper-evidence for the Postgres audit log: a SHA-256 hash chain.
//!
//! Each row stores the hash of the row before it, so every entry's hash commits
//! to the entire history that preceded it. An attacker who edits, reorders,
//! deletes, or truncates rows directly in the database (bypassing the append
//! path) breaks the chain at the point of tampering, and [`verify_chain`]
//! reports the first broken link. This is tamper-*evidence*, not prevention —
//! it cannot stop a write, but it makes a silent rewrite detectable.
//!
//! The logic here is pure: the SQL adapter in `audit_sink` reads the chain tip,
//! calls [`row_hash`] to extend it, and persists `(prev, hash)`; verification
//! walks the rows and re-derives each hash. Keeping it separate makes both the
//! hashing and the verifier testable with plain struct literals.

use sha2::{Digest, Sha256};

/// The chain's genesis predecessor — the `prev` of the very first row. A fixed,
/// all-zero 64-hex string so the first row's hash still commits to a known
/// anchor rather than an empty input.
pub const GENESIS_PREV: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// One link's content as the chain sees it. Mirrors the persisted audit columns
/// (minus the chain fields themselves), so a change to any of them changes the
/// hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainedEntry {
    pub prev: String,
    pub at: String,
    pub user: String,
    pub action: String,
    pub status: u16,
    pub hash: String,
}

/// The hash of a row: `SHA-256(prev \n at \n user \n action \n status)`, hex.
///
/// Newlines separate the fields so no field's content can be shifted into
/// another (the fields never contain a raw newline in practice; even if one
/// did, `prev` is a fixed-width hex digest, which anchors the framing).
#[must_use]
pub fn row_hash(prev: &str, at: &str, user: &str, action: &str, status: u16) -> String {
    let mut h = Sha256::new();
    h.update(prev.as_bytes());
    h.update(b"\n");
    h.update(at.as_bytes());
    h.update(b"\n");
    h.update(user.as_bytes());
    h.update(b"\n");
    h.update(action.as_bytes());
    h.update(b"\n");
    h.update(status.to_string().as_bytes());
    to_hex(&h.finalize())
}

/// Lower-case hex of a byte slice (no `hex` crate dependency needed).
fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// Where and why the chain is broken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainBreak {
    /// 0-based position of the first row that fails to verify.
    pub index: usize,
    /// Human-readable reason, for the operator report.
    pub reason: String,
}

/// Walk `rows` in chronological order and confirm each links to the one before
/// it. Returns the number of verified rows, or the first break.
///
/// # Errors
/// [`ChainBreak`] naming the first row whose `prev` does not match the previous
/// row's hash, or whose stored `hash` is not the hash of its own content.
pub fn verify_chain(rows: &[ChainedEntry]) -> Result<usize, ChainBreak> {
    let mut expected_prev = GENESIS_PREV.to_owned();
    for (index, row) in rows.iter().enumerate() {
        if row.prev != expected_prev {
            return Err(ChainBreak {
                index,
                reason: format!(
                    "prev mismatch: row links to {} but the chain tip here is {}",
                    short(&row.prev),
                    short(&expected_prev)
                ),
            });
        }
        let want = row_hash(&row.prev, &row.at, &row.user, &row.action, row.status);
        if row.hash != want {
            return Err(ChainBreak {
                index,
                reason: format!(
                    "content was altered: stored hash {} ≠ recomputed {}",
                    short(&row.hash),
                    short(&want)
                ),
            });
        }
        expected_prev.clone_from(&row.hash);
    }
    Ok(rows.len())
}

/// First 8 hex chars, for compact, non-leaky reporting.
fn short(h: &str) -> &str {
    h.get(..8).unwrap_or(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(prev: &str, at: &str, user: &str, action: &str, status: u16) -> ChainedEntry {
        ChainedEntry {
            prev: prev.to_owned(),
            at: at.to_owned(),
            user: user.to_owned(),
            action: action.to_owned(),
            status,
            hash: row_hash(prev, at, user, action, status),
        }
    }

    fn well_formed() -> Vec<ChainedEntry> {
        let a = link(GENESIS_PREV, "t1", "root", "login", 200);
        let b = link(&a.hash, "t2", "root", "POST /pause", 200);
        let c = link(&b.hash, "t3", "eve", "login", 401);
        vec![a, b, c]
    }

    #[test]
    fn a_well_formed_chain_verifies() {
        assert_eq!(verify_chain(&well_formed()), Ok(3));
    }

    #[test]
    fn an_empty_log_verifies() {
        assert_eq!(verify_chain(&[]), Ok(0));
    }

    #[test]
    fn the_hash_is_stable_and_commits_to_every_field() {
        let base = row_hash(GENESIS_PREV, "t", "u", "a", 200);
        assert_eq!(base, row_hash(GENESIS_PREV, "t", "u", "a", 200));
        assert_ne!(base, row_hash(GENESIS_PREV, "t", "u", "a", 403)); // status
        assert_ne!(base, row_hash(GENESIS_PREV, "t", "u", "b", 200)); // action
        assert_ne!(base, row_hash(GENESIS_PREV, "t", "x", "a", 200)); // user
        assert_ne!(base, row_hash(GENESIS_PREV, "s", "u", "a", 200)); // at
    }

    #[test]
    fn editing_a_rows_content_is_detected() {
        let mut rows = well_formed();
        rows[1].action = "POST /resume".to_owned(); // tamper, leave hash as-is
        let err = verify_chain(&rows).unwrap_err();
        assert_eq!(err.index, 1);
        assert!(err.reason.contains("altered"));
    }

    #[test]
    fn deleting_an_interior_row_is_detected() {
        let mut rows = well_formed();
        rows.remove(1); // row 2 now follows row 0, but still carries row 1's prev
        let err = verify_chain(&rows).unwrap_err();
        assert_eq!(err.index, 1);
        assert!(err.reason.contains("prev mismatch"));
    }

    #[test]
    fn reordering_rows_is_detected() {
        let mut rows = well_formed();
        rows.swap(0, 1);
        assert!(verify_chain(&rows).is_err());
    }
}
