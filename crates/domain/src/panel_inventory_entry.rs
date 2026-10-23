//! The frozen above-the-fold panel inventory record (CXA-B281.a, subtask 1/3
//! of CXA-B281).
//!
//! One [`PanelInventoryEntry`] is the canonical identity of a single rendered
//! panel: which panel (`panel_id`), where it lives (`route`), where it sits
//! (`order_index`), what paints it (`component_name`) and a deterministic
//! fingerprint of its visibility rules (`visibility_rules_hash`). This is the
//! record the golden snapshot freezes and the drift guardrail (later
//! subtasks) will diff against.
//!
//! Pure domain: no IO, no framework. Serialization is a hand-rolled CANONICAL
//! form — keys in sorted (alphabetical) order, entries keyed by ascending
//! `order_index` upstream — so equal inventories always serialize to equal
//! bytes, which is what makes the committed snapshot byte-stable.
//! [`fnv1a_64_hex`] is the deterministic hash over an ORDERED token list; it
//! is deliberately order-sensitive because visibility-rule order is contract.

/// Canonical identity of one above-the-fold panel, frozen for diffing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelInventoryEntry {
    /// Stable panel id (the registry slug: `drain`, `alerts`, …).
    pub panel_id: String,
    /// The route the panel is rendered on (`#/overview` today).
    pub route: String,
    /// Zero-based paint order within the route. Sort key of every document.
    pub order_index: u32,
    /// The client module/function that owns the panel's fetch + paint.
    pub component_name: String,
    /// Deterministic fingerprint (FNV-1a 64, hex) of the panel's visibility
    /// rules in contract order.
    pub visibility_rules_hash: String,
}

impl PanelInventoryEntry {
    /// Build an entry, refusing blanks up front — a blank id would silently
    /// collide every panel into one row in a later diff.
    ///
    /// # Errors
    /// When any identifier field is empty or the hash is not lowercase hex.
    pub fn try_new(
        panel_id: &str,
        route: &str,
        order_index: u32,
        component_name: &str,
        visibility_rules_hash: &str,
    ) -> Result<Self, String> {
        for (label, v) in [
            ("panel_id", panel_id),
            ("route", route),
            ("component_name", component_name),
        ] {
            if v.trim().is_empty() {
                return Err(format!("`{label}` must not be blank"));
            }
        }
        if !is_lower_hex(visibility_rules_hash) || visibility_rules_hash.is_empty() {
            return Err("`visibility_rules_hash` must be non-empty lowercase hex".into());
        }
        Ok(Self {
            panel_id: panel_id.into(),
            route: route.into(),
            order_index,
            component_name: component_name.into(),
            visibility_rules_hash: visibility_rules_hash.into(),
        })
    }

    /// Serialize to the canonical single-line JSON object: keys ALWAYS in
    /// sorted (alphabetical) order, minimal escaping, no whitespace. Two
    /// entries constructed in different orders but carrying equal data
    /// produce equal bytes — that is the property the golden snapshot relies on.
    #[must_use]
    pub fn to_canonical_json(&self) -> String {
        // Sorted-key pairs, emitted literally so ordering is visible here.
        let mut out = String::from("{\"component_name\":");
        out.push_str(&quote(&self.component_name));
        out.push_str(",\"order_index\":");
        out.push_str(&self.order_index.to_string());
        out.push_str(",\"panel_id\":");
        out.push_str(&quote(&self.panel_id));
        out.push_str(",\"route\":");
        out.push_str(&quote(&self.route));
        out.push_str(",\"visibility_rules_hash\":");
        out.push_str(&quote(&self.visibility_rules_hash));
        out.push('}');
        out
    }

    /// Parse back a value produced by [`Self::to_canonical_json`]. Strictly
    /// accepts the canonical form (that strictness is what lets the test
    /// suite pin key ordering); anything else is rejected loudly.
    ///
    /// # Errors
    /// When the input is not the canonical single-line form.
    pub fn parse_canonical(input: &str) -> Result<Self, String> {
        let s = input.trim();
        let inner = s
            .strip_prefix('{')
            .and_then(|x| x.strip_suffix('}'))
            .ok_or_else(|| "not a canonical `{…}` object".to_string())?;
        let parts = split_top_level(inner)?;
        let expected = [
            "component_name",
            "order_index",
            "panel_id",
            "route",
            "visibility_rules_hash",
        ];
        if parts.len() != expected.len() {
            return Err(format!(
                "expected {} keys in sorted order, found {}",
                expected.len(),
                parts.len()
            ));
        }
        let mut fields: Vec<(String, String)> = Vec::with_capacity(parts.len());
        for (i, part) in parts.iter().enumerate() {
            let (k, v) = split_kv(part, i)?;
            if k != expected[i] {
                return Err(format!("key {i}: expected `{}`, found `{k}`", expected[i]));
            }
            fields.push((k, v));
        }
        let get = |k: &str| {
            fields
                .iter()
                .find(|(kk, _)| kk == k)
                .map(|(_, v)| v.clone())
                .ok_or_else(|| format!("missing `{k}`"))
        };
        let order_index: u32 = get("order_index")?
            .parse()
            .map_err(|e| format!("bad `order_index`: {e}"))?;
        Self::try_new(
            &get("panel_id")?,
            &get("route")?,
            order_index,
            &get("component_name")?,
            &get("visibility_rules_hash")?,
        )
    }
}

/// FNV-1a 64-bit hash of an ORDERED token list, lowercase-hex encoded.
///
/// Deterministic across platforms and runs (no std-hash randomization) and
/// order-sensitive, so rule permutations are visible instead of folding into
/// the same fingerprint.
#[must_use]
pub fn fnv1a_64_hex<S: AsRef<str>>(tokens: &[S]) -> String {
    let tokens: Vec<&str> = tokens.iter().map(AsRef::as_ref).collect();
    hash_str_tokens(&tokens)
}

/// The FNV core over plain `&str` tokens.
fn hash_str_tokens(tokens: &[&str]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for t in tokens {
        for b in t.as_bytes() {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        // Token separator keeps ["ab","c"] distinct from ["a","bc"].
        hash ^= 0x1f;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn is_lower_hex(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn quote(v: &str) -> String {
    let mut out = String::with_capacity(v.len() + 2);
    out.push('"');
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn unquote(v: &str) -> Result<String, String> {
    let body = v
        .strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .ok_or("value not quoted")?;
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            other => return Err(format!("unsupported escape \\{other:?}")),
        }
    }
    Ok(out)
}

/// Split a canonical object body on commas that sit OUTSIDE quoted strings.
fn split_top_level(inner: &str) -> Result<Vec<&str>, String> {
    let mut parts = Vec::new();
    let mut depth_quotes = false;
    let mut start = 0usize;
    for (i, c) in inner.char_indices() {
        match c {
            '"' => depth_quotes = !depth_quotes,
            ',' if !depth_quotes => {
                parts.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if depth_quotes {
        return Err("unterminated string".into());
    }
    parts.push(&inner[start..]);
    Ok(parts)
}

fn split_kv(part: &str, idx: usize) -> Result<(String, String), String> {
    let (raw_k, raw_v) = part
        .split_once(':')
        .ok_or_else(|| format!("part {idx} lacks `:`"))?;
    let k = unquote(raw_k.trim())?;
    // `order_index` is the only non-string value in the canonical form.
    let v = if k == "order_index" {
        raw_v
            .trim()
            .parse::<u32>()
            .map_err(|e| format!("bad `order_index`: {e}"))?
            .to_string()
    } else {
        unquote(raw_v.trim())?
    };
    Ok((k, v))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PanelInventoryEntry {
        PanelInventoryEntry::try_new(
            "kpis",
            "#/overview",
            3,
            "kpis.js overviewKpis()",
            &fnv1a_64_hex(&["empty", "ready", "zero"]),
        )
        .expect("valid sample")
    }

    #[test]
    fn round_trips_losslessly() {
        let e = sample();
        let parsed = PanelInventoryEntry::parse_canonical(&e.to_canonical_json()).expect("parses");
        assert_eq!(parsed, e);
    }

    #[test]
    fn keys_are_emitted_in_sorted_alphabetical_order() {
        let line = sample().to_canonical_json();
        let keys = [
            "component_name",
            "order_index",
            "panel_id",
            "route",
            "visibility_rules_hash",
        ];
        let mut last_pos = 0usize;
        for k in keys {
            let pos = line.find(k).unwrap_or_else(|| panic!("key `{k}` missing"));
            assert!(pos > last_pos, "`{k}` out of sorted order");
            last_pos = pos;
        }
    }

    #[test]
    fn equal_data_serializes_to_equal_bytes_regardless_of_construction_order() {
        let a = PanelInventoryEntry::try_new("alerts", "#/overview", 1, "a", "00ff").unwrap();
        let b = PanelInventoryEntry::try_new("alerts", "#/overview", 1, "a", "00ff").unwrap();
        assert_eq!(a.to_canonical_json(), b.to_canonical_json());
    }

    #[test]
    fn rejects_blank_ids_and_bad_hashes() {
        assert!(PanelInventoryEntry::try_new("", "#/overview", 0, "c", "00ff").is_err());
        assert!(PanelInventoryEntry::try_new("p", "", 0, "c", "00ff").is_err());
        assert!(PanelInventoryEntry::try_new("p", "#/overview", 0, "", "00ff").is_err());
        assert!(PanelInventoryEntry::try_new("p", "#/overview", 0, "c", "XYZ").is_err());
    }

    #[test]
    fn hash_is_deterministic_and_order_sensitive() {
        let h1 = fnv1a_64_hex(&["empty", "ready"]);
        let h2 = fnv1a_64_hex(&["empty", "ready"]);
        assert_eq!(h1, h2, "same input must hash identically");
        assert_ne!(
            h1,
            fnv1a_64_hex(&["ready", "empty"]),
            "rule order is contract"
        );
        assert_eq!(h1.len(), 16);
        assert!(h1.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn escaped_values_survive_a_round_trip() {
        let e = PanelInventoryEntry::try_new(
            "drain",
            "#/overview",
            0,
            "core.js \"drainBanner\"",
            "abcd0123",
        )
        .unwrap();
        let parsed = PanelInventoryEntry::parse_canonical(&e.to_canonical_json()).unwrap();
        assert_eq!(parsed, e);
    }
}
