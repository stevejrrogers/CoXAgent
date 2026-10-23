//! CLI entry for regenerating the frozen above-the-fold inventory
//! (CXA-B281.a).
//!
//! `cargo test -p coxagent-app --test fold_inventory_freeze_b281 -- --nocapture`
//! rebuilds and asserts the golden snapshot byte-for-byte; this binary is the
//! human regeneration path documented in the snapshot header.
//!
//! IO lives here (thin shell); every decision is in domain/application.

use std::path::Path;

/// kept out of the diffed data lines.
use coxagent_application::use_cases::freeze_inventory;
use coxagent_presentation::fold_freeze_adapter::ManifestFreezeSource;

/// Header stamped onto the golden document — provenance + regeneration
/// instructions, kept OUT of the diffed data lines.
fn header(now_utc: &str) -> String {
    format!(
        "GOLDEN SNAPSHOT — frozen above-the-fold Overview panel inventory \
(CXA-B281.a).\nGenerated: {now_utc} by \
`cargo run -p coxagent-app --bin freeze_fold_inventory`. Do not edit the \
data lines by hand; regenerate and commit the whole file.\nOne canonical \
JSON record per line, keyed by order_index; shape defined by \
coxagent_domain::PanelInventoryEntry."
    )
}

fn main() {
    let src = ManifestFreezeSource;
    let inv = freeze_inventory(&src).unwrap_or_else(|e| panic!("cannot freeze the inventory: {e}"));
    // `chrono` is not an app-crate dependency; the committed header records the
    // generation METHOD and command — the exact wall-clock second belongs to
    // git history, so we keep the stamp coarse and std-only.
    let now = match std::env::var("FREEZE_STAMP") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => "untimestamped-local-run".into(),
    };
    let doc = inv.render(&header(&now));

    let out = Path::new("crates/presentation/__inventories__/above_the_fold_panels.golden");
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", dir.display()));
    }
    std::fs::write(out, doc).unwrap_or_else(|e| panic!("cannot write {}: {e}", out.display()));
    println!("wrote {} ({} panels)", out.display(), inv.entries.len());
}
