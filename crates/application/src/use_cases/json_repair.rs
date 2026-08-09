//! One cheap salvage pass for unparseable agent JSON: instead of throwing away
//! an expensive BA/SA/PD call because the model wrapped its JSON in prose or
//! broke a bracket, ask the engine once to extract/repair the JSON — a tiny
//! call that rescues a big one. Callers fall back to the original parse error
//! when even the repair fails.

use crate::ports::outbound::{AgentEnginePort, AgentRequest};
use coxagent_domain::Role;
use std::path::Path;
use std::time::Duration;

/// Ask the engine to turn `raw` into valid JSON matching `schema_hint`.
/// Returns the repaired text (still to be parsed by the caller), or `None`.
pub async fn repair_json<E: AgentEnginePort + ?Sized>(
    engine: &E,
    raw: &str,
    schema_hint: &str,
    work_dir: &Path,
) -> Option<String> {
    // Nothing to repair: an empty/whitespace-only blob (the classic timeout
    // case) would only burn a 120s SM call that asks the engine to fix nothing.
    if raw.trim().is_empty() || raw.chars().filter(|c| !c.is_whitespace()).count() == 0 {
        return None;
    }
    let capped: String = raw.chars().take(8000).collect();
    let request = AgentRequest {
        role: Role::Sm, // routed to the cheap ceremony model when configured
        system_prompt: "You repair malformed JSON. Output ONLY the corrected JSON — no prose, \
                        no code fences."
            .to_owned(),
        task_prompt: format!(
            "The following output was supposed to be {schema_hint}, but it does not parse. \
             Extract and repair it into VALID JSON with exactly that shape, preserving the \
             content. Output ONLY the JSON.\n\nINPUT:\n{capped}"
        ),
        work_dir: work_dir.to_path_buf(),
        timeout: Duration::from_secs(120),
        escalation_level: 0,
        label: None,
    };
    let out = engine.run(request).await.ok()?;
    if !out.succeeded() {
        return None;
    }
    Some(out.stdout)
}
