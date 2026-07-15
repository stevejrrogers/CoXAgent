//! Token-saver — the CoXAgent-native take on rtk (compress noisy text before it
//! reaches the model) and caveman (ask the agent for terse output). Cutting the
//! size of big prompt embeds (diffs, logs, file dumps) and the verbosity of
//! replies directly reduces engine spend. All pure functions — cheap and tested.

/// A short directive appended to an agent's task when the token-saver is on,
/// nudging concise output without sacrificing correctness (caveman-style).
pub const TERSE: &str = "\n\nBe concise: no preamble, no restating the task, no \
    filler. Lead with the answer. Keep code, commands, file paths, and error text \
    exact and unabridged.";

/// Collapse runs of identical lines into `line  (×N)` and drop trailing
/// whitespace — the biggest, cheapest win on repetitive logs/output (rtk-style).
#[must_use]
pub fn dedupe_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev: Option<&str> = None;
    let mut run = 0u32;
    let flush = |out: &mut String, line: &str, run: u32| {
        if run == 0 {
            return;
        }
        out.push_str(line.trim_end());
        if run > 1 {
            out.push_str("  (×");
            out.push_str(&run.to_string());
            out.push(')');
        }
        out.push('\n');
    };
    for line in text.lines() {
        match prev {
            Some(p) if p == line => run += 1,
            _ => {
                if let Some(p) = prev {
                    flush(&mut out, p, run);
                }
                prev = Some(line);
                run = 1;
            }
        }
    }
    if let Some(p) = prev {
        flush(&mut out, p, run);
    }
    out
}

/// Keep a text within `max_chars` by preserving the head and tail (where the
/// signal usually lives) and marking the elided middle. Returns the input
/// unchanged when it already fits.
#[must_use]
pub fn clip_middle(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars || max_chars < 200 {
        return text.chars().take(max_chars.max(text.len())).collect();
    }
    let head = max_chars * 3 / 4;
    let tail = max_chars - head - 40;
    let h: String = text.chars().take(head).collect();
    let t: String = {
        let chars: Vec<char> = text.chars().collect();
        chars[chars.len().saturating_sub(tail)..].iter().collect()
    };
    format!("{h}\n… [{} chars elided] …\n{t}", text.len() - head - tail)
}

/// Compress a unified diff for review: drop the noisy `index`/`@@`-adjacent
/// metadata lines that carry little review signal, dedupe, then clip to budget.
#[must_use]
pub fn compress_diff(diff: &str, max_chars: usize) -> String {
    let filtered: String = diff
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            // Drop git object hashes and "\ No newline" markers — pure noise.
            !(t.starts_with("index ") || t.starts_with("\\ No newline"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    clip_middle(&dedupe_lines(&filtered), max_chars)
}

/// General-purpose compression for logs/output/context embeds.
#[must_use]
pub fn compress(text: &str, max_chars: usize) -> String {
    clip_middle(&dedupe_lines(text), max_chars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupe_collapses_repeats() {
        let got = dedupe_lines("a\nb\nb\nb\nc\n");
        assert_eq!(got, "a\nb  (×3)\nc\n");
    }

    #[test]
    fn dedupe_keeps_distinct_lines() {
        assert_eq!(dedupe_lines("x\ny\nz"), "x\ny\nz\n");
    }

    #[test]
    fn clip_middle_preserves_head_and_tail() {
        let s = "HEAD".to_owned() + &"m".repeat(2000) + "TAIL";
        let got = clip_middle(&s, 500);
        assert!(got.starts_with("HEAD"));
        assert!(got.ends_with("TAIL"));
        assert!(got.contains("elided"));
        assert!(got.len() < s.len());
    }

    #[test]
    fn clip_middle_noop_when_small() {
        assert_eq!(clip_middle("short", 500), "short");
    }

    #[test]
    fn compress_diff_drops_index_lines_and_dedupes() {
        let diff =
            "diff --git a/x b/x\nindex 111..222 100644\n+ same\n+ same\n\\ No newline at end";
        let got = compress_diff(diff, 10_000);
        assert!(!got.contains("index 111"));
        assert!(!got.contains("No newline"));
        assert!(got.contains("+ same  (×2)"));
    }
}
