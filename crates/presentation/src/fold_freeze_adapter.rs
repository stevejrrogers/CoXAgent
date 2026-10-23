//! Presentation-side freeze adapter (CXA-B281.a): implements
//! [`FoldFreezeSourcePort`] over the typed above-the-fold manifest, so the
//! application use case can build the golden inventory WITHOUT touching DOM,
//! JS or any view code.
//!
//! The component names here echo the paint sites documented in
//! `crates/presentation/src/web/js/overview_vm.js`; keep both in sync when a
//! panel's renderer moves.

use crate::overview_panels::{OverviewPanel, OverviewPanelId, TerminalState, OVERVIEW_PANELS};
use coxagent_application::ports::outbound::fold_freeze_source::{
    DeclaredPanel, FoldFreezeSourcePort,
};

/// One declared row per above-the-fold panel, in manifest (DOM) order.
#[derive(Debug, Clone, Copy)]
pub struct ManifestFreezeSource;

impl FoldFreezeSourcePort for ManifestFreezeSource {
    fn declared_panels(&self) -> Result<Vec<DeclaredPanel>, String> {
        if OVERVIEW_PANELS.is_empty() {
            return Err("the Overview panel manifest is empty".into());
        }
        Ok(OVERVIEW_PANELS
            .iter()
            .map(|p| DeclaredPanel {
                slug: p.id.to_string(),
                dom_id: p.dom_id.to_owned(),
                component: dependency_component(p),
                visibility_rule_tokens: rule_token_names(p.id, p.states),
            })
            .collect())
    }
}

/// The paint-site attribution frozen as `component_name`: the owning JS
/// module/function (`dependency.owner`) plus the DOM mount — prose, never a
/// debug-formatted struct (the record must stay 5 canonical keys).
fn dependency_component(p: &OverviewPanel) -> String {
    format!("{} @ {}", p.dependency.owner, p.dom_id)
}

/// Stable name for one [`TerminalState`] variant (no derive-Display dep).
fn state_name(s: TerminalState) -> &'static str {
    match s {
        TerminalState::Loading => "loading",
        TerminalState::Empty => "empty",
        TerminalState::Error => "error",
        TerminalState::Ready => "ready",
        TerminalState::Attention => "attention",
        TerminalState::Zero => "zero",
    }
}

/// The panel's visibility-rule tokens: every declared state name except
/// `loading` (transient, not a visibility rule), in declaration order.
fn rule_token_names(_id: OverviewPanelId, states: &'static [TerminalState]) -> Vec<String> {
    states
        .iter()
        .copied()
        .filter(|s| !matches!(s, TerminalState::Loading))
        .map(state_name)
        .map(str::to_owned)
        .collect()
}
