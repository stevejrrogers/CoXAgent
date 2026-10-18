//! Global search (CXA-F275): one query across a project's tickets, wiki pages
//! and chat threads — the prior art an operator at a human gate needs.
//!
//! PURE application logic, zero IO: every decision is a function over the
//! `ProjectState`, the query and the viewer passed in (the hexagonal rule —
//! the endpoint in `presentation/server/search.rs` only forwards the resolved
//! principal and flattens the result). Ranking follows the SA weights: field
//! weight title/id/body = 3/2/1, match quality prefix/substring = 2/1, with a
//! recency tiebreak; results are capped per kind and in total so a 400-ticket
//! backlog can never render unbounded.

use serde::{Deserialize, Serialize};

use crate::auth::{AuthRole, AuthUser};
use crate::state::ProjectState;

/// Queries shorter than this many characters match nothing (AC4): a one-letter
/// needle over three corpora is all noise, so the use case refuses it and the
/// palette shows its explicit empty state instead.
pub const MIN_QUERY_LEN: usize = 2;
/// Input validation: queries are truncated server-side to this many characters
/// (plain case-insensitive substring matching — no regex, no injection surface).
pub const MAX_QUERY_LEN: usize = 200;
/// Per-kind cap (AC5): at most this many hits of one kind are returned, so a
/// 400-ticket backlog cannot flood the palette. The group's `has_more` flag is
/// the show-more affordance's signal.
pub const MAX_PER_KIND: usize = 8;
/// Total cap across all kinds, bounding every response to O(1) size.
pub const MAX_TOTAL: usize = 30;

/// Field weights (SA contract): what the query matched beats where it matched.
const W_LABEL: u32 = 3;
const W_ID: u32 = 2;
const W_BODY: u32 = 1;
/// Match quality: a field that starts with the query outranks a mid-field hit.
const Q_PREFIX: u32 = 2;
const Q_SUBSTRING: u32 = 1;

/// Context characters kept around a body match in a snippet (window centered
/// on the match, `…`-elided at the edges).
const SNIPPET_WINDOW: usize = 92;
/// Body-as-label truncation for the rows whose text IS their body (messages,
/// thread comments) — the palette shows one line per hit.
const LABEL_MAX: usize = 90;

/// What kind of surface a hit belongs to. The palette groups and labels by
/// this; `link` carries the SPA surface the hit deep-links to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchKind {
    /// A work ticket (open or closed — see the closed/archived note below).
    Ticket,
    /// A wiki page (`state.docs`).
    Page,
    /// A comment on a ticket's discussion thread (`state.comments`).
    Comment,
    /// A team-chat message (`state.chat`).
    Message,
}

impl SearchKind {
    /// Stable group order: tickets, wiki, then the two chat surfaces — the
    /// order the palette renders its sections in.
    #[must_use]
    pub fn rank(self) -> u8 {
        match self {
            Self::Ticket => 0,
            Self::Page => 1,
            Self::Comment => 2,
            Self::Message => 3,
        }
    }

    /// The SPA surface this kind deep-links to (`#board` opens the Work view
    /// where the ticket dialog lives, `#docs` the Wiki, `#chat` the chat —
    /// which is a MODE there, not a hash route: clients map the kind to the
    /// surface's own entry point, `link` names the destination).
    #[must_use]
    pub fn link(self) -> &'static str {
        match self {
            Self::Ticket | Self::Comment => "#board",
            Self::Page => "#docs",
            Self::Message => "#chat",
        }
    }
}

/// One labeled, deep-linkable hit. `id`/`ref` identify the row so the client
/// can open the exact surface (ticket dialog, wiki page, anchored message):
/// ticket → id is the ticket id; page → ref is its folder; comment → ref is
/// the ticket whose thread it belongs to; message → ref is the channel id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchHit {
    pub kind: SearchKind,
    pub id: String,
    #[serde(rename = "ref")]
    pub ref_: String,
    pub label: String,
    pub snippet: String,
    pub sub: String,
    pub at: String,
    /// SPA surface the hit deep-links to (see [`SearchKind::link`]).
    pub link: &'static str,
}

/// One kind's ranked, capped slice of the results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchGroup {
    pub kind: SearchKind,
    pub hits: Vec<SearchHit>,
    /// There were more matches than [`MAX_PER_KIND`] — the show-more signal.
    pub has_more: bool,
}

/// The grouped result of one global search. Empty `groups` IS the explicit
/// empty state (AC4): the palette renders "No results for …", never a blank
/// panel — and a query shorter than [`MIN_QUERY_LEN`] returns exactly this.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GroupedSearch {
    pub groups: Vec<SearchGroup>,
}

impl GroupedSearch {
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.groups.iter().all(|g| g.hits.is_empty())
    }

    /// Every hit, grouped order — the flat wire form the endpoint serves.
    #[must_use]
    pub fn flat(&self) -> Vec<SearchHit> {
        self.groups
            .iter()
            .flat_map(|g| g.hits.iter().cloned())
            .collect()
    }
}

/// Search ONE project. Results are scoped to what `viewer` may see: project
/// membership (AC2), then per-row channel visibility for chat (AC2's thread
/// half). A viewer with no access gets the empty result — never an error.
#[must_use]
pub fn global_search(
    state: &ProjectState,
    pid: &str,
    query: &str,
    viewer: &AuthUser,
) -> GroupedSearch {
    group_rows(project_rows(state, pid, query, viewer))
}

/// Search several projects at once (the endpoint's no-pid sweep) and merge
/// into one ranked, capped response. Projects the viewer cannot see contribute
/// nothing.
#[must_use]
pub fn global_search_many<'a>(
    projects: impl IntoIterator<Item = (&'a str, &'a ProjectState)>,
    query: &str,
    viewer: &AuthUser,
) -> GroupedSearch {
    let rows = projects
        .into_iter()
        .flat_map(|(pid, state)| project_rows(state, pid, query, viewer))
        .collect();
    group_rows(rows)
}

// --- scoping -----------------------------------------------------------------

/// AC2: project access rides on the viewer's memberships (`AuthUser.projects`),
/// with the one house exception documented beside the check: Super/Admin
/// bypass membership everywhere else (`auth_mw`'s per-project gate in
/// `server/auth.rs` — the river_scope rule), so search grants them the same.
/// Role authority ALONE is never access: a lead-tier or Viewer account that is
/// not a member sees nothing, exactly like every other per-project route.
fn may_view_project(viewer: &AuthUser, pid: &str) -> bool {
    viewer.role.is_super()
        || viewer.role == AuthRole::Admin
        || viewer.projects.iter().any(|p| p == pid)
}

// --- corpus rows -------------------------------------------------------------

/// One scored candidate. Internal: `group_rows` turns rows into the capped,
/// grouped wire form.
struct Row {
    hit: SearchHit,
    score: u32,
    at: String,
}

fn project_rows(state: &ProjectState, pid: &str, query: &str, viewer: &AuthUser) -> Vec<Row> {
    let Some(q) = SearchQuery::new(query) else {
        return Vec::new();
    };
    if !may_view_project(viewer, pid) {
        return Vec::new();
    }
    let mut rows = Vec::new();
    rows.extend(ticket_rows(state, &q));
    rows.extend(page_rows(state, &q));
    rows.extend(comment_rows(state, &q));
    rows.extend(message_rows(state, viewer, &q));
    rows
}

/// AC3 — closed/archived tickets are an EXPLICIT decision here, never an error.
///
/// * CLOSED tickets (Done / Verified / Rejected / Documented — the terminal
///   statuses the real transition table drives tickets to) live in
///   `state.tickets` and search like any other row. They are the prior art
///   this box exists to surface; their status rides in `sub` so a gate
///   operator reads "done"/"verified" at a glance.
/// * ARCHIVED tickets (CXA-F274's archive store) do not exist in project state
///   today: until that read-back surface ships they are simply ABSENT from
///   results — no row is invented, no error raised. When it ships, merge its
///   rows HERE with an "archived" label in `sub`, so the closed/archived case
///   stays a decision in one place.
fn ticket_rows(state: &ProjectState, q: &SearchQuery) -> Vec<Row> {
    state
        .tickets
        .iter()
        .filter_map(|t| {
            let id = t.id().to_string();
            let score = best_score(
                &[
                    (t.title(), W_LABEL),
                    (id.as_str(), W_ID),
                    (t.description(), W_BODY),
                ],
                &q.needle,
            )?;
            let hit = SearchHit {
                kind: SearchKind::Ticket,
                id,
                ref_: String::new(),
                label: t.title().to_owned(),
                snippet: snippet(t.description(), &q.needle),
                sub: status_label(t.status()),
                at: t.created_at().unwrap_or_default().to_owned(),
                link: SearchKind::Ticket.link(),
            };
            Some(Row {
                hit,
                score,
                at: t.created_at().unwrap_or_default().to_owned(),
            })
        })
        .collect()
}

fn page_rows(state: &ProjectState, q: &SearchQuery) -> Vec<Row> {
    state
        .docs
        .iter()
        .filter_map(|d| {
            // An untitled page is not a hit a human can act on — skip it.
            if d.title.trim().is_empty() {
                return None;
            }
            let score = best_score(
                &[
                    (d.title.as_str(), W_LABEL),
                    (d.id.as_str(), W_ID),
                    (d.body.as_str(), W_BODY),
                ],
                &q.needle,
            )?;
            let sub = if d.folder.trim().is_empty() {
                d.category.clone()
            } else {
                d.folder.clone()
            };
            let hit = SearchHit {
                kind: SearchKind::Page,
                id: d.id.clone(),
                ref_: d.folder.clone(),
                label: d.title.clone(),
                snippet: snippet(&d.body, &q.needle),
                sub,
                at: d.updated_at.clone(),
                link: SearchKind::Page.link(),
            };
            Some(Row {
                hit,
                score,
                at: d.updated_at.clone(),
            })
        })
        .collect()
}

/// Ticket discussion threads (`state.comments` with a `ticket` attached — the
/// SA contract's `ref = ticket id it belongs to`). Channel-wide scrum comments
/// carry no ticket ref and are the Scrum view's own surface, so they are not
/// search rows.
fn comment_rows(state: &ProjectState, q: &SearchQuery) -> Vec<Row> {
    state
        .comments
        .iter()
        .filter_map(|c| {
            let ticket_ref = c.ticket.as_ref()?;
            let score = best_score(
                &[
                    (c.body.as_str(), W_LABEL),
                    (c.id.as_str(), W_ID),
                    (ticket_ref.as_str(), W_BODY),
                ],
                &q.needle,
            )?;
            let hit = SearchHit {
                kind: SearchKind::Comment,
                id: c.id.clone(),
                ref_: ticket_ref.clone(),
                label: clamp_label(&c.body),
                snippet: String::new(),
                sub: format!("thread · {ticket_ref}"),
                at: c.at.clone(),
                link: SearchKind::Comment.link(),
            };
            Some(Row {
                hit,
                score,
                at: c.at.clone(),
            })
        })
        .collect()
}

/// Team-chat messages. AC2's thread half: visibility is the SHIPPED predicate —
/// [`crate::state::Channel::can_view`], the exact check `chat_list_ep` gates
/// the channel's history with — so a private room's messages never leak into
/// search, for any role. A message whose channel record has vanished (data
/// drift) is skipped, not shown: fail closed on a privacy surface.
fn message_rows(state: &ProjectState, viewer: &AuthUser, q: &SearchQuery) -> Vec<Row> {
    state
        .chat
        .iter()
        .filter(|m| !m.deleted)
        .filter(|m| {
            state
                .channel(&m.channel)
                .is_some_and(|c| c.can_view(&viewer.username))
        })
        .filter_map(|m| {
            let score = best_score(
                &[(m.body.as_str(), W_LABEL), (m.user.as_str(), W_ID)],
                &q.needle,
            )?;
            let hit = SearchHit {
                kind: SearchKind::Message,
                id: m.id.clone(),
                ref_: m.channel.clone(),
                label: clamp_label(&m.body),
                snippet: String::new(),
                sub: format!("{} · #{}", m.user, m.channel),
                at: m.at.clone(),
                link: SearchKind::Message.link(),
            };
            Some(Row {
                hit,
                score,
                at: m.at.clone(),
            })
        })
        .collect()
}

// --- ranking -----------------------------------------------------------------

/// The validated query: trimmed, refused when shorter than
/// [`MIN_QUERY_LEN`], truncated at [`MAX_QUERY_LEN`] characters, with the
/// lowercased needle used for matching.
struct SearchQuery {
    needle: String,
}

impl SearchQuery {
    fn new(q: &str) -> Option<Self> {
        let trimmed = q.trim();
        if trimmed.chars().count() < MIN_QUERY_LEN {
            return None;
        }
        Some(Self {
            needle: trimmed
                .chars()
                .take(MAX_QUERY_LEN)
                .collect::<String>()
                .to_lowercase(),
        })
    }
}

/// Best (weight × quality) over the row's fields; `None` when no field
/// matches. Max — not sum — so a needle appearing in both title and id does
/// not double-count a row past a better plain-title match.
fn best_score(fields: &[(&str, u32)], needle: &str) -> Option<u32> {
    fields
        .iter()
        .filter_map(|(text, weight)| quality(text, needle).map(|q| weight * q))
        .max()
}

/// Match quality over one field: prefix beats substring (SA weights).
fn quality(text: &str, needle: &str) -> Option<u32> {
    let hay = text.to_lowercase();
    if hay.is_empty() {
        return None;
    }
    let starts = hay.starts_with(needle);
    if !starts && !hay.contains(needle) {
        return None;
    }
    Some(if starts { Q_PREFIX } else { Q_SUBSTRING })
}

/// Rank, cap per kind, cap in total, group in stable kind order. Ordering:
/// score desc, then recency desc (RFC3339 UTC strings order chronologically
/// lexicographically; undated rows sink), then kind and id for determinism.
fn group_rows(rows: Vec<Row>) -> GroupedSearch {
    let mut rows = rows;
    rows.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| b.at.cmp(&a.at))
            .then_with(|| a.hit.kind.rank().cmp(&b.hit.kind.rank()))
            .then_with(|| a.hit.id.cmp(&b.hit.id))
    });
    // Per-kind cap first (AC5), remembering whether that kind overflowed.
    let mut per_kind: Vec<(SearchKind, bool, Vec<Row>)> = Vec::new();
    for row in rows {
        match per_kind.iter_mut().find(|(k, _, _)| *k == row.hit.kind) {
            Some((_, _, bucket)) => bucket.push(row),
            None => per_kind.push((row.hit.kind, false, vec![row])),
        }
    }
    let mut capped: Vec<Row> = Vec::new();
    let mut more_by_kind: Vec<(SearchKind, bool)> = Vec::new();
    for (kind, _, mut bucket) in per_kind {
        bucket.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| b.at.cmp(&a.at))
                .then_with(|| a.hit.id.cmp(&b.hit.id))
        });
        let has_more = bucket.len() > MAX_PER_KIND;
        more_by_kind.push((kind, has_more));
        capped.extend(bucket.into_iter().take(MAX_PER_KIND));
    }
    // Total cap across kinds bounds every response (O(1) wire size).
    capped.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| b.at.cmp(&a.at))
            .then_with(|| a.hit.kind.rank().cmp(&b.hit.kind.rank()))
            .then_with(|| a.hit.id.cmp(&b.hit.id))
    });
    capped.truncate(MAX_TOTAL);
    let mut groups = Vec::new();
    for kind in [
        SearchKind::Ticket,
        SearchKind::Page,
        SearchKind::Comment,
        SearchKind::Message,
    ] {
        let hits: Vec<SearchHit> = capped
            .iter()
            .filter(|r| r.hit.kind == kind)
            .map(|r| r.hit.clone())
            .collect();
        if hits.is_empty() {
            continue;
        }
        let has_more = more_by_kind.iter().any(|(k, more)| *k == kind && *more);
        groups.push(SearchGroup {
            kind,
            hits,
            has_more,
        });
    }
    GroupedSearch { groups }
}

// --- text shaping ------------------------------------------------------------

/// A body window centered on the match, `…`-elided; when the match was in the
/// title/id instead, the head of the body. Empty bodies yield nothing.
fn snippet(body: &str, needle: &str) -> String {
    let flat = body.trim();
    if flat.is_empty() {
        return String::new();
    }
    let lower = flat.to_lowercase();
    let Some(byte_pos) = lower.find(needle) else {
        return clamp_chars(flat, LABEL_MAX);
    };
    // The window is cut on CHARS, not bytes: `find` returns a byte offset,
    // and skipping that many chars on a multibyte body (the team writes
    // Vietnamese) would land the window short of the match. Unicode
    // lowercase maps 1:1 per char for the scripts this app ships, so the
    // match's char position carries over; an exotic expansion (İ → i̇)
    // shifts the window by a char, never panics or splits a glyph.
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

/// First `max` characters plus an ellipsis when truncated — a one-line label.
fn clamp_chars(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    format!("{}…", flat.chars().take(max).collect::<String>())
}

fn clamp_label(body: &str) -> String {
    clamp_chars(body, LABEL_MAX)
}

/// The domain status in its wire form (the label the board itself shows).
fn status_label(status: coxagent_domain::Status) -> String {
    serde_json::to_string(&status)
        .unwrap_or_default()
        .trim_matches('"')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ChatMsg, DocPage};
    use coxagent_domain::{
        Complexity, Priority, Role, Status, TechnicalDesign, Ticket, TicketId, TicketType,
    };

    // Fixtures over the REAL constructors — no fabricated shapes (the same
    // discipline as global_search_f275_tdd.rs, which drives the domain's
    // transition table instead of inventing rows).

    fn feature(id: &str, title: &str, description: &str) -> Ticket {
        Ticket::new(
            TicketId::new(id).expect("valid ticket id"),
            TicketType::Feature,
            title,
            description,
            Priority::High,
            Complexity::Medium,
            false,
        )
        .expect("valid ticket")
    }

    fn done_feature(id: &str, title: &str) -> Ticket {
        let mut t = feature(
            id,
            title,
            "the payment provider drops the connection mid-retry",
        );
        t.set_technical_design(
            Role::Sa,
            TechnicalDesign {
                approach: "exponential backoff on the retry loop".to_owned(),
                ..TechnicalDesign::default()
            },
        )
        .expect("SA owns the design");
        t.transition_to(Role::Sa, Status::Ready)
            .expect("designed feature readies");
        t.claim(Role::DevFeature, "finn@mac", "2026-08-30T09:00:00Z")
            .expect("claim is a legal edge");
        t.transition_to(Role::DevFeature, Status::Done)
            .expect("DEV completes the work");
        t
    }

    fn verified_bug(id: &str, title: &str) -> Ticket {
        let mut t = Ticket::new(
            TicketId::new(id).expect("valid ticket id"),
            TicketType::Bug,
            title,
            "the payment webhook times out on the second retry",
            Priority::High,
            Complexity::Small,
            false,
        )
        .expect("valid ticket");
        t.transition_to(Role::DevBug, Status::InProgress)
            .expect("bug claim is a legal edge");
        t.transition_to(Role::DevBug, Status::Fixed)
            .expect("DEV completes the fix");
        t.transition_to(Role::Test, Status::Verified)
            .expect("TEST renders the verify verdict");
        t
    }

    fn rejected_feature(id: &str, title: &str) -> Ticket {
        let mut t = feature(
            id,
            title,
            "duplicate of the payment retry work already shipped",
        );
        t.transition_to(Role::Po, Status::Rejected)
            .expect("PO owns the reject gate");
        t
    }

    fn wiki_page(id: &str, title: &str, body: &str) -> DocPage {
        DocPage {
            id: id.to_owned(),
            folder: "Product/Integrations".to_owned(),
            category: "product".to_owned(),
            title: title.to_owned(),
            body: body.to_owned(),
            updated_at: "2026-08-31T10:00:00Z".to_owned(),
            updated_by: "DOCS".to_owned(),
        }
    }

    fn chat_message(id: &str, user: &str, body: &str, channel: &str) -> ChatMsg {
        ChatMsg {
            id: id.to_owned(),
            at: "2026-08-31T09:41:00Z".to_owned(),
            user: user.to_owned(),
            body: body.to_owned(),
            edited: None,
            channel: channel.to_owned(),
            attachments: Vec::new(),
            reactions: Vec::new(),
            thread_id: None,
            reply_count: 0,
            deleted: false,
        }
    }

    fn member_viewer() -> AuthUser {
        AuthUser {
            username: "maya".to_owned(),
            name: String::new(),
            email: String::new(),
            role: AuthRole::Viewer,
            projects: vec!["cxa".to_owned()],
        }
    }

    fn outsider_lead() -> AuthUser {
        AuthUser {
            username: "mallory".to_owned(),
            name: String::new(),
            email: String::new(),
            role: AuthRole::TechLead,
            projects: vec!["other-project".to_owned()],
        }
    }

    fn searchable_state() -> ProjectState {
        let mut s = ProjectState::default();
        s.tickets
            .push(done_feature("CXC-F275-live", "Fix payment webhook retries"));
        s.docs.push(wiki_page(
            "doc-payments",
            "Payment webhook integration guide",
            "…configure retry with exponential backoff for failed payment hooks…",
        ));
        s.create_channel("Payments", "maya")
            .expect("maya creates the room");
        s.invite_to_channel("payments", "maya", "bob")
            .expect("maya invites bob");
        s.chat.push(chat_message(
            "msg-1",
            "maya",
            "the payment hook keeps timing out",
            "payments",
        ));
        s.chat.push(chat_message(
            "msg-2",
            "bob",
            "general chatter about the board",
            "general",
        ));
        s
    }

    fn ticket_hits(result: &GroupedSearch) -> Vec<&SearchHit> {
        result
            .groups
            .iter()
            .find(|g| g.kind == SearchKind::Ticket)
            .map(|g| g.hits.iter().collect())
            .unwrap_or_default()
    }

    // --- ranking (SA test plan) ---------------------------------------------

    #[test]
    fn title_prefix_beats_body_substring() {
        let mut s = ProjectState::default();
        s.tickets.push(feature(
            "CXC-F001",
            "Payment webhook retries",
            "unrelated body text",
        ));
        s.tickets.push(feature(
            "CXC-F002",
            "Unrelated title entirely",
            "somewhere in here it says payment once",
        ));
        let r = global_search(&s, "cxa", "payment", &member_viewer());
        let hits = ticket_hits(&r);
        assert_eq!(hits.len(), 2, "both tickets match");
        assert_eq!(hits[0].id, "CXC-F001", "the title match ranks first");
        assert_eq!(hits[0].sub, "pending", "sub carries the wire status");
    }

    #[test]
    fn id_matches_rank_between_title_and_body() {
        let mut s = ProjectState::default();
        s.tickets
            .push(feature("CXC-PAY-1", "Unrelated title", "unrelated body"));
        let r = global_search(&s, "cxa", "CXC-PAY", &member_viewer());
        assert_eq!(ticket_hits(&r).len(), 1, "the id field is searchable");
    }

    #[test]
    fn recency_breaks_score_ties() {
        let mut s = ProjectState::default();
        let mut older = feature("CXC-F001", "Payment webhook retries", "same body");
        older.stamp_created_at("2026-08-01T00:00:00Z");
        let mut newer = feature("CXC-F002", "Payment webhook budget", "same body");
        newer.stamp_created_at("2026-08-31T00:00:00Z");
        s.tickets.push(older);
        s.tickets.push(newer);
        let r = global_search(&s, "cxa", "payment webhook", &member_viewer());
        let hits = ticket_hits(&r);
        assert_eq!(
            hits[0].id, "CXC-F002",
            "the more recent ticket wins the tie"
        );
    }

    #[test]
    fn snippet_window_centers_the_match() {
        let mut s = ProjectState::default();
        let filler = "x".repeat(200);
        s.docs.push(wiki_page(
            "doc-1",
            "Deploy health gate",
            &format!("{filler} the health endpoint answers on the published port {filler}"),
        ));
        let r = global_search(&s, "cxa", "endpoint", &member_viewer());
        let page = r
            .groups
            .iter()
            .find(|g| g.kind == SearchKind::Page)
            .expect("the page matches")
            .hits[0]
            .clone();
        assert!(
            page.snippet.starts_with('…'),
            "the window is elided on the left"
        );
        assert!(
            page.snippet.contains("endpoint"),
            "the match stays inside the window"
        );
        assert!(
            page.snippet.chars().count() <= SNIPPET_WINDOW + 2,
            "the window is bounded"
        );
    }

    /// Regression: `find` returns a BYTE offset; cutting the window with it on
    /// a multibyte body skipped 1/3 chars per diacritic and pushed the match
    /// out of the window entirely. The team writes Vietnamese — this is the
    /// common case, not an edge.
    #[test]
    fn snippet_window_stays_char_safe_on_multibyte_bodies() {
        let mut s = ProjectState::default();
        let filler = "xưạế".repeat(60); // 240 multibyte chars before the match
        s.docs.push(wiki_page(
            "doc-vi",
            "Hướng dẫn cổng thanh toán",
            &format!("{filler} cổng thanh toán trả lỗi giữa.retry {filler}"),
        ));
        let r = global_search(&s, "cxa", "giữa", &member_viewer());
        let page = r
            .groups
            .iter()
            .find(|g| g.kind == SearchKind::Page)
            .expect("the page matches")
            .hits[0]
            .clone();
        assert!(
            page.snippet.contains("giữa"),
            "the multibyte match must sit inside its window, got {:?}",
            page.snippet
        );
        assert!(page.snippet.chars().count() <= SNIPPET_WINDOW + 2);
    }

    // --- AC4: minimum length and the explicit empty state -------------------

    #[test]
    fn queries_shorter_than_two_characters_yield_the_explicit_empty_state() {
        let s = searchable_state();
        for q in ["", "p", "  ", "  p  "] {
            let r = global_search(&s, "cxa", q, &member_viewer());
            assert!(
                r.is_empty(),
                "query {q:?} must return the empty result, not hits"
            );
            assert!(
                r.groups.is_empty(),
                "the empty state is explicit: no groups to render"
            );
        }
    }

    #[test]
    fn zero_matches_yield_the_explicit_empty_state() {
        let s = searchable_state();
        let r = global_search(&s, "cxa", "zzqx", &member_viewer());
        assert!(
            r.is_empty(),
            "no matches — the palette renders its no-results state"
        );
    }

    #[test]
    fn overlong_queries_are_truncated_not_errors() {
        let s = searchable_state();
        // 700 characters: truncated to MAX_QUERY_LEN, the needle simply no
        // longer matches a row — the contract is "never an error", and the
        // explicit empty state is exactly what the palette then shows.
        let q = "payment".repeat(100);
        let r = global_search(&s, "cxa", &q, &member_viewer());
        assert!(r.is_empty(), "a 700-char needle is bounded, not a crash");
    }

    // --- AC5: caps -----------------------------------------------------------

    #[test]
    fn per_kind_cap_bounds_the_list_and_reports_more() {
        let mut s = ProjectState::default();
        for i in 0..(MAX_PER_KIND + 4) {
            s.tickets.push(feature(
                &format!("CXC-F{i:03}"),
                &format!("payment backlog item {i:03}"),
                "seeded for the cap",
            ));
        }
        let r = global_search(&s, "cxa", "payment", &member_viewer());
        let group = r
            .groups
            .iter()
            .find(|g| g.kind == SearchKind::Ticket)
            .expect("tickets match");
        assert_eq!(group.hits.len(), MAX_PER_KIND, "the per-kind cap holds");
        assert!(
            group.has_more,
            "the overflow is reported as the show-more signal"
        );
    }

    #[test]
    fn the_total_cap_bounds_the_whole_response() {
        let mut s = ProjectState::default();
        // Four kinds at eleven matches each: 44 raw rows, 32 after the
        // per-kind cap — enough to prove the 30-total cap fires.
        for i in 0..11 {
            let tid = format!("CXC-F{i:03}");
            s.tickets
                .push(feature(&tid, &format!("payment ticket {i}"), "b"));
            s.docs.push(wiki_page(
                &format!("doc-{i}"),
                &format!("payment page {i}"),
                "b",
            ));
            s.post_comment("USER", &format!("payment thread {i}"), Some(tid));
            s.chat.push(chat_message(
                &format!("m{i}"),
                "maya",
                &format!("payment message {i}"),
                "general",
            ));
        }
        let r = global_search(&s, "cxa", "payment", &member_viewer());
        assert_eq!(r.flat().len(), MAX_TOTAL, "the whole response stays capped");
        assert_eq!(
            r.groups.len(),
            4,
            "every kind still contributes under the cap"
        );
    }

    // --- row hygiene ---------------------------------------------------------

    #[test]
    fn deleted_chat_messages_are_excluded() {
        let mut s = ProjectState::default();
        let mut gone = chat_message("m-gone", "maya", "payment secret note", "general");
        gone.deleted = true;
        s.chat.push(gone);
        s.chat.push(chat_message(
            "m-here",
            "bob",
            "payment note that stays",
            "general",
        ));
        let r = global_search(&s, "cxa", "payment", &member_viewer());
        let msgs = &r
            .groups
            .iter()
            .find(|g| g.kind == SearchKind::Message)
            .expect("one hit")
            .hits;
        assert_eq!(msgs.len(), 1);
        assert_eq!(
            msgs[0].id, "m-here",
            "the soft-deleted message never surfaces"
        );
    }

    #[test]
    fn empty_title_pages_are_excluded() {
        let mut s = ProjectState::default();
        s.docs.push(wiki_page(
            "doc-blank",
            "   ",
            "payment body without a title",
        ));
        s.docs
            .push(wiki_page("doc-ok", "Payment guide", "payment body"));
        let r = global_search(&s, "cxa", "payment", &member_viewer());
        let pages = &r
            .groups
            .iter()
            .find(|g| g.kind == SearchKind::Page)
            .expect("one hit")
            .hits;
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].id, "doc-ok");
    }

    // --- AC3: closed tickets are an explicit decision, never an error --------

    #[test]
    fn closed_tickets_search_like_any_other_row_without_error() {
        let mut s = ProjectState::default();
        s.tickets
            .push(done_feature("CXC-F001", "Fix payment webhook retries"));
        s.tickets
            .push(verified_bug("CXC-B001", "Payment webhook timeout"));
        s.tickets
            .push(rejected_feature("CXC-F002", "Duplicate payment retry idea"));
        let r = global_search(&s, "cxa", "payment", &member_viewer());
        let hits = ticket_hits(&r);
        assert_eq!(
            hits.len(),
            3,
            "closed tickets are the prior art — they surface"
        );
        let subs: Vec<&str> = hits.iter().map(|h| h.sub.as_str()).collect();
        assert!(
            subs.contains(&"done") && subs.contains(&"verified") && subs.contains(&"rejected"),
            "each closed ticket carries its terminal status: {subs:?}"
        );
    }

    // --- AC2: scoping --------------------------------------------------------

    #[test]
    fn an_outside_role_sees_nothing_from_a_project_it_is_not_a_member_of() {
        let s = searchable_state();
        let r = global_search(&s, "cxa", "payment", &outsider_lead());
        assert!(r.is_empty(), "role authority is not project access (AC2)");
    }

    #[test]
    fn a_read_only_member_still_sees_results() {
        let s = searchable_state();
        let r = global_search(&s, "cxa", "payment", &member_viewer());
        assert!(
            !r.is_empty(),
            "Viewer is read-only, not blind — membership is the gate"
        );
    }

    #[test]
    fn super_admin_sees_every_project_like_the_rest_of_the_api() {
        let s = searchable_state();
        let super_no_membership = AuthUser {
            username: "root".to_owned(),
            name: String::new(),
            email: String::new(),
            role: AuthRole::Super,
            projects: Vec::new(),
        };
        let r = global_search(&s, "cxa", "payment", &super_no_membership);
        assert!(
            !r.is_empty(),
            "the documented house bypass holds for search too"
        );
    }

    #[test]
    fn private_channel_threads_never_leak_to_non_members() {
        let s = searchable_state();
        // maya (owner) sees her room; mallory (not invited) must not — even
        // though the project-level check would pass for a member of `cxa`.
        let member_outside_room = AuthUser {
            username: "carol".to_owned(),
            name: String::new(),
            email: String::new(),
            role: AuthRole::Be,
            projects: vec!["cxa".to_owned()],
        };
        let r = global_search(&s, "cxa", "timing out", &member_outside_room);
        assert!(
            r.is_empty(),
            "the private room's message is invisible to a non-member"
        );
        let r = global_search(&s, "cxa", "timing out", &member_viewer());
        assert_eq!(r.flat().len(), 1, "the owner sees her own room's message");
    }

    #[test]
    fn general_channel_messages_are_visible_to_every_member() {
        let s = searchable_state();
        let r = global_search(&s, "cxa", "board", &member_viewer());
        let msgs = r.flat();
        assert!(
            msgs.iter()
                .any(|h| h.kind == SearchKind::Message && h.ref_ == "general"),
            "#general is open: {msgs:?}"
        );
    }

    // --- merging several projects (the endpoint's no-pid sweep) --------------

    #[test]
    fn many_projects_merge_into_one_capped_ranked_response() {
        let mut a = searchable_state();
        a.tickets
            .push(feature("CXC-F900", "Payment across projects", "b"));
        let mut b = searchable_state();
        b.docs.push(wiki_page("doc-b", "Payment runbook", "b"));
        let member_of_both = AuthUser {
            username: "maya".to_owned(),
            name: String::new(),
            email: String::new(),
            role: AuthRole::Be,
            projects: vec!["cxa".to_owned(), "other".to_owned()],
        };
        let joined = [("cxa", &a), ("other", &b)];
        let r = global_search_many(joined, "payment", &member_of_both);
        assert!(
            ticket_hits(&r).iter().any(|h| h.id == "CXC-F900"),
            "project A's tickets surface"
        );
        assert!(
            r.flat()
                .iter()
                .any(|h| h.kind == SearchKind::Page && h.id == "doc-b"),
            "project B's pages merge into the same response"
        );
    }

    #[test]
    fn the_sweep_skips_projects_the_viewer_cannot_see() {
        let a = searchable_state();
        let b = searchable_state();
        let joined = [("cxa", &a), ("other", &b)];
        let r = global_search_many(joined, "payment", &outsider_lead());
        assert!(r.is_empty(), "the outsider is a member of neither project");
    }
}
