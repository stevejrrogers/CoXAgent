//! Shared single-call ceremony runner.
//!
//! A Scrum ceremony (standup, planning, grooming, discussion) used to spend one
//! engine call *per role turn* — a standup alone fanned out to ~12 separate
//! `claude` invocations, each re-sending the whole system prompt and context.
//! That is the dominant token cost of running the team continuously.
//!
//! This module collapses a ceremony into **one** engine call: the model role-
//! plays the entire ceremony as a short transcript (`ROLE: line`), which we then
//! parse back into per-speaker turns and post to the feed exactly as before. The
//! feed reads identically to the fan-out version, at ~1/N the token cost.

use crate::config::Language;
use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest};
use coxagent_domain::Role;
use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

/// One parsed line of a ceremony transcript.
pub struct CeremonyTurn {
    pub speaker: String,
    pub text: String,
}

/// Run a whole facilitated ceremony in a single engine call and return the
/// parsed per-speaker turns, in order.
///
/// `framing` describes the ceremony and the speaking style; `roster` is the set
/// of allowed speakers (label + persona) the model may use; `task` is the
/// grounded context plus the facilitation instructions. Only lines whose speaker
/// is in `roster` (plus the facilitator `SM`) are kept, so stray model prose
/// never leaks into the feed as a bogus author.
///
/// # Errors
/// [`AppError`] when the engine fails.
pub async fn run_transcript<E: AgentEnginePort + ?Sized>(
    engine: &E,
    work_dir: &Path,
    lang: Language,
    framing: &str,
    roster: &[(&str, &str)],
    task: &str,
) -> Result<Vec<CeremonyTurn>, AppError> {
    Ok(
        run_transcript_raw(engine, work_dir, lang, framing, roster, task)
            .await?
            .0,
    )
}

/// Like [`run_transcript`] but also returns the raw engine stdout, for callers
/// (e.g. discussion) that need trailing markers the transcript parser drops.
///
/// # Errors
/// [`AppError`] when the engine fails.
pub async fn run_transcript_raw<E: AgentEnginePort + ?Sized>(
    engine: &E,
    work_dir: &Path,
    lang: Language,
    framing: &str,
    roster: &[(&str, &str)],
    task: &str,
) -> Result<(Vec<CeremonyTurn>, String), AppError> {
    let mut roles_line = String::new();
    for (label, persona) in roster {
        let _ = write!(roles_line, "\n- {label}: {persona}");
    }
    let system_prompt = format!(
        "{framing}\nProduce the ENTIRE ceremony as one short transcript. Each line is exactly \
         `SPEAKER: message` on its own line. Allowed speakers (use their label verbatim):{roles_line}\n\
         - SM: Scrum Master, facilitator who opens and closes.\n\
         Every turn is first person, 1-2 sentences, concrete and grounded in the context — no \
         preamble, no markdown, no bullet lists, no sign-off. Speak like real teammates.{}",
        lang.reply_directive()
    );
    let request = AgentRequest {
        role: Role::Sm,
        system_prompt,
        task_prompt: task.to_owned(),
        work_dir: work_dir.to_path_buf(),
        timeout: Duration::from_secs(180),
        escalation_level: 0,
    };
    let outcome = engine.run(request).await?;
    if !outcome.succeeded() {
        return Err(PortError::Backend(format!(
            "ceremony engine failed: {}",
            outcome.stderr.trim()
        ))
        .into());
    }
    let mut allowed: Vec<String> = roster.iter().map(|(l, _)| (*l).to_uppercase()).collect();
    allowed.push("SM".to_owned());
    let turns = parse_transcript(&outcome.stdout, &allowed);
    Ok((turns, outcome.stdout))
}

/// Parse a `SPEAKER: line` transcript into ordered turns, keeping only speakers
/// in `allowed` and folding continuation lines into the current turn.
fn parse_transcript(raw: &str, allowed: &[String]) -> Vec<CeremonyTurn> {
    let mut turns: Vec<CeremonyTurn> = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((head, rest)) = trimmed.split_once(':') {
            let label = head.trim().trim_start_matches(['-', '*', '•', ' ']).trim();
            let up = label.to_uppercase();
            if allowed.iter().any(|a| a == &up) {
                turns.push(CeremonyTurn {
                    speaker: canonical_speaker(&up, allowed),
                    text: rest.trim().to_owned(),
                });
                continue;
            }
        }
        // Continuation of the previous speaker's turn (wrapped line).
        if let Some(last) = turns.last_mut() {
            last.text.push(' ');
            last.text.push_str(trimmed);
        }
    }
    turns.retain(|t| !t.text.trim().is_empty());
    turns
}

/// Return the roster's canonical casing for a matched uppercase speaker token.
fn canonical_speaker(up: &str, allowed: &[String]) -> String {
    allowed
        .iter()
        .find(|a| a.as_str() == up)
        .cloned()
        .unwrap_or_else(|| up.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed() -> Vec<String> {
        vec!["SM".into(), "BA".into(), "SA".into(), "DEV-FEATURE".into()]
    }

    #[test]
    fn parses_multi_speaker_transcript() {
        let raw = "SM: Morning team, sprint 3 rolling.\nBA: Finished the spec, next is auth.\n\
                   SA: Design looks fine. BLOCKER: staging is down.\nSM: Focus on unblocking staging.";
        let turns = parse_transcript(raw, &allowed());
        assert_eq!(turns.len(), 4);
        assert_eq!(turns[0].speaker, "SM");
        assert_eq!(turns[1].speaker, "BA");
        assert!(turns[2].text.contains("BLOCKER"));
    }

    #[test]
    fn folds_wrapped_continuation_lines() {
        let raw = "BA: This is a long update\nthat wrapped across lines.\nSA: Short one.";
        let turns = parse_transcript(raw, &allowed());
        assert_eq!(turns.len(), 2);
        assert_eq!(
            turns[0].text,
            "This is a long update that wrapped across lines."
        );
    }

    #[test]
    fn drops_unknown_speakers_and_empty() {
        let raw = "RANDOM: ignore me\nBA: kept\n\nSM:   ";
        let turns = parse_transcript(raw, &allowed());
        // RANDOM line is not a known speaker, but has no prior turn to fold into.
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].speaker, "BA");
    }

    #[test]
    fn tolerates_bullet_prefixed_speaker() {
        let raw = "- SM: opening\n- BA: update";
        let turns = parse_transcript(raw, &allowed());
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].speaker, "SM");
    }
}
