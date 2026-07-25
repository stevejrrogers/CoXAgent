//! `AnalyzeAttachmentUseCase` — when a human drops an image or file into the
//! Discussion, an agent reads it and responds. Two modes, decided from the
//! accompanying message:
//!   - **answer**: the human asked something ("what's wrong with this diagram?")
//!     → the agent reads the attachment and answers the question directly.
//!   - **proactive**: the human posted only an attachment (or a bare caption)
//!     → the agent reads it anyway, says what it sees, and asks one sharp
//!     clarifying question back so the thread keeps moving.
//!
//! The agent's file-reading ability (the `claude`/opencode Read tool, which
//! natively views images) is driven by handing it the absolute on-disk paths of
//! the uploaded media; the URL the browser uses is not reachable from the CLI.

use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use coxagent_domain::Role;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// One attachment the agent can open, resolved to its physical path.
pub struct ReadableAttachment {
    pub name: String,
    pub mime: String,
    pub path: PathBuf,
}

impl ReadableAttachment {
    fn is_image(&self) -> bool {
        self.mime.starts_with("image/")
    }
}

/// Runs a single agent turn that reads posted attachments and replies.
pub struct AnalyzeAttachmentUseCase<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> {
    store: Arc<S>,
    engine: Arc<E>,
    work_dir: PathBuf,
}

impl<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> AnalyzeAttachmentUseCase<S, E> {
    pub fn new(store: Arc<S>, engine: Arc<E>, work_dir: PathBuf) -> Self {
        Self {
            store,
            engine,
            work_dir,
        }
    }

    /// Read `attachments` (posted by `author` with the caption `body` on the
    /// `ticket` thread) and post the agent's response as a comment.
    ///
    /// Returns the posted reply text. Attachments with no readable file are
    /// skipped; if none are readable the call is a no-op returning `None`.
    ///
    /// # Errors
    /// [`AppError`] when the engine fails.
    pub async fn execute(
        &self,
        author: &str,
        body: &str,
        ticket: Option<String>,
        attachments: &[ReadableAttachment],
    ) -> Result<Option<String>, AppError> {
        let readable: Vec<&ReadableAttachment> =
            attachments.iter().filter(|a| a.path.is_file()).collect();
        if readable.is_empty() {
            return Ok(None);
        }

        let asked = looks_like_question(body);
        let task = compose_task(author, body, asked, &readable);

        let request = AgentRequest {
            role: Role::Sa,
            system_prompt: SYSTEM_PROMPT.to_owned(),
            task_prompt: task,
            work_dir: self.work_dir.clone(),
            // Reading an image and reasoning about it can take a while.
            timeout: Duration::from_secs(180),
            escalation_level: 0,
        };
        let outcome = self.engine.run(request).await?;
        if !outcome.succeeded() {
            return Err(PortError::Backend(format!(
                "attachment analysis failed: {}",
                outcome.stderr.trim()
            ))
            .into());
        }
        let reply = outcome.stdout.trim().to_owned();
        if reply.is_empty() {
            return Ok(None);
        }
        self.post("SA", &reply, ticket).await;
        Ok(Some(reply))
    }

    async fn post(&self, author: &str, body: &str, ticket: Option<String>) {
        if let Ok(mut state) = self.store.load().await {
            state.post_comment(author, body, ticket);
            let _ = self.store.save(&state).await;
        }
    }
}

fn compose_task(author: &str, body: &str, asked: bool, readable: &[&ReadableAttachment]) -> String {
    let mut files = String::new();
    for a in readable {
        let kind = if a.is_image() { "image" } else { "file" };
        let _ = writeln!(
            files,
            "- {kind} \"{}\" ({}) — read it at: {}",
            a.name,
            a.mime,
            a.path.display()
        );
    }

    let caption = if body.trim().is_empty() {
        "(no message — just the attachment)".to_owned()
    } else {
        format!("\"{}\"", body.trim())
    };

    let instruction = if asked {
        "The teammate asked a question. Open each attachment with your file-reading tool \
             (it can view images directly), then answer their question specifically, grounded in \
             what you actually see. If something is genuinely ambiguous, note your assumption. \
             2-5 sentences, no preamble."
    } else {
        "The teammate dropped this in without asking anything specific. Open each attachment \
             with your file-reading tool (it can view images directly), say concisely what you \
             see and what stands out, then ask ONE sharp, useful question back — the thing you'd \
             most need to know to help. 2-5 sentences, no preamble."
    };

    format!(
        "{author} posted in the team Discussion.\nCaption: {caption}\n\nAttachments:\n{files}\n{instruction}"
    )
}

const SYSTEM_PROMPT: &str = "You are SA (Solution Architect) on an autonomous software team, \
    reviewing something a teammate shared in the Discussion channel. Speak plainly in the first \
    person, like a real teammate. You have a file-reading tool that can open and view images and \
    text files directly — use it on the paths you are given before you respond. Be concrete and \
    concise; never claim you cannot see images.";

/// Leading words that mark a caption as a direct question to answer.
const QUESTION_LEADS: [&str; 12] = [
    "what",
    "why",
    "how",
    "when",
    "where",
    "which",
    "who",
    "can you",
    "could you",
    "should",
    "is this",
    "does",
];

/// Heuristic: did the caption ask something the agent should answer directly?
/// A question mark, or a leading interrogative, counts.
fn looks_like_question(body: &str) -> bool {
    let b = body.trim().to_lowercase();
    if b.is_empty() {
        return false;
    }
    if b.contains('?') {
        return true;
    }
    QUESTION_LEADS.iter().any(|w| b.starts_with(w))
}

#[cfg(test)]
mod tests {
    use super::looks_like_question;

    #[test]
    fn detects_questions() {
        assert!(looks_like_question("what is wrong here?"));
        assert!(looks_like_question("How should we lay this out"));
        assert!(looks_like_question("can you review this mock"));
        assert!(looks_like_question("Does this match the spec?"));
    }

    #[test]
    fn plain_captions_are_not_questions() {
        assert!(!looks_like_question("here's the latest mock"));
        assert!(!looks_like_question(""));
        assert!(!looks_like_question("new design attached"));
    }
}
