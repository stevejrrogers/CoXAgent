//! Freeze the above-the-fold panel inventory (CXA-B281.a) — pure assembly of
//! [`PanelInventoryEntry`]s over the rows a [`FoldFreezeSourcePort`] returns.
//!
//! No IO here: port in, golden document out. `freeze_inventory` is the only
//! writer of the snapshot format; both the checked-in golden file and any
//! future drift check share it, so they can never disagree.

use crate::ports::outbound::fold_freeze_source::{DeclaredPanel, FoldFreezeSourcePort};
use coxagent_domain::{fnv1a_64_hex, PanelInventoryEntry};

/// The route every above-the-fold Overview panel renders on.
pub const OVERVIEW_ROUTE: &str = "#/overview";

/// The freeze result: ordered entries plus the ready-to-commit document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenInventory {
    /// Entries in ascending `order_index` (the adapter's render order).
    pub entries: Vec<PanelInventoryEntry>,
}

impl FrozenInventory {
    /// Serialize to the committed form: header comment lines + one canonical
    /// JSON record per line. Byte-stable given equal input rows.
    #[must_use]
    pub fn render(&self, header_comment: &str) -> String {
        let mut doc = String::new();
        for line in header_comment.lines() {
            doc.push_str("// ");
            doc.push_str(line.trim_end());
            doc.push('\n');
        }
        for e in &self.entries {
            doc.push_str(&e.to_canonical_json());
            doc.push('\n');
        }
        doc
    }
}

/// Build the frozen inventory from declared panel rows (port output).
///
/// The port contract is "rows in top-of-view to bottom render order"; that
/// order becomes the record's `order_index`, so ordering is attributed to the
/// declarative manifest rather than invented here. Visibility tokens are
/// hashed in their declared order via the domain's deterministic FNV-1a.
///
/// # Errors
/// When the port cannot produce rows, returns none (refusing to freeze an
/// empty inventory), or when a row fails domain validation — the error names
/// the offending slug. Never panics: out-of-range positions fall through as
/// `TryFromIntError` text instead of an `expect`.
pub fn freeze_inventory(source: &dyn FoldFreezeSourcePort) -> Result<FrozenInventory, String> {
    let panels = source.declared_panels()?;
    if panels.is_empty() {
        return Err("declared panel list is empty — refusing to freeze an empty inventory".into());
    }
    let entries: Vec<PanelInventoryEntry> = panels
        .iter()
        .enumerate()
        .map(|(i, p)| entry_of(u32::try_from(i).map_err(|e| e.to_string())?, p))
        .collect::<Result<Vec<_>, String>>()?;
    Ok(FrozenInventory { entries })
}

/// Domain entry for one declared row at its render position.
fn entry_of(order_index: u32, p: &DeclaredPanel) -> Result<PanelInventoryEntry, String> {
    PanelInventoryEntry::try_new(
        &p.slug,
        OVERVIEW_ROUTE,
        order_index,
        &p.component,
        &fnv1a_64_hex(&p.visibility_rule_tokens),
    )
    .map_err(|e| format!("panel `{}` rejected by the domain: {e}", p.slug))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(slug: &str, component: &str) -> DeclaredPanel {
        DeclaredPanel {
            slug: slug.into(),
            dom_id: format!("ov-{slug}"),
            component: component.into(),
            visibility_rule_tokens: vec!["empty".into(), "ready".into()],
        }
    }

    struct Fixed(Vec<DeclaredPanel>);
    impl FoldFreezeSourcePort for Fixed {
        fn declared_panels(&self) -> Result<Vec<DeclaredPanel>, String> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn freezes_rows_in_render_order_with_deterministic_hashes() {
        let src = Fixed(vec![
            row("drain", "shell.js drainBanner()"),
            row("alerts", "core.js alertsHtml()"),
        ]);
        let inv = freeze_inventory(&src).expect("freezes");
        assert_eq!(inv.entries.len(), 2);
        assert_eq!(inv.entries[0].panel_id, "drain");
        assert_eq!(inv.entries[0].order_index, 0);
        assert_eq!(inv.entries[1].order_index, 1);
        assert_eq!(inv.entries[1].route, OVERVIEW_ROUTE);
        let h = fnv1a_64_hex(&["empty", "ready"]);
        assert_eq!(inv.entries[0].visibility_rules_hash, h);
        assert_eq!(inv.entries[1].visibility_rules_hash, h);
    }

    #[test]
    fn rendering_is_byte_stable_and_round_trips_per_line() {
        let mk = || {
            freeze_inventory(&Fixed(vec![row("kpis", "kpis.js overviewKpis()")])).expect("freezes")
        };
        let a = mk().render("hdr\nline2");
        let b = mk().render("hdr\nline2");
        assert_eq!(a, b, "same rows must render byte-identical docs");
        assert!(a.starts_with("// hdr\n// line2\n"));
        let data_line = a.lines().last().unwrap_or_default();
        let parsed =
            PanelInventoryEntry::parse_canonical(data_line).expect("snapshot line re-parses");
        assert_eq!(parsed.panel_id, "kpis");
    }

    #[test]
    fn refuses_to_freeze_an_empty_declaration() {
        let err = freeze_inventory(&Fixed(vec![])).expect_err("empty must fail");
        assert!(
            err.contains("empty"),
            "error should name the problem: {err}"
        );
    }
}
