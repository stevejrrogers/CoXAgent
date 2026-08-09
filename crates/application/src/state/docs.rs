// Part of the `state` module split by bounded context — see state/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Wiki types: pages, the design system, and where a page belongs.

use serde::{Deserialize, Serialize};

/// The project-level design system authored once by PD. Injected into DEV
/// prompts for UI tickets so implementation is visually consistent — the
/// design analogue of architecture governance (proactive, in-prompt).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignSystem {
    /// The overall design language / principles in prose.
    pub principles: String,
    /// Color tokens, e.g. `primary: cyan #0891B2`.
    pub palette: Vec<String>,
    /// Typography guidance (families, scale, weights).
    pub typography: String,
    /// Component conventions, e.g. `buttons: 8px radius, filled primary`.
    pub components: Vec<String>,
}

impl DesignSystem {
    /// Whether any field carries content (an authored system, not a blank one).
    #[must_use]
    pub fn is_populated(&self) -> bool {
        !self.principles.is_empty()
            || !self.palette.is_empty()
            || !self.typography.is_empty()
            || !self.components.is_empty()
    }
}

/// The standard, role-owned Wiki spaces — a Confluence-like structure so each
/// role's knowledge has an obvious home instead of everything landing in one
/// folder. Ordered as they should appear in the tree.
/// The standard Wiki space a ticket's documentation belongs in, by type: a
/// feature is product knowledge; a chore (merge/rebase/maintenance) or a bug fix
/// is engineering, not a feature.
#[must_use]
pub fn standard_doc_folder(ticket_type: coxagent_domain::TicketType) -> &'static str {
    use coxagent_domain::TicketType;
    match ticket_type {
        TicketType::Feature => "Product",
        TicketType::Chore | TicketType::Bug => "Engineering",
    }
}

/// Which wiki space a ticket's page belongs in. The ticket TYPE alone gets
/// this wrong: an infrastructure feature (sandboxing, a deploy gate) is a
/// Feature ticket and would file under Product, which is how a product space
/// ends up holding nothing a product person would read. The subject decides,
/// with the type as the tie-breaker.
#[must_use]
pub fn doc_space_for(
    ticket_type: coxagent_domain::TicketType,
    title: &str,
    description: &str,
) -> &'static str {
    use coxagent_domain::TicketType;
    const ENGINEERING: &[&str] = &[
        "docker",
        "compose",
        "ci ",
        "pipeline",
        "clippy",
        "lint",
        "sandbox",
        "seatbelt",
        "bwrap",
        "deploy gate",
        "health check",
        "rollback",
        "refactor",
        "migration",
        "schema",
        "runner",
        "cargo",
        "build fails",
        "compile",
    ];
    let text = format!("{title} {description}").to_lowercase();
    if ENGINEERING.iter().any(|k| text.contains(k)) {
        return "Engineering";
    }
    match ticket_type {
        TicketType::Feature => "Product",
        TicketType::Chore | TicketType::Bug => "Engineering",
    }
}

/// The colour/category bucket for a Wiki folder, keyed off its top-level space.
/// Keeps DOCS-written pages consistent with the UI's folder colouring.
#[must_use]
pub fn doc_category_of(folder: &str) -> &'static str {
    match folder
        .split('/')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "architecture" | "technical" | "engineering" => "technical",
        "design" | "flows" => "flows",
        "qa" | "testing" | "test" | "tests" => "qa",
        "operations" | "ops" | "release notes" | "releases" => "ops",
        "product" | "features" => "product",
        // An unrecognised space is not silently "product": mislabelling a
        // team/ops page as product colours it wrongly in the wiki and skews
        // every filter built on the category.
        _ => "general",
    }
}

/// One living documentation page. `category` is `"product"` or `"technical"`;
/// `body` is Markdown. Pages are written by the DOCS agent and editable by
/// humans, and are structured so both people and agents can read them.
/// Idle-cycle refresh bookkeeping for one Wiki page. `at` is the RFC3339 time of
/// the last refresh attempt (a cooldown, so a page is not rewritten every
/// cycle); `fails` counts consecutive rewrites the structure gate rejected — at
/// the cap the page is parked (it needs a human/redesign, not more calls).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocRefreshMark {
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub fails: u32,
}

/// One living documentation page. `category` is `"product"` or `"technical"`;
/// `body` is Markdown. Pages are written by the DOCS agent and editable by
/// humans, and are structured so both people and agents can read them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocPage {
    pub id: String,
    /// Folder path the page lives under, `/`-separated for nesting
    /// (e.g. `"Technical/Architecture"`). Empty = root.
    #[serde(default)]
    pub folder: String,
    /// Coarse colour bucket for the tag: `product`/`technical`/`flows`/`qa`/`ops`.
    pub category: String,
    pub title: String,
    /// Markdown body.
    pub body: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub updated_by: String,
}
