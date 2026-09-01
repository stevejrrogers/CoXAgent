//! CXA-F022 regression tests — the four correctness contracts gating F001.
//!
//! Each bug's root cause is fixed at source in [`prompts_resolve`] (not masked):
//! every one of these FAILS against pre-fix behaviour and passes here.
//!
//!   * g1 missing override file  => embedded default wins
//!   * g2 stray / non-role key   => rejected, never injected
//!   * g3 blank/whitespace-only  => treated as absent (embedded wins)
//!   * g4 BASE + ENGINEERING_STANDARDS survive when only a ROLE body is replaced
use coxagent_application::ports::outbound::{FileMeta, WorkspaceFilesPort};
use coxagent_application::{prompts, prompts_resolve};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const PROJ: &str = "/proj";

/// In-memory files port double: `files[path]` is that file's content.
struct MemFiles(HashMap<PathBuf, String>);

impl MemFiles {
    fn new() -> Self {
        Self(HashMap::new())
    }
    fn put(&mut self, rel: &str, content: &str) {
        self.0.insert(PathBuf::from(rel), content.to_string());
    }
}

#[async_trait::async_trait]
impl WorkspaceFilesPort for MemFiles {
    async fn read(&self, path: &Path) -> Option<String> {
        self.0.get(path).cloned()
    }
    async fn read_bytes(&self, path: &Path) -> Option<Vec<u8>> {
        self.0.get(path).map(String::as_bytes).map(<[u8]>::to_vec)
    }
    async fn write(&self, _p: &Path, _c: &str) -> bool {
        false
    }
    async fn write_bytes(&self, _p: &Path, _b: &[u8]) -> bool {
        false
    }
    async fn delete(&self, _p: &Path) -> bool {
        false
    }
    async fn stat(&self, path: &Path) -> Option<FileMeta> {
        self.0.contains_key(path).then(|| FileMeta {
            path: path.to_path_buf(),
            modified_epoch: 0,
            size: 0,
        })
    }
    async fn list_recursive(&self, dir: &Path) -> Vec<PathBuf> {
        self.0
            .keys()
            .filter(|p| p.starts_with(dir))
            .cloned()
            .collect()
    }
    async fn list_dirs(&self, _dir: &Path) -> Vec<PathBuf> {
        Vec::new()
    }
    async fn list(&self, dir: &Path) -> Vec<FileMeta> {
        self.0
            .iter()
            .filter(|(p, _)| p.parent() == Some(dir))
            .map(|(p, c)| FileMeta {
                path: p.clone(),
                modified_epoch: 0,
                size: c.len() as u64,
            })
            .collect()
    }
}

// ---- g1 / g3 at source (pure precedence decision) ----

#[test]
fn g1_missing_file_falls_back_to_embedded_default() {
    let body = prompts_resolve::resolve_role_body("EMBEDDED", None);
    assert_eq!(body.as_ref(), "EMBEDDED");
}

#[test]
fn g3_blank_or_whitespace_only_counts_as_absent_not_empty() {
    assert_eq!(
        prompts_resolve::resolve_role_body("E", Some("   \n\t ")).as_ref(),
        "E"
    );
    assert_eq!(
        prompts_resolve::resolve_role_body("E", Some("")).as_ref(),
        "E"
    );
}

#[test]
fn nonblank_override_wins_and_is_trimmed_of_surrounding_space() {
    let got = prompts_resolve::resolve_role_body("EMBEDDED", Some("  MY OVERRIDE\n"));
    assert_eq!(got.as_ref(), "MY OVERRIDE");
}

// ---- loader behind the files port ----

#[tokio::test]
async fn g1_missing_file_on_disk_falls_back_to_embedded_default() {
    let fs = MemFiles::new();
    assert!(
        prompts_resolve::load_role_override(Some(&fs), Path::new(PROJ), "prompts", "ba")
            .await
            .is_none(),
        "no prompts/ba.md => no override"
    );
}

#[tokio::test]
async fn g2_stray_nonrole_file_is_rejected_not_injected() {
    // An unknown role key must never be loaded even if a matching file exists:
    // a stray / typo'd key is rejected outright, never injected into a prompt.
    let mut fs = MemFiles::new();
    fs.put("/proj/prompts/README.md", "SHOULD NOT INJECT");
    assert!(
        prompts_resolve::load_role_override(Some(&fs), Path::new(PROJ), "prompts", "README")
            .await
            .is_none(),
        "stray non-role file is rejected"
    );
}

#[tokio::test]
async fn g3_blank_file_on_disk_counts_as_absent_not_empty() {
    let mut fs = MemFiles::new();
    fs.put("/proj/prompts/ba.md", "   \n\t ");
    assert!(
        prompts_resolve::load_role_override(Some(&fs), Path::new(PROJ), "prompts", "ba")
            .await
            .is_none(),
        "whitespace-only body => absent, embedded wins"
    );
}

// ---- compose invariants survive when only a ROLE body is replaced ----

#[tokio::test]
async fn g4_base_and_engineering_invariants_survive_even_when_role_replaced() {
    for role in ["ba", "sa", "dev", "test", "docs"] {
        let mut fs = MemFiles::new();
        fs.put(
            &format!("/proj/prompts/{role}.md"),
            &format!("CUSTOM {role} BODY"),
        );
        let sp =
            prompts_resolve::compose_role_system(Some(&fs), Path::new(PROJ), "prompts", role).await;
        // F001 invariant: BASE first, then ENGINEERING_STANDARDS.
        assert!(sp.starts_with(prompts::BASE), "{role}: opens with BASE");
        let after_base = &sp[prompts::BASE.len()..];
        assert!(
            after_base.starts_with("\n\n") && after_base.contains(prompts::ENGINEERING_STANDARDS),
            "{role}: BASE followed by ENGINEERING_STANDARDS"
        );
        // The override replaced only its own role section (not the base rules).
        assert!(
            sp.contains(&format!("CUSTOM {role} BODY")),
            "{role}: override used"
        );
    }
}
