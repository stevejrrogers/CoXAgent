//! Doc-leak detector (CXA-B315): credential-shaped strings must never reappear
//! in documentation or non-code config once CXA-B311a scrubbed the B031 doc.
//!
//! Split per the house rules:
//!
//! - [`scan_doc`] is a PURE decision function `(&str, &str) -> Vec<…>`: given a
//!   repo-relative path and its contents it returns findings. No IO of any kind
//!   happens here — the hexagonal ratchet holds by construction, not by review.
//! - The repo walk that feeds it lives in the guard adapter
//!   (`crates/app/tests/doc_leak_guard.rs`), behind the boundary.
//!
//! Detector families (kept deliberately coarse — false negatives cost more than
//! false positives in a post-scrub tree, and every rule names itself so a hit
//! tells the author WHY):
//!
//! 1. known provider key prefixes (`AKIA…`, `sk-…`, `ghp_…`, `github_pat_…`,
//!    `xoxb-…`);
//! 2. assignment lines (`password=` / `token:` …) whose value is a long literal
//!    mixing character classes;
//! 3. credentials embedded in connection URLs (`postgres://user:pass@…`) —
//!    the exact shape CXA-B031 leaked and SEC-41E384 re-flagged;
//! 4. unencrypted PEM private-key blocks.
//!
//! Documentation shorthand is not a leak: placeholder vocabulary (`example`,
//! `change-me`, `<your-token>`, env interpolation `${VAR}`, bullet runs) and
//! CI-fixture hosts pass. A value must *look* typeable-and-secret-shaped to be
//! reported, and reported snippets are masked so the guard output cannot
//! itself become the next leak.

/// One credential-shaped hit: enough for an author to find and judge the line
/// without the finding reprinting the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocLeakFinding {
    /// Repo-relative path of the scanned file (as given to [`scan_doc`]).
    pub path: String,
    /// 1-based line number within the file.
    pub line: usize,
    /// Which detector fired — the WHY of the failure message.
    pub rule: &'static str,
    /// The match with its middle masked (`ghp_a…9f`), safe to print.
    pub snippet: String,
}

/// File extensions the guard scans: documentation and non-code configuration.
/// Code files own their own review lanes (`security_sweep`, `deps_scan`); this
/// guard exists because docs/config have none.
#[must_use]
pub fn is_scanned_path(path: &str) -> bool {
    const EXTENSIONS: [&str; 12] = [
        ".md",
        ".markdown",
        ".txt",
        ".rst",
        ".adoc",
        ".yml",
        ".yaml",
        ".toml",
        ".cfg",
        ".conf",
        ".ini",
        ".example",
    ];
    let lower = path.to_ascii_lowercase();
    EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
        || lower.ends_with(".env.example")
        || lower.ends_with("dockerfile")
}

/// Value vocabulary that marks a string as documentation shorthand rather than
/// a live secret. Mirrors the convention CXA-B030 left behind (`change-me-…`)
/// and the reserved words the security sweep already honours.
const PLACEHOLDER_WORDS: [&str; 14] = [
    "pass",
    "password",
    "passwd",
    "secret",
    "token",
    "key",
    "example",
    "changeme",
    "change-me",
    "changleme",
    "placeholder",
    "redacted",
    "test",
    "dummy",
];

fn is_placeholder_value(value: &str) -> bool {
    let v = value.trim();
    // Shell/compose interpolation delegates the secret to the environment —
    // nothing literal to leak (SEC-41E384 rule).
    if v.starts_with('$') || v.contains("${") {
        return true;
    }
    // Angle-bracket fill-in slots and masked/bullet renderings.
    if v.starts_with('<')
        || v.starts_with('[')
        || v.starts_with('…')
        || v.chars()
            .all(|c| matches!(c, '•' | '*' | '_' | '-' | '.' | '…'))
    {
        return true;
    }
    let bare = v.trim_matches(|c: char| c == '"' || c == '\'' || matches!(c, ',' | ';' | ')'));
    let lowered = bare.to_ascii_lowercase();
    PLACEHOLDER_WORDS
        .iter()
        .any(|word| lowered == *word || lowered.starts_with(&format!("{word}-")))
}

/// Character classes present in a value; a real secret mixes several.
fn char_classes(value: &str) -> usize {
    const CLASSES: [fn(char) -> bool; 4] = [
        |c: char| c.is_ascii_lowercase(),
        |c: char| c.is_ascii_uppercase(),
        |c: char| c.is_ascii_digit(),
        |c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '/' || c == '-' || c == '@'),
    ];
    CLASSES
        .into_iter()
        .filter(|class| value.chars().any(class))
        .count()
}

/// Mask the middle of a snippet so printing a finding cannot republish the
/// secret into logs, screenshots or this repo's own evidence docs.
fn masked(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 10 {
        return format!("{}…", "*".repeat(chars.len()));
    }
    let head: String = chars[..5].iter().collect();
    let tail: String = chars[chars.len() - 3..].iter().collect();
    format!("{head}…{tail}")
}

fn finding(path: &str, line_no: usize, rule: &'static str, raw: &str) -> DocLeakFinding {
    DocLeakFinding {
        path: path.to_owned(),
        line: line_no,
        rule,
        snippet: masked(raw),
    }
}

/// Known provider token shapes. Long-enough tails that prose mentioning the
/// prefix ("use `sk-` keys") can never trip it.
type ProviderFinding = (&'static str, String);
type ProviderRule = (&'static str, &'static str, fn(&str) -> bool);

fn provider_prefix_finding(line: &str) -> Option<ProviderFinding> {
    const RULES: [ProviderRule; 5] = [
        (
            "AWS access key id (AKIA…) in doc/config",
            "AKIA",
            |rest: &str| {
                rest.len() >= 16
                    && rest
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            },
        ),
        (
            "OpenAI-style API key (sk-…) in doc/config",
            "sk-",
            |rest: &str| rest.len() >= 20 && rest.chars().any(|c: char| c.is_ascii_alphanumeric()),
        ),
        (
            "GitHub personal access token (gh?_…) in doc/config",
            "ghp_",
            |rest: &str| rest.len() >= 36 && rest.chars().all(|c| c.is_ascii_alphanumeric()),
        ),
        (
            "GitHub fine-grained PAT (github_pat_…) in doc/config",
            "github_pat_",
            |rest: &str| rest.len() >= 22 && rest.chars().any(|c: char| c.is_ascii_alphanumeric()),
        ),
        (
            "Slack bot token (xox?-…) in doc/config",
            "xox",
            |rest: &str| {
                matches!(&rest[..1], "b" | "p" | "a" | "r" | "s")
                    && rest.starts_with('-')
                    && rest.len() >= 12
            },
        ),
    ];
    for (rule, prefix, accept) in RULES {
        let mut from = 0;
        while let Some(at) = line[from..].find(prefix) {
            let start = from + at;
            let rest = &line[start + prefix.len()..];
            let tail: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            let matched = format!("{prefix}{tail}");
            if accept(&tail) {
                return Some((rule, matched));
            }
            from = start + prefix.len();
        }
    }
    None
}

/// Assignment-line detector: `password=hunter2DoNotCommit` and friends. The key
/// saying what it is carries most of the signal; length plus character-class
/// spread keeps prose assignments (`token=authentication flow`) out.
///
/// Evaluated per shell-style token (`KEY=VALUE`), not over the raw line, so a
/// documentation sentence quoting several short example assignments
/// (`PG_PASSWORD=ci-smoke FOO=bar`) reads as its actual parts instead of one
/// inflated pseudo-value, and interpolated keys (`${VAR}`) never match.
fn assignment_finding(line: &str) -> Option<(&'static str, String)> {
    const KEYS: [&str; 7] = [
        "password", "passwd", "secret", "token", "api_key", "api-key", "apikey",
    ];
    for token in line.split_whitespace() {
        let Some((key, value)) = token.split_once(['=', ':']) else {
            continue;
        };
        let key = key.trim().trim_matches(['"', '\'', '`', '-']);
        if key.starts_with('$') || key.contains('{') || key.is_empty() {
            continue;
        }
        let key_lowered = key.to_ascii_lowercase();
        if !KEYS.iter().any(|k| key_lowed_ends_with(&key_lowered, k)) {
            continue;
        }
        let value = value.trim().trim_matches(['"', '\'']).trim_end_matches(',');
        let inline_comment = value.find("# ").map_or(value, |at| &value[..at]);
        let candidate = inline_comment.trim();
        if candidate.chars().count() < 18
            || char_classes(candidate) < 3
            || is_placeholder_value(candidate)
        {
            continue;
        }
        return Some((
            "credential assigned as a long literal in doc/config",
            candidate.to_owned(),
        ));
    }
    None
}

fn key_lowed_ends_with(key: &str, want: &str) -> bool {
    key == want
        || key
            .strip_suffix(want)
            .is_some_and(|rest| rest.ends_with('_') || rest.ends_with('-') || rest.is_empty())
}

/// Connection-URL detector — the shape both CXA-B031 and SEC-41E384 leaked.
/// A URL password must be *live*: longer than a doc word, not interpolation,
/// not the reserved shorthand vocabulary.
fn url_credential_finding(line: &str) -> Option<(&'static str, String)> {
    const SCHEMES: [&str; 6] = [
        "postgres://",
        "postgresql://",
        "mysql://",
        "redis://",
        "mongodb://",
        "mongodb+srv://",
    ];
    for scheme in SCHEMES {
        let Some(rest) = line.find(scheme).map(|at| &line[at + scheme.len()..]) else {
            continue;
        };
        let Some(at_sign) = rest.find('@') else {
            continue;
        };
        let userinfo = &rest[..at_sign];
        if userinfo.contains('/') || userinfo.contains(' ') {
            continue;
        }
        let password = userinfo.split_once(':').map_or(userinfo, |(_, pass)| pass);
        if password.chars().count() < 6 || is_placeholder_value(password) {
            continue;
        }
        return Some((
            "credentials embedded in a connection URL in doc/config",
            format!("{scheme}{userinfo}@"),
        ));
    }
    None
}

/// Private-key material has no placeholder ambiguity worth modelling: an
/// unencrypted PEM block in a doc means somebody pasted the real thing.
fn private_key_finding(line: &str) -> Option<(&'static str, String)> {
    let t = line.trim();
    t.starts_with("-----BEGIN ")
        .then(|| ("unencrypted private key block in doc/config", t.to_owned()))
        .filter(|(_, marker)| marker.contains("PRIVATE KEY"))
}

/// Scan one file's contents. Pure: same inputs, same findings, no IO.
///
/// Lines marked with a trailing `<!-- doc-leak:allow -->` (or `# doc-leak:allow`
/// in config dialects) are skipped, so a legitimately-required literal can name
/// itself instead of forcing a whole-file exclusion.
#[must_use]
pub fn scan_doc(path: &str, contents: &str) -> Vec<DocLeakFinding> {
    if !is_scanned_path(path) {
        return Vec::new();
    }
    let mut findings = Vec::new();
    for (index, line) in contents.lines().enumerate() {
        let line_no = index + 1;
        if line.contains("doc-leak:allow") {
            continue;
        }
        if let Some((rule, raw)) = provider_prefix_finding(line)
            .or_else(|| private_key_finding(line))
            .or_else(|| url_credential_finding(line))
            .or_else(|| assignment_finding(line))
        {
            findings.push(finding(path, line_no, rule, &raw));
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    const B031_SCRUBBED_SNIPPET: &str = "\
### Deploy secret rotation

There is no user-facing command — migration runs automatically inside every
app-driven deploy. To observe it manually:

```sh
export COXAGENT_DB_DSN=\"postgres://${COXAGENT_PG_USER:?set a Postgres role}:${COXAGENT_PG_PASSWORD:?set a Postgres password, never inline it here}@${COXAGENT_DB_HOST:-localhost}:5432/coxagent\"
PG_PASSWORD=<stored per-project, see the deploy-secrets store>
```
";

    #[test]
    fn the_scrubbed_b031_doc_form_produces_zero_findings() {
        assert!(
            scan_doc("wiki/engineering/deploy-secrets.md", B031_SCRUBBED_SNIPPET).is_empty(),
            "the sanitized doc shape must stay clean — placeholder forms are not leaks"
        );
    }

    #[test]
    fn reintroducing_the_old_literal_dsn_shape_is_caught_with_file_and_line() {
        let leaked = format!(
            "{B031_SCRUBBED_SNIPPET}\
             export COXAGENT_DB_DSN='postgres://cox:sK7vQm2xR9dLp4WtZ8nJhF3y@db.internal:5432/coxagent'\n"
        );
        let hits = scan_doc("docs/wiki/engineering/deployment/postgres-dsn.md", &leaked);
        assert_eq!(
            hits.len(),
            1,
            "one planted URL credential, exactly one finding"
        );
        assert_eq!(hits[0].line, B031_SCRUBBED_SNIPPET.lines().count() + 1);
        assert!(
            hits[0].rule.contains("connection URL"),
            "failure must say WHY: {}",
            hits[0].rule
        );
        assert!(
            !hits[0].snippet.contains("sK7vQm2xR9dLp4WtZ8nJhF3y"),
            "snippets are masked"
        );
    }

    #[test]
    fn provider_prefixed_keys_are_flagged_wherever_they_appear() {
        let doc = "- aws: AKIAIOSFODNN7EXAMPLE\n- openai: sk-proj4fT8kQ2wEr9tYu1iOp0aSdFgHjKl234567890\n- github: ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJKLMNPQ\n";
        for line in doc.lines() {
            let hits = scan_doc("notes/providers.txt", &format!("{line}\n"));
            let names = hits
                .iter()
                .map(|h| h.rule.to_owned())
                .collect::<Vec<_>>()
                .join("; ");
            assert_eq!(
                hits.len(),
                1,
                "expected one hit for {line}, got {}: {}",
                hits.len(),
                names
            );
        }
        let hits = scan_doc("notes/providers.txt", doc);
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].line, 1);
        assert!(hits[0].rule.contains("AWS"), "{}", hits[0].rule);
    }

    #[test]
    fn long_literal_assignments_to_password_keys_are_flagged_but_shorthand_is_not() {
        let leak = "password=Wq9zXe2Rt5Yu8Io1Pp3As\n";
        let hits = scan_doc("config/settings.conf", leak);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].rule.contains("long literal"));

        let shorthand = "password=change-me-minio-secret # repo placeholder convention\n";
        assert!(scan_doc("deploy/.env.example", shorthand).is_empty());
    }

    #[test]
    fn code_files_and_interpolation_never_reach_the_detector() {
        assert!(scan_doc("src/main.rs", "let password=\"hunter2DoNotCommit\";").is_empty());
        assert!(scan_doc(
            "README.md",
            "export DB_DSN=\"postgres://${PG_USER:?set}:${PG_PASSWORD:?never inline}@db:5432/x\"\n"
        )
        .is_empty());
    }

    #[test]
    fn inline_allow_marker_exempts_one_line_only() {
        let doc = "token=Wq9zXe2Rt5Yu8Io1Pp3As # doc-leak:allow\n";
        assert!(scan_doc("notes/tokens.md", doc).is_empty());
    }
}
