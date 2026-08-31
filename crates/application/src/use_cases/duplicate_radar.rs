//! Cross-project duplicate radar (CXA-F253) — the PURE decision core.
//!
//! Each project's BA dedupes only against its own board, so the same generic
//! feature ("add a cycle-performance dashboard") gets independently invented
//! and built in every project on the hub. This module runs the SAME similarity
//! predicate the per-project gates use ([`crate::parsing::jaccard`]) over a
//! cross-project registry of committed ticket titles + scopes, and reports the
//! matching pairs. It never suppresses anything by itself: matches surface as
//! radar entries a human resolves (redirect / reject / allow), and pairs the
//! human allowed are excluded here via the persisted allowlist.
//!
//! Layering: no IO, no framework imports — the caller (the HTTP adapter)
//! gathers [`TicketSnapshot`]s through `StateStorePort` and hands them in.
//! Only title + scope metadata crosses projects, never raw documents.

use crate::parsing::{jaccard, normalize_title, title_tokens};
use std::collections::HashMap;

/// Similarity above which two titles are near-paraphrase duplicates — the
/// exact bar [`crate::parsing::duplicates_existing`] enforces within one
/// project, so "duplicate" means the same thing here as everywhere else.
pub const DUPE_THRESHOLD: f64 = 0.6;

/// Stricter bar for tickets that BOTH carry the same bounded-context service
/// tag (AC4): shared infrastructure legitimately repeats across projects, so
/// a paraphrase-level match is presumed legitimate and skipped — only a
/// near-identical title still surfaces for a human decision (who can allow
/// the pair permanently).
pub const SAME_TAG_DUPE_THRESHOLD: f64 = 0.9;

/// Normalized titles shorter than this are noise ("x", "ai") and never match.
pub const MIN_TITLE_LEN: usize = 4;

/// One ticket's lightweight, cross-project-safe metadata: identifiers, the
/// title, a trimmed scope statement, and the bounded-context service tag.
/// This is ALL the radar sees — content isolation between tenants holds
/// because no raw document ever enters a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketSnapshot {
    pub project_id: String,
    pub project_name: String,
    pub ticket_id: String,
    pub title: String,
    /// The ticket's scope statement (its description, trimmed by the caller).
    pub scope: String,
    /// Bounded-context service tag, when the BA marked shared infrastructure.
    pub service_tag: Option<String>,
}

/// One side of a radar entry as reported to the human.
#[derive(Debug, Clone, PartialEq)]
pub struct RadarTicket {
    pub project_id: String,
    pub project_name: String,
    pub ticket_id: String,
    pub title: String,
    pub scope: String,
    /// Similarity to the entry's home ticket (1.0 when titles normalize
    /// identically). Absent on the home side — a pair property, not a
    /// ticket property.
    pub score: Option<f64>,
}

/// One radar entry: the HOME ticket plus every ticket in OTHER projects that
/// matches it. Identical normalized titles collapse into one entry (home +
/// several dups); near-paraphrase matches stay one-to-one.
#[derive(Debug, Clone, PartialEq)]
pub struct DuplicatePair {
    pub home: RadarTicket,
    pub dups: Vec<RadarTicket>,
    /// The normalized title this entry collapsed on — the group key when
    /// several projects filed the identical title, else the home title's norm.
    pub normalized_title: String,
}

/// Stable, order-independent key for one cross-project ticket pair — the
/// persisted allowlist is keyed by exactly this (the SA ruling: pair-keyed,
/// never keyed by free-text title which breaks on rename).
#[must_use]
pub fn pair_key(a: (&str, &str), b: (&str, &str)) -> String {
    let ka = format!("{}/{}", a.0, a.1);
    let kb = format!("{}/{}", b.0, b.1);
    if ka <= kb {
        format!("{ka}|{kb}")
    } else {
        format!("{kb}|{ka}")
    }
}

/// Do both tickets carry the SAME bounded-context service tag? Comparison is
/// trimmed and case-insensitive — the tag is human-authored free text, and a
/// capitalization difference must not silently void the carve-out.
fn same_service_tag(a: &TicketSnapshot, b: &TicketSnapshot) -> bool {
    match (&a.service_tag, &b.service_tag) {
        (Some(x), Some(y)) => x.trim().eq_ignore_ascii_case(y.trim()),
        _ => false,
    }
}

struct Entry<'a> {
    snap: &'a TicketSnapshot,
    norm: String,
    toks: std::collections::HashSet<String>,
}

/// Find cross-project duplicate candidates: pure over the snapshots, minus
/// whatever pairs `allowed_pairs` (persisted allowlist keys from
/// [`pair_key`]) the human already resolved as "keep both".
#[must_use]
pub fn find_cross_project_duplicates(
    snapshots: &[TicketSnapshot],
    allowed_pairs: &[String],
) -> Vec<DuplicatePair> {
    let entries: Vec<Entry<'_>> = snapshots
        .iter()
        .map(|s| Entry {
            norm: normalize_title(&s.title),
            toks: title_tokens(&s.title),
            snap: s,
        })
        .filter(|e| e.norm.len() >= MIN_TITLE_LEN)
        .collect();
    let mut collapsed: Vec<bool> = vec![false; entries.len()];
    let mut out = collapse_identical_titles(&entries, allowed_pairs, &mut collapsed);
    out.extend(paraphrase_pairs(&entries, allowed_pairs, &collapsed));
    out.sort_by(|x, y| {
        (&x.home.project_name, &x.home.title).cmp(&(&y.home.project_name, &y.home.title))
    });
    out
}

/// Pass 1 — identical normalized titles collapse into one entry per unique
/// title, however many projects filed it (the SA contract: "each unique
/// normalized title appears once"). Members marked in `collapsed` are skipped
/// by the paraphrase pass.
fn collapse_identical_titles(
    entries: &[Entry<'_>],
    allowed_pairs: &[String],
    collapsed: &mut [bool],
) -> Vec<DuplicatePair> {
    let mut by_norm: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        by_norm.entry(e.norm.as_str()).or_default().push(i);
    }
    let mut out = Vec::new();
    for idxs in by_norm.values() {
        if idxs.len() < 2 {
            continue;
        }
        let mut members: Vec<usize> = idxs.clone();
        members.sort_by(|x, y| {
            let (a, b) = (&entries[*x].snap, &entries[*y].snap);
            (&a.project_name, &a.ticket_id).cmp(&(&b.project_name, &b.ticket_id))
        });
        // Members of one group must span at least two projects — a project
        // matching itself is the per-project gate's job, not the radar's.
        let projects: std::collections::HashSet<&str> = members
            .iter()
            .map(|i| entries[*i].snap.project_id.as_str())
            .collect();
        if projects.len() < 2 {
            continue;
        }
        let home_i = members[0];
        let home = &entries[home_i].snap;
        let mut dups = Vec::new();
        for i in members.into_iter().skip(1) {
            let m = &entries[i].snap;
            if m.project_id == home.project_id {
                continue;
            }
            if allowed_pairs.contains(&pair_key(
                (&home.project_id, &home.ticket_id),
                (&m.project_id, &m.ticket_id),
            )) {
                continue;
            }
            dups.push(RadarTicket {
                project_id: m.project_id.clone(),
                project_name: m.project_name.clone(),
                ticket_id: m.ticket_id.clone(),
                title: m.title.clone(),
                scope: m.scope.clone(),
                score: Some(1.0),
            });
            collapsed[i] = true;
        }
        if dups.is_empty() {
            continue;
        }
        collapsed[home_i] = true;
        out.push(DuplicatePair {
            normalized_title: entries[home_i].norm.clone(),
            home: radar_home(home),
            dups,
        });
    }
    out
}

/// Pass 2 — near-paraphrase matches (different normalized titles, same
/// content tokens): one-to-one entries. The home side is the stable-lower
/// of the two so the radar does not flip-flop between reloads.
fn paraphrase_pairs(
    entries: &[Entry<'_>],
    allowed_pairs: &[String],
    collapsed: &[bool],
) -> Vec<DuplicatePair> {
    let mut out = Vec::new();
    for (i, a) in entries.iter().enumerate() {
        if collapsed[i] {
            continue;
        }
        for b in entries.iter().skip(i + 1) {
            if b.norm == a.norm {
                continue; // identical-title pairs were handled by the collapse
            }
            let score = jaccard(&a.toks, &b.toks);
            if score < DUPE_THRESHOLD {
                continue;
            }
            // AC4 carve-out: both sides tagged with the SAME bounded-context
            // tag are presumed legitimately-shared infrastructure unless they
            // are near-identical beyond the stricter bar.
            if same_service_tag(a.snap, b.snap) && score < SAME_TAG_DUPE_THRESHOLD {
                continue;
            }
            if allowed_pairs.contains(&pair_key(
                (&a.snap.project_id, &a.snap.ticket_id),
                (&b.snap.project_id, &b.snap.ticket_id),
            )) {
                continue;
            }
            let (home, dup, score) = if (&a.snap.project_name, &a.snap.ticket_id)
                <= (&b.snap.project_name, &b.snap.ticket_id)
            {
                (a.snap, b.snap, score)
            } else {
                (b.snap, a.snap, score)
            };
            out.push(DuplicatePair {
                normalized_title: normalize_title(&home.title),
                home: radar_home(home),
                dups: vec![RadarTicket {
                    project_id: dup.project_id.clone(),
                    project_name: dup.project_name.clone(),
                    ticket_id: dup.ticket_id.clone(),
                    title: dup.title.clone(),
                    scope: dup.scope.clone(),
                    score: Some(score),
                }],
            });
        }
    }
    out
}

fn radar_home(s: &TicketSnapshot) -> RadarTicket {
    RadarTicket {
        project_id: s.project_id.clone(),
        project_name: s.project_name.clone(),
        ticket_id: s.ticket_id.clone(),
        title: s.title.clone(),
        scope: s.scope.clone(),
        score: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(project: &str, name: &str, ticket: &str, title: &str) -> TicketSnapshot {
        TicketSnapshot {
            project_id: project.to_owned(),
            project_name: name.to_owned(),
            ticket_id: ticket.to_owned(),
            title: title.to_owned(),
            scope: format!("scope of {ticket}"),
            service_tag: None,
        }
    }

    fn tagged(s: TicketSnapshot, tag: &str) -> TicketSnapshot {
        TicketSnapshot {
            service_tag: Some(tag.to_owned()),
            ..s
        }
    }

    #[test]
    fn two_projects_sharing_a_title_collapse_into_one_entry() {
        let out = find_cross_project_duplicates(
            &[
                snap("p1", "Alpha", "T-9", "Fix flaky login"),
                snap("p2", "Beta", "T-2", "fix flaky   LOGIN"),
            ],
            &[],
        );
        assert_eq!(out.len(), 1, "one unique normalized title, one entry");
        let e = &out[0];
        assert_eq!((e.home.project_id.as_str(), e.home.ticket_id.as_str()), ("p1", "T-9"));
        assert_eq!(e.dups.len(), 1);
        assert_eq!(e.dups[0].project_id, "p2");
        assert_eq!(e.dups[0].score, Some(1.0), "identical normalized titles");
        assert_eq!(e.normalized_title, "fix flaky login");
    }

    #[test]
    fn the_same_title_within_one_project_never_matches() {
        let out = find_cross_project_duplicates(
            &[
                snap("p1", "Alpha", "T-1", "Fix flaky login"),
                snap("p1", "Alpha", "T-2", "Fix flaky login"),
            ],
            &[],
        );
        assert!(out.is_empty(), "the radar is cross-project only");
    }

    #[test]
    fn different_titles_across_projects_stay_silent() {
        let out = find_cross_project_duplicates(
            &[
                snap("p1", "Alpha", "T-1", "Add CORS middleware"),
                snap("p2", "Beta", "T-1", "Per-engine cost leaderboard"),
            ],
            &[],
        );
        assert!(out.is_empty());
    }

    #[test]
    fn tiny_or_blank_titles_are_ignored() {
        let out = find_cross_project_duplicates(
            &[
                snap("p1", "Alpha", "T-1", "AI"),
                snap("p2", "Beta", "T-1", "   "),
                snap("p2", "Beta", "T-2", "a"),
            ],
            &[],
        );
        assert!(out.is_empty(), "noise titles never match, not even each other");
    }

    #[test]
    fn matching_is_case_and_spacing_insensitive_but_display_casing_survives() {
        let out = find_cross_project_duplicates(
            &[
                snap("p1", "Alpha", "T-1", "Surface Burn Trends"),
                snap("p2", "Beta", "T-1", "surface burn trends"),
            ],
            &[],
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].home.title, "Surface Burn Trends");
        assert_eq!(out[0].dups[0].title, "surface burn trends");
    }

    #[test]
    fn ac4_same_tag_paraphrases_are_presumed_legitimate_shared_infra() {
        // Same bounded-context tag, near-paraphrase titles (jaccard between
        // DUPE_THRESHOLD and the stricter bar): skipped by the carve-out…
        let pair = vec![
            tagged(
                snap("p1", "Alpha", "T-1", "Add cycle performance dashboard"),
                "infra",
            ),
            tagged(
                snap("p2", "Beta", "T-1", "Add cycle performance dashboard with alerts"),
                "infra",
            ),
        ];
        let j = jaccard(&title_tokens(&pair[0].title), &title_tokens(&pair[1].title));
        assert!(
            (DUPE_THRESHOLD..SAME_TAG_DUPE_THRESHOLD).contains(&j),
            "fixture must sit inside the carve-out band, got {j}"
        );
        assert!(find_cross_project_duplicates(&pair, &[]).is_empty());
        // …but the SAME paraphrase without tags is a normal radar match.
        let untagged: Vec<TicketSnapshot> = pair
            .iter()
            .map(|s| TicketSnapshot { service_tag: None, ..s.clone() })
            .collect();
        assert_eq!(find_cross_project_duplicates(&untagged, &[]).len(), 1);
        // …and DIFFERENT tags do not void the match either.
        let mixed = vec![
            tagged(
                snap("p1", "Alpha", "T-1", "Add cycle performance dashboard"),
                "infra",
            ),
            tagged(
                snap("p2", "Beta", "T-1", "Add cycle performance dashboard with alerts"),
                "ci",
            ),
        ];
        assert_eq!(find_cross_project_duplicates(&mixed, &[]).len(), 1);
    }

    #[test]
    fn ac4_identical_same_tag_titles_still_surface_beyond_the_stricter_bar() {
        // Exact-identical titles carry similarity 1.0, which exceeds the
        // stricter test — they surface for a human decision, who can allow
        // the pair permanently via the allowlist.
        let pair = vec![
            tagged(snap("p1", "Alpha", "T-1", "Add Redis cache layer"), "infra"),
            tagged(snap("p2", "Beta", "T-1", "Add Redis cache layer"), "infra"),
        ];
        let out = find_cross_project_duplicates(&pair, &[]);
        assert_eq!(out.len(), 1, "1.0 >= SAME_TAG_DUPE_THRESHOLD surfaces");
        // The human allows it — the pair never comes back.
        let key = pair_key(("p1", "T-1"), ("p2", "T-1"));
        assert!(find_cross_project_duplicates(&pair, &[key]).is_empty());
    }

    #[test]
    fn allowed_pairs_are_excluded_but_unrelated_matches_survive() {
        let snaps = vec![
            snap("p1", "Alpha", "T-1", "Add dark mode toggle"),
            snap("p2", "Beta", "T-1", "Add dark mode toggle"),
            snap("p3", "Gamma", "T-1", "Add dark mode toggle"),
        ];
        // Allowing only the p1–p3 pair leaves the p1–p2 match visible.
        let key = pair_key(("p1", "T-1"), ("p3", "T-1"));
        let out = find_cross_project_duplicates(&snaps, std::slice::from_ref(&key));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].dups.len(), 1, "p3 collapsed away");
        assert_eq!(out[0].dups[0].project_id, "p2");
        // The key is order-independent.
        assert_eq!(key, pair_key(("p3", "T-1"), ("p1", "T-1")));
    }

    #[test]
    fn three_projects_filing_one_title_produce_one_entry_with_two_dups() {
        let out = find_cross_project_duplicates(
            &[
                snap("p2", "Beta", "T-1", "Add cycle performance dashboard"),
                snap("p1", "Alpha", "T-7", "Add cycle performance dashboard"),
                snap("p3", "Gamma", "T-2", "Add cycle performance dashboard"),
            ],
            &[],
        );
        assert_eq!(out.len(), 1);
        // Home is the stable-lowest member by project name.
        assert_eq!(out[0].home.project_name, "Alpha");
        let dup_projects: Vec<&str> = out[0].dups.iter().map(|d| d.project_name.as_str()).collect();
        assert_eq!(dup_projects, vec!["Beta", "Gamma"]);
    }

    #[test]
    fn paraphrase_matches_stay_one_to_one_and_sorted_by_home_project() {
        // Same content tokens but a different normalized title — the
        // identical-title collapse must not merge these into one group.
        let out = find_cross_project_duplicates(
            &[
                snap("p1", "Alpha", "T-1", "Add cycle performance dashboard"),
                snap("p2", "Beta", "T-1", "Add cycle performance dashboard with alerts"),
            ],
            &[],
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].home.project_name, "Alpha");
        let score = out[0].dups[0].score.expect("pair score");
        assert!(
            (DUPE_THRESHOLD..SAME_TAG_DUPE_THRESHOLD).contains(&score),
            "paraphrase sits below the identical-title collapse, got {score}"
        );
    }

    #[test]
    fn entries_carry_ids_projects_scopes_and_scores_for_the_view() {
        let out = find_cross_project_duplicates(
            &[
                snap("p1", "Alpha", "T-9", "Fix flaky login"),
                snap("p2", "Beta", "T-2", "Fix flaky login"),
            ],
            &[],
        );
        let e = &out[0];
        // AC2: ids, projects, scopes and the similarity score are all present.
        assert!(!e.home.scope.is_empty());
        assert_eq!(e.dups[0].scope, "scope of T-2");
        assert_eq!(e.dups[0].ticket_id, "T-2");
        assert_eq!(e.home.score, None);
    }
}
