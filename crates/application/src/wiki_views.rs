//! Wiki depth views (CXA-F364 slice 1): table of contents, backlinks and
//! in-wiki search as PURE functions over the pages a project's doc store
//! already holds — zero schema change, zero IO (the hexagonal rule; the
//! endpoints in `presentation/server/docs.rs` only fetch the pages/tickets and
//! forward them here).
//!
//! Three views ship in this slice:
//! * [`toc`] — the auto table of contents for long pages. It is the reference
//!   implementation of TWO contracts the wiki JS (`mdRender`'s heading anchors
//!   and `buildToc`) mirrors token for token: the [`TOC_MIN_HEADINGS`]
//!   threshold ("short pages render none") and [`heading_anchor`]'s slug rule
//!   (a TOC link that does not match its heading's id scrolls nowhere). The
//!   e2e docs spec gates the mirrored behaviour in the browser.
//! * [`page_backlinks`] / [`ticket_backlinks`] — "referenced by": sibling page
//!   bodies and ticket descriptions that mention this page (by id, `[[id]]`
//!   token or title, case-insensitively — the wiki editor has no link syntax
//!   today, so prose mentions ARE the links).
//! * [`wiki_search`] — the sidebar filter's backend: title+body matching with
//!   a centered snippet the tree highlights.

use serde::{Deserialize, Serialize};

use crate::state::DocPage;
use coxagent_domain::Ticket;

/// Pages with fewer headings than this render NO table of contents (the AC's
/// "3+ headings" bound). Mirrored by `TOC_MIN_HEADINGS` in the wiki JS.
pub const TOC_MIN_HEADINGS: usize = 3;

/// One table-of-contents row: a markdown heading, its level (1–4, what
/// `mdRender` emits) and the anchor id the rendered heading carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TocEntry {
    pub level: usize,
    pub text: String,
    pub anchor: String,
}

/// The anchor id a rendered heading with `text` must carry so the TOC links
/// to it. Slug rule: lowercase, every run of non-alphanumeric characters
/// becomes one `-`, trimmed at the edges, prefixed `h-` so it can never
/// collide with another element id. The Nth duplicate heading appends
/// `-N` (first occurrence keeps the bare slug). MUST stay in sync with the
/// JS mirror (`headingAnchor` in the wiki JS) — the e2e TOC test fails if
/// the two drift.
#[must_use]
pub fn heading_anchor(text: &str, nth_duplicate: usize) -> String {
    let mut slug = String::from("h-");
    let mut prev_dash = false;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && slug.len() > 2 {
            slug.push('-');
            prev_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if nth_duplicate > 1 {
        slug.push('-');
        slug.push_str(&nth_duplicate.to_string());
    }
    slug
}

/// The table of contents for a markdown body: every `#{1,4}` heading outside
/// fenced code blocks, in document order. Fences are skipped first — a
/// `# comment` inside a ``` block is code, not a section.
#[must_use]
pub fn toc(body: &str) -> Vec<TocEntry> {
    let mut entries = Vec::new();
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut in_fence = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let trimmed = line.trim_start();
        if !trimmed.starts_with('#') {
            continue;
        }
        let level = trimmed.chars().take_while(|c| *c == '#').count();
        if level > 4 {
            continue;
        }
        let Some(text) = trimmed.get(level..).and_then(|r| r.strip_prefix(' ')) else {
            continue;
        };
        let text = strip_inline_markers(text.trim());
        if text.is_empty() {
            continue;
        }
        // Count per SLUG (not per raw text): "Setup!" and "Setup" collapse to
        // the same anchor, so both sides must share one counter — exactly what
        // the JS mirror does with its per-render `seen` map.
        let base = heading_anchor(&text, 1);
        let count = seen.entry(base).or_insert(0);
        *count += 1;
        entries.push(TocEntry {
            level,
            anchor: heading_anchor(&text, *count),
            text,
        });
    }
    entries
}

/// Strip the inline markdown markers a heading may carry (`**b**`, `*i*`,
/// `` `c` ``) so TOC labels read like the rendered text.
fn strip_inline_markers(text: &str) -> String {
    text.replace(['*', '`'], "").trim().to_owned()
}

/// One "referenced by" row: a wiki page or a ticket whose text mentions the
/// target page. `sub` carries the row's location (folder for pages; empty for
/// tickets — the board owns ticket context).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Backlink {
    pub kind: BacklinkKind,
    pub id: String,
    pub title: String,
    pub sub: String,
}

/// Where a backlink comes from — the two sources the AC names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BacklinkKind {
    /// Another wiki page's body.
    Page,
    /// A ticket's description.
    Ticket,
}

/// Whether `text` (a page body or a ticket description) references the page:
/// a case-insensitive mention of the page's id (covering the `[[id]]` wiki
/// token) or of its title. The editor ships no link syntax, so a prose
/// mention IS the link; if SA introduces one, this matcher moves with it.
#[must_use]
pub fn references_page(text: &str, page_id: &str, page_title: &str) -> bool {
    let hay = text.to_lowercase();
    (!page_id.trim().is_empty() && hay.contains(&page_id.to_lowercase()))
        || (!page_title.trim().is_empty() && hay.contains(&page_title.to_lowercase()))
}

/// Wiki pages whose body references the given page, document-store order.
/// The page never backlinks itself, and an untitled source page is skipped —
/// a backlink row with no title is a link a human cannot act on (the same
/// hygiene as global_search's page rows).
#[must_use]
pub fn page_backlinks(pages: &[DocPage], page_id: &str, page_title: &str) -> Vec<Backlink> {
    pages
        .iter()
        .filter(|p| p.id != page_id && !p.title.trim().is_empty())
        .filter(|p| references_page(&p.body, page_id, page_title))
        .map(|p| Backlink {
            kind: BacklinkKind::Page,
            id: p.id.clone(),
            title: p.title.clone(),
            sub: p.folder.clone(),
        })
        .collect()
}

/// Tickets whose description references the given page, backlog order.
#[must_use]
pub fn ticket_backlinks(tickets: &[Ticket], page_id: &str, page_title: &str) -> Vec<Backlink> {
    tickets
        .iter()
        .filter(|t| references_page(t.description(), page_id, page_title))
        .map(|t| Backlink {
            kind: BacklinkKind::Ticket,
            id: t.id().to_string(),
            title: t.title().to_owned(),
            sub: String::new(),
        })
        .collect()
}

/// Queries shorter than this never match — the input is the sidebar filter,
/// and an empty box means "no filter" (the full tree), not "no results".
pub const WIKI_SEARCH_MIN_LEN: usize = 1;
/// Input validation: queries are truncated server-side to this many
/// characters (parity with global_search's `MAX_QUERY_LEN`) — plain
/// case-insensitive substring matching must stay O(corpus) whatever the
/// caller sends. No regex, no injection surface.
pub const WIKI_MAX_QUERY_LEN: usize = 200;
/// Bound on one search response so a 400-page wiki cannot flood the rail.
pub const WIKI_MAX_HITS: usize = 100;
/// Context characters kept around a body match in a snippet.
const SNIPPET_WINDOW: usize = 92;

/// One filtered-tree row: the page plus where the query hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WikiHit {
    pub id: String,
    pub title: String,
    pub folder: String,
    pub snippet: String,
    /// The query matched the title (ranked above body-only matches; the rail
    /// highlights the title either way).
    pub in_title: bool,
}

/// Search the wiki's titles and bodies for `query` (case-insensitive
/// substring — no regex, no injection surface). Title matches rank above
/// body-only matches, then alphabetically for determinism; capped at
/// [`WIKI_MAX_HITS`]. A blank/short query matches nothing: for the sidebar
/// that state MEANS "unfiltered", and the rail redraws the full tree.
#[must_use]
pub fn wiki_search(pages: &[DocPage], query: &str) -> Vec<WikiHit> {
    let needle = query
        .trim()
        .chars()
        .take(WIKI_MAX_QUERY_LEN)
        .collect::<String>()
        .to_lowercase();
    if needle.chars().count() < WIKI_SEARCH_MIN_LEN {
        return Vec::new();
    }
    let mut hits: Vec<(u8, WikiHit)> = pages
        .iter()
        .filter(|p| !p.title.trim().is_empty())
        .filter_map(|p| {
            let title_low = p.title.to_lowercase();
            let body_low = p.body.to_lowercase();
            let in_title = title_low.contains(&needle);
            if !in_title && !body_low.contains(&needle) {
                return None;
            }
            let rank: u8 = u8::from(!in_title);
            Some((
                rank,
                WikiHit {
                    id: p.id.clone(),
                    title: p.title.clone(),
                    folder: p.folder.clone(),
                    snippet: snippet(&p.body, &needle),
                    in_title,
                },
            ))
        })
        .collect();
    hits.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.title.to_lowercase().cmp(&b.1.title.to_lowercase()))
    });
    hits.truncate(WIKI_MAX_HITS);
    hits.into_iter().map(|(_, h)| h).collect()
}

/// A body window centered on the first match, `…`-elided at the edges; the
/// head of the body when the match was in the title alone. Cut on CHARS, not
/// bytes — the team writes Vietnamese (see global_search's `snippet`).
fn snippet(body: &str, needle: &str) -> String {
    let flat = body.trim();
    if flat.is_empty() {
        return String::new();
    }
    let lower = flat.to_lowercase();
    let Some(byte_pos) = lower.find(needle) else {
        return clamp_chars(flat);
    };
    let match_char = lower[..byte_pos].chars().count();
    let needle_chars = needle.chars().count();
    let lead = match_char.saturating_sub(SNIPPET_WINDOW.saturating_sub(needle_chars) / 2);
    let window: String = flat.chars().skip(lead).take(SNIPPET_WINDOW).collect();
    let prefix = if lead > 0 { "…" } else { "" };
    let suffix = if lead + window.chars().count() < flat.chars().count() {
        "…"
    } else {
        ""
    };
    format!("{prefix}{window}{suffix}")
}

/// First 90 characters plus an ellipsis when truncated — a one-line fallback.
fn clamp_chars(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 90 {
        return flat;
    }
    format!("{}…", flat.chars().take(90).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use coxagent_domain::{Complexity, Priority, TicketId, TicketType};

    fn page(id: &str, title: &str, body: &str) -> DocPage {
        DocPage {
            id: id.to_owned(),
            folder: "Engineering".to_owned(),
            category: "technical".to_owned(),
            title: title.to_owned(),
            body: body.to_owned(),
            updated_at: "2026-09-01T10:00:00Z".to_owned(),
            updated_by: "DOCS".to_owned(),
        }
    }

    fn ticket(id: &str, title: &str, description: &str) -> Ticket {
        Ticket::new(
            TicketId::new(id).expect("valid ticket id"),
            TicketType::Feature,
            title,
            description,
            Priority::Medium,
            Complexity::Small,
            false,
        )
        .expect("valid ticket")
    }

    // --- toc ---------------------------------------------------------------

    #[test]
    fn short_pages_render_no_toc() {
        let body = "# One\n\njust prose\n";
        assert!(toc(body).len() < TOC_MIN_HEADINGS);
        assert_eq!(toc("no headings at all"), Vec::<TocEntry>::new());
    }

    #[test]
    fn pages_with_three_headings_produce_ordered_anchored_entries() {
        let body = "# Deploy health gate\n\nintro\n\n## Probe window\n\n## Recovery\n\n### Notes\n";
        let t = toc(body);
        assert_eq!(t.len(), 4);
        assert_eq!(t[0].level, 1);
        assert_eq!(t[0].anchor, "h-deploy-health-gate");
        assert_eq!(t[1].text, "Probe window");
        assert_eq!(t[3].level, 3);
    }

    #[test]
    fn headings_inside_code_fences_are_not_sections() {
        let body = "# Real\n\n```bash\n# not a heading\n```\n\n## Also real\n\n## Third\n";
        let t = toc(body);
        assert_eq!(
            t.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            vec!["Real", "Also real", "Third"],
            "a `# comment` inside a fence is code, not a section"
        );
    }

    #[test]
    fn duplicate_headings_get_distinct_anchors() {
        let body = "## Setup\n\n## Setup\n\n## Setup\n\n## Done\n";
        let t = toc(body);
        assert_eq!(t[0].anchor, "h-setup");
        assert_eq!(t[1].anchor, "h-setup-2");
        assert_eq!(t[2].anchor, "h-setup-3");
        assert_eq!(t[3].anchor, "h-done");
    }

    #[test]
    fn anchors_slug_punctuation_and_inline_markers() {
        assert_eq!(
            heading_anchor("CXA-F364: Wiki depth!", 1),
            "h-cxa-f364-wiki-depth"
        );
        let t = toc("## **Bold** `code` heading\n");
        assert_eq!(t[0].text, "Bold code heading");
        assert_eq!(t[0].anchor, "h-bold-code-heading");
    }

    // --- backlinks ----------------------------------------------------------

    #[test]
    fn a_sibling_body_referencing_by_title_is_a_backlink() {
        let target = page("doc-health", "Deploy health gate", "The probe answers 200.");
        let sibling = page(
            "doc-arch",
            "Deploy architecture",
            "Deploys refuse to finish until the Deploy health gate probe passes.",
        );
        let links = page_backlinks(&[target.clone(), sibling], &target.id, &target.title);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].kind, BacklinkKind::Page);
        assert_eq!(links[0].id, "doc-arch");
        assert_eq!(links[0].sub, "Engineering");
    }

    #[test]
    fn references_match_by_id_wiki_token_and_case_insensitively() {
        assert!(references_page(
            "see [[deploy-health-gate]] first",
            "deploy-health-gate",
            "Deploy health gate"
        ));
        assert!(references_page(
            "DEPLOY HEALTH GATE is the gate",
            "doc-health",
            "Deploy health gate"
        ));
        assert!(references_page(
            "the probe, doc-health, answers",
            "doc-health",
            "Deploy health gate"
        ));
        assert!(
            !references_page("nothing relevant here", "doc-health", "Deploy health gate"),
            "an unrelated body is not a backlink"
        );
        assert!(
            !references_page("Deploy health gate", "", ""),
            "a page with no identity cannot be referenced"
        );
    }

    #[test]
    fn a_page_never_backlinks_itself() {
        let self_ref = page(
            "doc-health",
            "Deploy health gate",
            "The deploy health gate guards deploys.",
        );
        assert!(page_backlinks(
            std::slice::from_ref(&self_ref),
            &self_ref.id,
            &self_ref.title
        )
        .is_empty());
    }

    #[test]
    fn untitled_source_pages_are_not_backlink_rows() {
        let target = page("doc-health", "Deploy health gate", "body");
        let untitled = page(
            "doc-blank",
            "   ",
            "mentions the Deploy health gate by title",
        );
        let links = page_backlinks(&[untitled], &target.id, &target.title);
        assert!(
            links.is_empty(),
            "a backlink with no title is a link a human cannot act on"
        );
    }

    #[test]
    fn ticket_descriptions_referencing_the_page_are_backlinks() {
        let target = page("doc-health", "Deploy health gate", "body");
        let hits = ticket(
            "CXC-F364-1",
            "Harden the deploy probe",
            "The probe contract lives in the Deploy health gate wiki page.",
        );
        let misses = ticket("CXC-F364-2", "Unrelated", "nothing to see");
        let links = ticket_backlinks(&[hits, misses], &target.id, &target.title);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].kind, BacklinkKind::Ticket);
        assert_eq!(links[0].id, "CXC-F364-1");
        assert!(links[0].sub.is_empty());
    }

    // --- wiki_search ---------------------------------------------------------

    #[test]
    fn title_matches_rank_above_body_only_matches() {
        let pages = [
            page(
                "doc-1",
                "Unrelated title",
                "somewhere in here it says rollout once",
            ),
            page("doc-2", "Rollout runbook", "step by step"),
        ];
        let hits = wiki_search(&pages, "rollout");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, "doc-2", "the title match ranks first");
        assert!(hits[0].in_title);
        assert!(!hits[1].in_title);
        assert!(hits[1].snippet.contains("rollout"));
    }

    #[test]
    fn blank_queries_match_nothing_the_rail_shows_the_full_tree() {
        let pages = [page("doc-1", "Rollout runbook", "rollout")];
        assert!(wiki_search(&pages, "").is_empty());
        assert!(wiki_search(&pages, "   ").is_empty());
    }

    #[test]
    fn zero_matches_return_an_empty_list() {
        let pages = [page("doc-1", "Rollout runbook", "rollout")];
        assert!(wiki_search(&pages, "zzqx").is_empty());
    }

    #[test]
    fn overlong_queries_are_truncated_not_errors() {
        let pages = [page("doc-1", "Rollout runbook", "rollout")];
        // 700 characters: truncated to WIKI_MAX_QUERY_LEN, the needle simply no
        // longer matches a row — the contract is "never an error" (the same
        // decision global_search documents for its corpus).
        let q = "rollout".repeat(100);
        assert!(wiki_search(&pages, &q).is_empty(), "bounded, not a crash");
    }

    #[test]
    fn snippets_center_on_the_match_and_stay_char_safe_on_multibyte_bodies() {
        let filler = "xưạế".repeat(60);
        let pages = [page(
            "doc-vi",
            "Hướng dẫn triển khai",
            &format!("{filler} cổng trả lỗi giữa.retry {filler}"),
        )];
        let hits = wiki_search(&pages, "giữa");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.starts_with('…'), "elided on the left");
        assert!(
            hits[0].snippet.contains("giữa"),
            "the multibyte match stays inside its window, got {:?}",
            hits[0].snippet
        );
    }

    #[test]
    fn untitled_pages_are_not_hits() {
        let pages = [
            page("doc-blank", "   ", "rollout body without a title"),
            page("doc-ok", "Rollout runbook", "rollout"),
        ];
        let hits = wiki_search(&pages, "rollout");
        assert_eq!(
            hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(),
            ["doc-ok"]
        );
    }

    #[test]
    fn the_hit_cap_bounds_the_response() {
        let pages: Vec<DocPage> = (0..(WIKI_MAX_HITS + 10))
            .map(|i| page(&format!("doc-{i}"), &format!("Rollout page {i}"), "rollout"))
            .collect();
        assert_eq!(wiki_search(&pages, "rollout").len(), WIKI_MAX_HITS);
    }

    #[test]
    fn hits_carry_the_folder_the_rail_groups_by() {
        let pages = [page("doc-1", "Rollout runbook", "rollout")];
        assert_eq!(wiki_search(&pages, "rollout")[0].folder, "Engineering");
    }
}
