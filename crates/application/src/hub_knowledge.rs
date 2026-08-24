//! Cross-project knowledge aggregation (CXA-F017).
//!
//! Pure functions over per-sibling snapshots plus hub-lessons text: no IO here,
//! adapters hand us slices. Shareable knowledge reuses the same sources agent
//! briefs read — wiki pages from state.docs (like prompts::wiki_block), closed
//! tickets from state.tickets (like prompts::prior_fix_block), lessons from the
//! same hub_lessons.md bullets prompts::hub_lessons_block shows.
use coxagent_domain::{CurationState, KnowledgeEntry, KnowledgeKind};

/// Project id used for hub-level lessons entries.
pub const LESSONS_PROJECT_ID: &str = "hub-lessons";

fn is_solved(status: coxagent_domain::Status) -> bool {
    matches!(
        status,
        coxagent_domain::Status::Fixed
            | coxagent_domain::Status::Done
            | coxagent_domain::Status::Documented
            | coxagent_domain::Status::Verified
    )
}

fn slug(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_dash = false;
    for ch in raw.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_dash = false;
        } else if !out.is_empty() && !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "untitled".to_owned()
    } else {
        out.chars().take(64).collect()
    }
}

fn build_entry(project_id: &str, kind: KnowledgeKind, title: &str) -> KnowledgeEntry {
    KnowledgeEntry {
        entry_id: format!("{}:{}:{}", project_id, kind.as_str(), slug(title)),
        project_id: project_id.to_owned(),
        kind,
        title: title.trim().to_owned(),
    }
}

/// All sharable knowledge in one sibling's snapshot: wiki pages plus closed
/// tickets, folded into stable, de-duplicated entries sorted by entry id.
#[must_use]
pub fn entries_from_project(
    project_id: &str,
    docs: &[crate::state::DocPage],
    tickets: &[coxagent_domain::Ticket],
) -> Vec<KnowledgeEntry> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out: Vec<KnowledgeEntry> = Vec::new();
    for d in docs
        .iter()
        .filter(|d| !d.title.trim().is_empty() && !d.body.trim().is_empty())
    {
        if seen.insert(d.title.clone()) {
            out.push(build_entry(project_id, KnowledgeKind::Page, &d.title));
        }
    }
    for t in tickets.iter().filter(|t| is_solved(t.status())) {
        if seen.insert(format!("decision-{}", t.title())) {
            out.push(build_entry(project_id, KnowledgeKind::Decision, t.title()));
        }
    }
    out.sort_by(|a, b| a.entry_id.cmp(&b.entry_id));
    out.dedup_by(|a, b| a.entry_id == b.entry_id);
    out
}

/// Every lesson bullet in `text` (one `- item` per line) as hub-level entries.
#[must_use]
pub fn lessons_from_text(text: &str) -> Vec<KnowledgeEntry> {
    let mut out = Vec::new();
    for line in text.lines() {
        let bullet = line.trim().strip_prefix('-').unwrap_or(line).trim();
        if !bullet.is_empty() {
            out.push(build_entry(
                LESSONS_PROJECT_ID,
                KnowledgeKind::Lesson,
                bullet,
            ));
        }
    }
    out
}

/// Whether an entry should be shown given its persisted curation state.
#[must_use]
pub fn visible(curation: Option<&CurationState>) -> bool {
    curation.map_or(true, |c| !c.hidden_in_briefs)
}

#[cfg(test)]
mod tests {
    use super::{entries_from_project, lessons_from_text};
    use coxagent_domain::CurationState;

    fn page(title: &str) -> crate::state::DocPage {
        crate::state::DocPage {
            id: format!("doc-{title}"),
            folder: "Engineering".to_owned(),
            category: "technical".to_owned(),
            title: title.to_owned(),
            body: "body text".to_owned(),
            updated_at: String::new(),
            updated_by: String::new(),
        }
    }

    #[test]
    fn wiki_pages_become_page_entries_grouped_by_project() {
        let docs = vec![page("Deploy health gate"), page("Auth hardening")];
        let entries = entries_from_project("demo", &docs, &[]);
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.kind.as_str() == "Page"));
        assert!(entries.iter().all(|e| e.project_id == "demo"));
    }

    #[test]
    fn lessons_render_as_hub_level_bullets() {
        let text = "- one lesson\n\n- another lesson\n";
        let entries = lessons_from_text(text);
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.kind.as_str() == "Lesson"));
        assert!(entries
            .iter()
            .all(|e| e.project_id == super::LESSONS_PROJECT_ID));
    }

    #[test]
    fn curation_flag_gates_visibility_without_entry_data() {
        let shown = CurationState {
            hidden_in_briefs: false,
        };
        let hidden = CurationState {
            hidden_in_briefs: true,
        };
        assert!(super::visible(Some(&shown)));
        assert!(!super::visible(Some(&hidden)));
        assert!(super::visible(None));
    }

    fn fixed_bug(title: &str) -> coxagent_domain::Ticket {
        let mut t = coxagent_domain::Ticket::new(
            coxagent_domain::TicketId::new(format!("CXA-T{}", super::slug(title))).unwrap(),
            coxagent_domain::TicketType ::Bug ,
            title.to_owned(),
            "desc",
            coxagent_domain ::Priority ::Medium ,
            coxagent_domain ::Complexity ::Small ,
            false,
        )
        .unwrap();
        t.transition_to(coxagent_domain ::Role ::DevBug ,coxagent_domain ::Status ::InProgress )
            .unwrap();
        t.transition_to(coxagent_domain ::Role ::DevBug ,coxagent_domain ::Status ::Fixed )
            .unwrap();
        t
    }

    #[test]
    fn solved_ticket_becomes_a_decision_entry_and_open_tickets_stay_out() {
        let tickets = vec![fixed_bug("Port clash on deploy")];
        let entries = entries_from_project("demo", &[], &tickets);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind.as_str(), "Decision");
        assert_eq!(entries[0].project_id, "demo");
    }

    #[test]
    fn open_bug_is_not_shared_across_projects() {
        let open = coxagent_domain ::Ticket::new(
            coxagent_domain::TicketId::new("CXA-OPEN1".to_owned()).unwrap(),
            coxagent_domain ::TicketType ::Bug ,
            "Investigate alert",
            "still triaging",
            coxagent_domain ::Priority ::Medium ,
            coxagent_domain ::Complexity ::Small ,
            false,
        )
        .unwrap();
        let entries = entries_from_project("demo", &[], &[open]);
        assert!(entries.is_empty());
    }

}
