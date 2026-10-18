//! Per-project override resolution for role system prompts (F001 seam).
//!
//! The embedded defaults ([crate::prompts]) are always the baseline; this
//! module adds an OPTIONAL per-project override milestone on top of them,
//! without changing what [crate::prompts::system_prompt] emits today.
//!
//! Layering: resolution is a PURE function over data somebody else already
//! read. The only IO here goes through [`WorkspaceFilesPort`] — never
//! `std::fs`. The decision ("which body wins") lives entirely in
//! [`resolve_role_body`], so it stays testable with an in-memory double and no
//! disk.
use crate::ports::outbound::WorkspaceFilesPort;
use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// Canonical override sub-directory relative to the project root when the
/// caller supplies none.
pub const DEFAULT_PROMPTS_SUBDIR: &str = "prompts";

/// Role keys an override may be loaded for. Anything outside this set is NOT a
/// valid per-role body — it must be rejected rather than injected into a system
/// prompt (`g2_stray_nonrole_file_is_rejected_not_injected`). An explicit list
/// also stops typos from silently loading nothing.
pub const KNOWN_ROLE_KEYS: &[&str] = &[
    "po", "sm", "ba", "sa", "pd", "dev", "dev-heal", "test", "docs",
];

#[inline]
#[must_use]
pub fn is_valid_role_key(key: &str) -> bool {
    KNOWN_ROLE_KEYS.contains(&key)
}

fn file_for(root: &Path, prompts_subdir: &str, role_key: &str) -> PathBuf {
    root.join(prompts_subdir).join(format!("{role_key}.md"))
}

fn embedded_for(role_key: &str) -> Option<&'static str> {
    use crate::prompts::{BA, DEV, DOCS, PD, SA, TEST};
    match role_key {
        "ba" => Some(BA),
        "sa" => Some(SA),
        "pd" => Some(PD),
        "dev" | "dev-heal" => Some(DEV),
        "test" => Some(TEST),
        "docs" => Some(DOCS),
        _ => None,
    }
}

/// Load an optional per-project override body for one role through the files
/// port — no disk access here.
#[must_use]
pub async fn load_role_override(
    files: Option<&dyn WorkspaceFilesPort>,
    work_dir: &Path,
    prompts_subdir: &str,
    role_key: &str,
) -> Option<String> {
    let files = files?;
    if !is_valid_role_key(role_key) {
        return None;
    }
    let path = file_for(work_dir, prompts_subdir, role_key);
    let raw = files.read(&path).await?;
    if raw.trim().is_empty() {
        // Blank counts as ABSENT (`g3`: blank == absent), so the embedded body
        // wins rather than an empty section leaking into every prompt.
        return None;
    }
    Some(raw)
}

/// Pure precedence decision between the embedded default and a loaded override.
#[must_use]
pub fn resolve_role_body<'a>(
    embedded_body: &'a str,
    loaded_override: Option<&'a str>,
) -> Cow<'a, str> {
    match loaded_override.map(str::trim) {
        Some(nonblank) if !nonblank.is_empty() => Cow::Owned(nonblank.to_string()),
        _ => Cow::Borrowed(embedded_body),
    }
}

/// Compose one role's full system prompt from BASE + ENGINEERING_STANDARDS +
/// resolved role body (`g4`: those two invariants survive even when only the
/// ROLE body is replaced).
#[must_use]
pub async fn compose_role_system(
    files: Option<&dyn WorkspaceFilesPort>,
    work_dir: &Path,
    prompts_subdir: &str,
    role_key: &str,
) -> String {
    let embedded = embedded_for(role_key).unwrap_or_default();
    let loaded = load_role_override(files, work_dir, prompts_subdir, role_key).await;
    let body = resolve_role_body(embedded, loaded.as_deref());
    format!(
        "{}\n\n{}\n\n{body}",
        crate::prompts::BASE,
        crate::prompts::ENGINEERING_STANDARDS
    )
}
