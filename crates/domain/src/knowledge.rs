//! Hub-wide cross-project knowledge value objects (CXA-F017).
//!
//! Pure domain types describing one discoverable unit of shared knowledge —
//! a wiki page another project wrote, or a lesson recorded at hub level — plus
//! how an operator curates it for display in generated briefs. These mirror
//! `DocPage` (which lives in application state) but live here because
//! cross-project curation is its own bounded context with zero IO; nothing in
//! this file touches disk, network or frameworks.
/// What kind of thing an entry summarises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::upper_case_acronyms)] // catalogue names users see verbatim
pub enum KnowledgeKind {
    /// A wiki page another team member wrote into their project store.
    Page,
    /// A lesson bullet recorded at hub level (`hub_lessons.md`).
    Lesson,
    /// A ticket already solved in a sibling project that shared its outcome.
    Decision,
}

impl KnowledgeKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Page => "Page",
            Self::Lesson => "Lesson",
            Self::Decision => "Decision",
        }
    }
}

/// One discoverable unit of shared knowledge surfaced by the hub dashboard.
#[derive(Debug)]
pub struct KnowledgeEntry {
    /// Stable composite id `<project_id>:<kind>:<slug>` used as KV key suffix.
    pub entry_id: String,
    /// Which sibling project owns this entry (`hub-lessons` for lessons).
    pub project_id: String,
    pub kind: KnowledgeKind,
    pub title: String,
}

/// How an operator has curated one entry for display in brief surfaces.
#[derive(Debug)]
pub struct CurationState {
    /// Hidden from generated brief blocks until explicitly un-hidden again.
    pub hidden_in_briefs: bool,
}
