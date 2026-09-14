//! Tiered repo-map rendering (CXA-F316).
//!
//! The orientation contract is "read `.coxagent/REPO_MAP.md` first", so the map
//! must cover the whole tree — or say exactly what it left out. Two stacked
//! silent truncations used to break that: the old renderer walked files
//! lexicographically and stopped dead at the byte budget (the on-disk map died
//! inside `crates/application/src/`, so `crates/domain`, `web/`, `e2e/` and the
//! rest never appeared), and `prompts::repo_map_block` then re-chopped that
//! already-truncated file to its first 3,000 chars. Both cuts live here now:
//!
//! * [`render`] — pure and budgeted, in tiers. Tier 0 (header with freshness
//!   and a `Coverage:` line, plus the top-level area rollup) always renders
//!   whole; per-file detail is allocated from the remaining budget in equal
//!   per-area shares so a huge early group can never starve the groups behind
//!   it; files that do not fit are listed as paths (compact tier); anything
//!   still left out is named by an explicit elision footer.
//! * [`prompt_slice`] — section-aware compaction of a rendered map for agent
//!   briefs: the tier-0 header (with the directory summary) is kept whole,
//!   whole `##` sections follow while they fit, and dropped sections are
//!   counted in a footer. Never a blind first-N-chars cut.

use crate::codegraph::{CodeGraph, FileNode, Symbol};
use std::collections::BTreeMap;

/// Bytes held back for the elision footer whenever the detail tier cannot take
/// every file, so stating what was omitted never itself overflows the budget.
const FOOTER_RESERVE: usize = 300;
/// Upper bound on the rendered `Coverage:` line (only the count digits vary);
/// keeps the tier arithmetic honest without a second assembly pass.
const COVERAGE_RESERVE: usize = 80;
/// Bytes held back for the prompt slice's "sections elided" footer.
const SLICE_FOOTER_RESERVE: usize = 200;

/// How a rendered map covered the graph: `detailed` files carry their symbol
/// listings, `compact` files appear as paths only, and the remainder are named
/// by the elision footer (`detailed + compact + elided == files_total`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapCoverage {
    pub files_total: usize,
    pub detailed: usize,
    pub compact: usize,
}

impl MapCoverage {
    /// Files present in the map in full or as paths.
    #[must_use]
    pub fn covered(&self) -> usize {
        self.detailed + self.compact
    }

    /// Files named by neither tier — the elision footer's count.
    #[must_use]
    pub fn elided(&self) -> usize {
        self.files_total.saturating_sub(self.covered())
    }

    /// The additive header line the artifact contract pins, e.g.
    /// `Coverage: 700/764 files (420 detailed · 280 compact)`.
    fn line(&self) -> String {
        format!(
            "Coverage: {}/{} files ({} detailed · {} compact)\n",
            self.covered(),
            self.files_total,
            self.detailed,
            self.compact
        )
    }
}

/// One top-level area: `crates/<crate>` for crate files, the first path
/// segment for anything else, `(root)` for top-level files.
struct Area {
    name: String,
    files: Vec<usize>,
}

fn area_name(path: &str) -> String {
    match path.split_once('/') {
        None => "(root)".to_owned(),
        Some(("crates", rest)) => match rest.split_once('/') {
            Some((crate_name, _)) => format!("crates/{crate_name}"),
            None => "crates".to_owned(),
        },
        Some((top, _)) => top.to_owned(),
    }
}

/// Group files by area. Indices reference `g.files`; areas come out name-
/// ordered, so the rendering is deterministic regardless of input order.
fn areas_of(g: &CodeGraph) -> Vec<Area> {
    let mut by_name: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, f) in g.files.iter().enumerate() {
        by_name.entry(area_name(&f.path)).or_default().push(i);
    }
    by_name
        .into_iter()
        .map(|(name, files)| Area { name, files })
        .collect()
}

/// Symbols grouped per file, preserving the graph's (name-sorted) order — the
/// per-file listing stays byte-identical to the pre-F316 renderer's.
fn symbols_by_file(g: &CodeGraph) -> BTreeMap<&str, Vec<&Symbol>> {
    let mut by_file: BTreeMap<&str, Vec<&Symbol>> = BTreeMap::new();
    for s in &g.symbols {
        by_file.entry(s.file.as_str()).or_default().push(s);
    }
    by_file
}

/// One file's detail section — same shape as the pre-F316 renderer's.
fn detail_section(f: &FileNode, syms: &[&Symbol]) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "## {} ({})", f.path, f.lang);
    if !syms.is_empty() {
        let list: Vec<String> = syms
            .iter()
            .take(12)
            .map(|sy| match &sy.scope {
                Some(sc) => format!("{} {sc}::{}", sy.kind, sy.name),
                None => format!("{} {}", sy.kind, sy.name),
            })
            .collect();
        let _ = writeln!(s, "  {}", list.join(", "));
    }
    s
}

/// Tier-0 header without the `Coverage:` line, which needs the tier outcome.
fn header(g: &CodeGraph) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "# Repo map — {} files, {} symbols\n\
         Query it (if `coxagent` is on PATH): `coxagent codegraph search|impact|callers <name>`.",
        g.files.len(),
        g.symbols.len()
    );
    // Freshness: an agent or human can tell how stale the orientation is.
    // `index` always sets `built_at`; a default-built graph claims nothing.
    if !g.built_at.is_empty() {
        if let Some(sha) = g.head_sha.as_deref() {
            let _ = writeln!(s, "Generated: {} · HEAD: {}", g.built_at, sha);
        } else {
            let _ = writeln!(s, "Generated: {}", g.built_at);
        }
    }
    if !g.languages.is_empty() {
        let langs: Vec<String> = g
            .languages
            .iter()
            .map(|(l, n)| format!("{l} {n}"))
            .collect();
        let _ = writeln!(s, "Languages: {}", langs.join(", "));
    }
    s
}

/// Tier-0 area rollup: names EVERY top-level area with its file/symbol counts,
/// so no part of the tree is ever invisible, whatever the budget.
fn areas_block(g: &CodeGraph, areas: &[Area]) -> String {
    use std::fmt::Write as _;
    let mut s = String::from("# Top-level areas\n");
    for a in areas {
        let syms: usize = a.files.iter().map(|&i| g.files[i].symbols).sum();
        let _ = writeln!(
            s,
            "- {} — {} files, {} symbols",
            a.name,
            a.files.len(),
            syms
        );
    }
    s
}

/// The explicit loss signal that replaces the old silent mid-file stop.
fn elision_footer(
    coverage: MapCoverage,
    syms_total: usize,
    syms_omitted: usize,
    max_chars: usize,
) -> String {
    format!(
        "\n… {} of {} files listed as paths only, {} of {} files omitted — \
         {} of {} symbols not shown (map budget {max_chars} chars); \
         query the code graph: `coxagent codegraph search|impact|callers <name>`.\n",
        coverage.compact,
        coverage.files_total,
        coverage.elided(),
        coverage.files_total,
        syms_omitted,
        syms_total,
    )
}

/// Tier 2: one "paths only" line per area for the files that got no detail,
/// whole lines only, while `spent` stays within `budget`. The caller invokes
/// this only when the detail tier cannot take every file. Returns the compact
/// text, per-file membership flags, and the number of files covered — spend is
/// accumulated in place so the caller's budget arithmetic stays honest.
fn compact_pass(
    g: &CodeGraph,
    areas: &[Area],
    in_detail: &[bool],
    budget: usize,
    spent: &mut usize,
) -> (String, Vec<bool>, usize) {
    use std::fmt::Write as _;
    let mut compact = String::new();
    let mut in_compact = vec![false; g.files.len()];
    let mut compacted = 0usize;
    for a in areas {
        let rest: Vec<usize> = a.files.iter().copied().filter(|&i| !in_detail[i]).collect();
        if rest.is_empty() {
            continue;
        }
        let mut line = String::new();
        let _ = write!(
            line,
            "## {} — {} more files, paths only: ",
            a.name,
            rest.len()
        );
        let paths: Vec<&str> = rest.iter().map(|&i| g.files[i].path.as_str()).collect();
        line.push_str(&paths.join(", "));
        line.push('\n');
        if *spent + line.len() <= budget {
            compact.push_str(&line);
            *spent += line.len();
            for &i in &rest {
                in_compact[i] = true;
            }
            compacted += rest.len();
        }
    }
    (compact, in_compact, compacted)
}

/// Render the tiered repo map for `g` within `max_chars`. Pure: a function of
/// the graph alone, no IO.
///
/// Tier 0 (header + coverage + area rollup) always renders whole, even when
/// the budget is smaller than the summary itself; the detail and compact tiers
/// share what is left, and the elision footer accounts for every file that
/// made neither tier.
#[must_use]
pub fn render(g: &CodeGraph, max_chars: usize) -> String {
    let areas = areas_of(g);
    let by_file = symbols_by_file(g);
    let head = header(g);
    let rollup = areas_block(g, &areas);

    // Detail text per file, built once and measured.
    let sections: Vec<String> = g
        .files
        .iter()
        .map(|f| {
            detail_section(
                f,
                by_file
                    .get(f.path.as_str())
                    .map_or(&[][..], |v| v.as_slice()),
            )
        })
        .collect();

    let total_detail: usize = sections.iter().map(String::len).sum();
    let fixed = head.len() + COVERAGE_RESERVE + rollup.len() + 2;
    // The detail tier cannot take every file: the compact tier and the elision
    // footer both exist only for this case.
    let detail_overflows = fixed + total_detail > max_chars;
    let reserve = if detail_overflows { FOOTER_RESERVE } else { 0 };
    let budget = max_chars.saturating_sub(fixed + reserve);
    let mut spent = 0usize;
    let mut in_detail = vec![false; g.files.len()];

    // Tier 1, round 1 — equal per-area shares of the budget. The pre-F316
    // renderer stopped dead at the first overrun, silently dropping every file
    // alphabetically after it; shares guarantee every area gets detail slots.
    for a in &areas {
        let share = budget / areas.len().max(1);
        let mut area_spent = 0usize;
        for &i in &a.files {
            let len = sections[i].len();
            if area_spent + len > share {
                continue;
            }
            area_spent += len;
            in_detail[i] = true;
        }
        spent += area_spent;
    }

    // Tier 2 — only when the detail tier overflows: files that got no detail
    // are listed as paths, grouped per area, whole lines only, while they fit.
    // Breadth before depth — paths for unseen areas beat extra detail scraps —
    // but never at the cost of downgrading a file whose detail still fits.
    let (compact, in_compact, compacted) = if detail_overflows {
        compact_pass(g, &areas, &in_detail, budget, &mut spent)
    } else {
        (String::new(), vec![false; g.files.len()], 0)
    };

    // Tier 1, round 2 — whatever the shares left unspent goes to files that
    // made neither tier, skipping anything that does not fit whole. When the
    // detail tier does NOT overflow, every share-busting file left out of
    // round 1 fits here: budget >= total detail.
    for a in &areas {
        for &i in &a.files {
            if in_detail[i] || in_compact[i] || spent + sections[i].len() > budget {
                continue;
            }
            spent += sections[i].len();
            in_detail[i] = true;
        }
    }

    let coverage = MapCoverage {
        files_total: g.files.len(),
        detailed: in_detail.iter().filter(|&&d| d).count(),
        compact: compacted,
    };

    // Assemble: grouped detail (area order), then compact paths, then footer —
    // the map ends with the loss signal, never a silent stop.
    let mut detail_txt = String::new();
    for (i, sec) in sections.iter().enumerate() {
        if in_detail[i] {
            detail_txt.push_str(sec);
        }
    }

    let mut out = head;
    out.push_str(&coverage.line());
    out.push('\n');
    out.push_str(&rollup);
    out.push('\n');
    out.push_str(&detail_txt);
    out.push_str(&compact);
    if coverage.detailed < coverage.files_total {
        let syms_omitted: usize = g
            .files
            .iter()
            .zip(in_detail.iter())
            .filter(|(_, d)| !**d)
            .map(|(f, _)| f.symbols)
            .sum();
        out.push_str(&elision_footer(
            coverage,
            g.symbols.len(),
            syms_omitted,
            max_chars,
        ));
    }
    out
}

/// Section-aware slice of a rendered map for an agent brief: the tier-0 header
/// (title, freshness, coverage, `# Top-level areas` rollup) is always kept
/// whole, whole `##` sections follow while they fit, and every dropped section
/// is counted in a footer pointing at the full map and the query CLI. Never a
/// blind first-N-chars cut.
#[must_use]
pub fn prompt_slice(map: &str, max_chars: usize) -> String {
    let first_section = map.find("\n## ").map_or(map.len(), |i| i + 1);
    let (head, rest) = map.split_at(first_section);
    let mut starts: Vec<usize> = vec![0];
    let mut from = 0;
    while let Some(i) = rest[from..].find("\n## ") {
        starts.push(from + i + 1);
        from += i + 1;
    }
    let sections: Vec<&str> = starts
        .iter()
        .enumerate()
        .map(|(k, &s)| {
            let end = starts.get(k + 1).copied().unwrap_or(rest.len());
            &rest[s..end]
        })
        .collect();

    let total: usize = sections.iter().map(|s| s.len()).sum();
    let will_drop = !sections.is_empty() && head.len() + total > max_chars;
    let limit = max_chars.saturating_sub(if will_drop { SLICE_FOOTER_RESERVE } else { 0 });

    let mut out = String::from(head);
    let mut kept = 0usize;
    for s in &sections {
        if out.len() + s.len() <= limit {
            out.push_str(s);
            kept += 1;
        }
    }
    if kept < sections.len() {
        use std::fmt::Write as _;
        let _ = write!(
            out,
            "\n… {} of {} map sections elided — full map: `.coxagent/REPO_MAP.md`; \
             query the code graph: `coxagent codegraph search|impact|callers <name>`.\n",
            sections.len() - kept,
            sections.len()
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(files: impl IntoIterator<Item = (String, usize)>) -> CodeGraph {
        let mut g = CodeGraph {
            built_at: "2026-09-02T00:00:00Z".to_owned(),
            ..CodeGraph::default()
        };
        for (path, n_syms) in files {
            g.files.push(FileNode {
                path: path.clone(),
                lang: "rust".to_owned(),
                loc: 10,
                symbols: n_syms,
                imports: Vec::new(),
            });
            for k in 0..n_syms {
                g.symbols.push(Symbol {
                    name: format!("s{k:02}"),
                    kind: "fn".to_owned(),
                    scope: None,
                    file: path.clone(),
                    line: k + 1,
                    lang: "rust".to_owned(),
                });
            }
        }
        g
    }

    /// Files with real per-file detail sections, i.e. not the rollup header,
    /// not a compact "paths only" line.
    fn detail_sections(map: &str) -> Vec<&str> {
        map.lines()
            .filter(|l| l.starts_with("## ") && !l.contains("paths only"))
            .collect()
    }

    fn coverage_of(map: &str) -> (usize, usize, usize) {
        let line = map
            .lines()
            .find(|l| l.starts_with("Coverage: "))
            .expect("coverage line present");
        let body = line.strip_prefix("Coverage: ").expect("prefix");
        let (counts, tiers) = body.split_once(" files (").expect("counts/tiers split");
        let (covered, total) = counts.split_once('/').expect("covered/total");
        let tiers = tiers.trim_end_matches(')');
        let (d, c) = tiers.split_once(" detailed · ").expect("detailed/compact");
        let d: usize = d.parse().expect("detailed number");
        let c: usize = c
            .trim_end_matches(" compact")
            .parse()
            .expect("compact number");
        (
            covered.parse().expect("covered number"),
            total.parse().expect("total number"),
            d + c,
        )
    }

    #[test]
    fn every_file_is_accounted_for_within_the_budget_even_when_detail_does_not_fit() {
        let paths: Vec<(String, usize)> = (0..40)
            .map(|i| (format!("aa/src/f{i:02}.rs"), 20))
            .chain((0..40).map(|i| (format!("zz/src/f{i:02}.rs"), 20)))
            .collect();
        let g = graph(paths);
        let full = render(&g, usize::MAX);
        let max = full.len() / 2;
        let map = render(&g, max);

        assert!(map.len() <= max, "rendered {} > budget {max}", map.len());
        let (covered, total, detailed_plus_compact) = coverage_of(&map);
        assert_eq!(total, 80, "every indexed file is counted");
        assert_eq!(covered, detailed_plus_compact, "coverage line is honest");
        assert!(covered < total, "detail really was cut in this scenario");

        // Both areas are named by the always-present rollup…
        assert!(map.contains("- aa — 40 files, 800 symbols"), "{map}");
        assert!(map.contains("- zz — 40 files, 800 symbols"), "{map}");
        // …and the footer accounts for the rest, pointing at the query CLI.
        assert!(
            map.contains(&format!("{} of 80 files omitted", total - covered)),
            "{map}"
        );
        assert!(map.contains("symbols not shown"), "{map}");
        assert!(
            map.contains("coxagent codegraph search|impact|callers"),
            "{map}"
        );
        // The map ENDS with the elision footer, not a silent stop.
        assert!(map.trim_end().ends_with("`."), "{map}");
    }

    #[test]
    fn a_huge_first_group_never_starves_later_groups() {
        // Regression vs the pre-F316 renderer, which walked files
        // lexicographically and stopped at the first budget overrun: everything
        // after a fat `aaa/…` silently vanished from the map.
        let paths: Vec<(String, usize)> = (0..60)
            .map(|i| (format!("aaa/f{i:02}.rs"), 6))
            .chain((0..3).map(|i| (format!("zzz/f{i}.rs"), 3)))
            .collect();
        let g = graph(paths);
        let map = render(&g, 2_000);

        assert!(
            map.contains("zzz/f0.rs") && map.contains("zzz/f1.rs") && map.contains("zzz/f2.rs"),
            "later group's files must still be visible: {map}"
        );
        assert!(map.contains("- aaa — 60 files"), "{map}");
        assert!(map.contains("- zzz — 3 files"), "{map}");
        assert!(map.contains("files omitted"), "{map}");
    }

    #[test]
    fn coverage_counts_match_what_is_actually_rendered() {
        let g = graph([
            ("a1.rs".to_owned(), 3),
            ("a2.rs".to_owned(), 3),
            ("z1.rs".to_owned(), 3),
            ("z2.rs".to_owned(), 3),
        ]);

        // Everything fits: all files detailed, no compact tier, no footer.
        let full = render(&g, 10_000);
        assert!(
            full.contains("Coverage: 4/4 files (4 detailed · 0 compact)"),
            "{full}"
        );
        assert_eq!(detail_sections(&full).len(), 4);
        assert!(!full.contains("paths only"));
        assert!(!full.contains("files omitted"));

        // Nothing fits: zero detail, everything omitted, footer counts it.
        let tiny = render(&g, 10);
        assert!(
            tiny.contains("Coverage: 0/4 files (0 detailed · 0 compact)"),
            "{tiny}"
        );
        assert_eq!(detail_sections(&tiny).len(), 0);
        assert!(
            tiny.contains("0 of 4 files listed as paths only, 4 of 4 files omitted"),
            "{tiny}"
        );

        // A middle budget stays honest: parsed coverage equals what rendered.
        let mid = render(&g, full.len() - 41);
        let (covered, total, _) = coverage_of(&mid);
        let rendered_detail = detail_sections(&mid).len();
        let rendered_paths: usize = mid
            .lines()
            .filter(|l| l.contains("paths only"))
            .filter_map(|l| l.split(" — ").nth(1))
            .filter_map(|r| r.split(" more files").next())
            .filter_map(|n| n.parse::<usize>().ok())
            .sum();
        assert_eq!(rendered_detail + rendered_paths, covered, "{mid}");
        assert!(covered <= total);
    }

    #[test]
    fn small_graph_under_budget_keeps_today_s_per_file_detail() {
        let g = CodeGraph {
            built_at: "2026-09-02T00:00:00Z".to_owned(),
            files: vec![FileNode {
                path: "src/main.rs".to_owned(),
                lang: "rust".to_owned(),
                loc: 5,
                symbols: 2,
                imports: Vec::new(),
            }],
            symbols: vec![
                Symbol {
                    name: "main".to_owned(),
                    kind: "fn".to_owned(),
                    scope: None,
                    file: "src/main.rs".to_owned(),
                    line: 1,
                    lang: "rust".to_owned(),
                },
                Symbol {
                    name: "S".to_owned(),
                    kind: "struct".to_owned(),
                    scope: None,
                    file: "src/main.rs".to_owned(),
                    line: 2,
                    lang: "rust".to_owned(),
                },
                Symbol {
                    name: "build".to_owned(),
                    kind: "fn".to_owned(),
                    scope: Some("App".to_owned()),
                    file: "src/main.rs".to_owned(),
                    line: 3,
                    lang: "rust".to_owned(),
                },
            ],
            ..CodeGraph::default()
        };
        let map = render(&g, 10_000);
        assert!(
            map.starts_with("# Repo map — 1 files, 3 symbols\n"),
            "{map}"
        );
        assert!(
            map.contains("## src/main.rs (rust)\n  fn main, struct S, fn App::build\n"),
            "per-file detail is byte-identical to the pre-F316 renderer: {map}"
        );
    }

    #[test]
    fn compact_pass_lists_undetailed_files_per_area_within_the_budget() {
        let g = graph([
            ("aa/f00.rs".to_owned(), 1),
            ("aa/f01.rs".to_owned(), 1),
            ("zz/f00.rs".to_owned(), 1),
        ]);
        let areas = areas_of(&g);
        // aa's files are detailed; zz's is not.
        let in_detail = vec![true, true, false];

        let mut spent = 10usize;
        let (compact, in_compact, compacted) = compact_pass(&g, &areas, &in_detail, 60, &mut spent);
        assert_eq!(compacted, 1);
        assert!(in_compact[2] && !in_compact[0] && !in_compact[1]);
        assert!(
            compact.contains("## zz — 1 more files, paths only: zz/f00.rs\n"),
            "{compact}"
        );
        assert_eq!(spent, 10 + compact.len(), "spend tracks what rendered");

        // A line that does not fit is skipped whole — never partial paths.
        let mut spent = 10usize;
        let (compact, _, compacted) = compact_pass(&g, &areas, &in_detail, 12, &mut spent);
        assert_eq!(compacted, 0);
        assert!(compact.is_empty());
        assert_eq!(spent, 10);
    }

    #[test]
    fn a_share_busting_file_is_still_detailed_when_the_whole_graph_fits() {
        // Regression: the compact pass used to claim files whose detail still
        // fit the budget, silently downgrading them to paths-only whenever one
        // file was bigger than its area's equal share.
        let g = graph([("aa/f00.rs".to_owned(), 10), ("bb/f00.rs".to_owned(), 1)]);
        let full = render(&g, usize::MAX);
        let map = render(&g, full.len() + 60);
        assert!(
            map.contains("Coverage: 2/2 files (2 detailed · 0 compact)"),
            "{map}"
        );
        assert!(
            map.contains(
                "## aa/f00.rs (rust)\n  fn s00, fn s01, fn s02, fn s03, fn s04, \
                 fn s05, fn s06, fn s07, fn s08, fn s09\n"
            ),
            "the file that busts its area's share keeps its full detail: {map}"
        );
        assert!(!map.contains("paths only"), "{map}");
        assert!(!map.contains("files omitted"), "{map}");
    }

    #[test]
    fn an_empty_graph_renders_an_honest_summary_without_claims_it_cannot_make() {
        let map = render(&CodeGraph::default(), 1_000);
        assert!(
            map.contains("Coverage: 0/0 files (0 detailed · 0 compact)"),
            "{map}"
        );
        assert!(
            !map.contains("Generated:"),
            "no built_at → no freshness claim: {map}"
        );
        assert!(!map.contains("files omitted"), "{map}");
    }

    #[test]
    fn prompt_slice_keeps_the_directory_summary_and_counts_elisions() {
        let paths: Vec<(String, usize)> = (0..20)
            .map(|i| (format!("aa/src/f{i:02}.rs"), 8))
            .chain((0..20).map(|i| (format!("zz/src/f{i:02}.rs"), 8)))
            .collect();
        let full = render(&graph(paths), usize::MAX);
        let slice = prompt_slice(&full, 1_200);

        let header_end = full.find("\n## ").expect("map has sections");
        assert!(
            slice.starts_with(&full[..header_end]),
            "tier-0 header kept whole"
        );
        assert!(slice.contains("# Top-level areas"));
        assert!(slice.contains("- aa — 20 files"), "{slice}");
        assert!(slice.contains("- zz — 20 files"), "{slice}");
        // Whole sections only — no mid-line cuts.
        for line in slice.lines().filter(|l| !l.starts_with('…')) {
            assert!(
                full.contains(line),
                "sliced line is not a whole map line: {line}"
            );
        }
        assert!(slice.contains("map sections elided"), "{slice}");
        assert!(slice.len() <= 1_200, "sliced {} > budget", slice.len());
    }

    #[test]
    fn prompt_slice_of_a_small_map_passes_through_unchanged() {
        let map = "# Repo map — 1 files, 1 symbols\nCoverage: 1/1 files (1 detailed · 0 compact)\n\
                   # Top-level areas\n- src — 1 files, 1 symbols\n\n\
                   ## src/lib.rs (rust)\n  fn a\n";
        assert_eq!(prompt_slice(map, 3_000), map);
    }

    #[tokio::test]
    async fn index_records_head_sha_and_the_saved_map_is_fresh_and_covered() {
        let dir = std::env::temp_dir().join(format!("repomap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git/refs/heads")).expect("mk");
        std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/main\n").expect("head");
        std::fs::write(
            dir.join(".git/refs/heads/main"),
            "6af0bac6deadbeef6af0bac6deadbeef6af0bac6\n",
        )
        .expect("ref");
        std::fs::create_dir_all(dir.join("src")).expect("src");
        std::fs::write(dir.join("src/lib.rs"), "pub fn a() {}\n").expect("src");
        std::fs::create_dir_all(dir.join("web")).expect("web");
        std::fs::write(dir.join("web/app.js"), "function boot(){}\n").expect("js");

        let g = CodeGraph::index(&crate::test_fs::StdFsFiles, &dir).await;
        assert_eq!(
            g.head_sha.as_deref(),
            Some("6af0bac6deadbeef6af0bac6deadbeef6af0bac6")
        );
        let map = g.repo_map(40_000);
        assert!(map.contains("Generated: "), "{map}");
        assert!(map.contains("HEAD: 6af0bac6deadbeef"), "{map}");
        // Every top-level area is named by the rollup (mini AC1).
        assert!(map.contains("- src — 1 files"), "{map}");
        assert!(map.contains("- web — 1 files"), "{map}");

        g.save(&crate::test_fs::StdFsFiles, &dir)
            .await
            .expect("save");
        let saved = std::fs::read_to_string(dir.join(".coxagent/REPO_MAP.md")).expect("map");
        assert!(
            saved.contains("Coverage: 2/2 files (2 detailed · 0 compact)"),
            "{saved}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn detached_head_sha_is_read_directly() {
        let dir = std::env::temp_dir().join(format!("repomap-dh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).expect("mk");
        std::fs::create_dir_all(dir.join("src")).expect("mk");
        std::fs::write(
            dir.join(".git/HEAD"),
            "77a6347bdeadbeef77a6347bdeadbeef77a6347b\n",
        )
        .expect("head");
        std::fs::write(dir.join("src/a.rs"), "pub fn a() {}\n").expect("src");

        let g = CodeGraph::index(&crate::test_fs::StdFsFiles, &dir).await;
        assert_eq!(
            g.head_sha.as_deref(),
            Some("77a6347bdeadbeef77a6347bdeadbeef77a6347b")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn garbage_head_is_never_mistaken_for_a_sha() {
        let dir = std::env::temp_dir().join(format!("repomap-junk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).expect("mk");
        std::fs::create_dir_all(dir.join("src")).expect("mk");
        std::fs::write(dir.join(".git/HEAD"), "not a sha at all\n").expect("head");
        std::fs::write(dir.join("src/a.rs"), "pub fn a() {}\n").expect("src");

        let g = CodeGraph::index(&crate::test_fs::StdFsFiles, &dir).await;
        assert_eq!(g.head_sha, None, "junk in .git/HEAD must not reach the map");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn missing_git_metadata_just_omits_the_sha() {
        let dir = std::env::temp_dir().join(format!("repomap-nogit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("mk");
        std::fs::write(dir.join("src/a.rs"), "pub fn a() {}\n").expect("src");

        let g = CodeGraph::index(&crate::test_fs::StdFsFiles, &dir).await;
        assert_eq!(g.head_sha, None);
        let map = g.repo_map(40_000);
        assert!(map.contains("Generated: "), "{map}");
        assert!(!map.contains("HEAD:"), "{map}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
