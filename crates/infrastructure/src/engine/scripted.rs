//! `ScriptedEngine` — a deterministic, offline `AgentEnginePort` that drives the
//! full pipeline end to end and, for the DEV role, writes real runnable code
//! into the codebase. It exists so the orchestration can be demonstrated and
//! tested without any LLM credentials; the opencode/claude adapters are the real
//! engines and swap in unchanged (same port).
//!
//! It answers by role (detected from the system prompt): BA proposes a fixed
//! backlog once, SA returns a design, DEV writes files, TEST reports no bugs.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{AgentEnginePort, AgentOutcome, AgentRequest};
use coxagent_application::PortError;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A scripted engine that also materialises code for DEV tasks.
#[derive(Default)]
pub struct ScriptedEngine {
    ba_calls: AtomicUsize,
}

impl ScriptedEngine {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn ok(stdout: String) -> AgentOutcome {
        AgentOutcome {
            stdout,
            stderr: String::new(),
            exit_code: Some(0),
            usage: None,
            trace: String::new(),
            session_id: None,
            sandbox: coxagent_application::ports::outbound::SandboxStatus::NotRequested,
            engine: "scripted".to_owned(),
        }
    }
}

#[async_trait]
impl AgentEnginePort for ScriptedEngine {
    fn id(&self) -> &'static str {
        "scripted"
    }

    async fn run(&self, request: AgentRequest) -> Result<AgentOutcome, PortError> {
        let sp = &request.system_prompt;

        if sp.contains("Business Analyst") {
            // Propose a small backlog once; nothing further afterwards.
            if self.ba_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(Self::ok(BA_BACKLOG.to_owned()));
            }
            return Ok(Self::ok("[]".to_owned()));
        }

        if sp.contains("Product Owner") {
            return Ok(Self::ok(PO_MILESTONES.to_owned()));
        }

        if sp.contains("Solution Architect") {
            return Ok(Self::ok(SA_DESIGN.to_owned()));
        }

        if sp.contains("design system") {
            return Ok(Self::ok(PD_DESIGN_SYSTEM.to_owned()));
        }

        if sp.contains("Product Designer") {
            return Ok(Self::ok(PD_UX.to_owned()));
        }

        if sp.contains("QA Engineer") {
            // A clean build: no bugs.
            return Ok(Self::ok("[]".to_owned()));
        }

        if sp.contains("Tech Writer") {
            let dir = request.work_dir.join("docs");
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join("guide.md"), "# Feature guide\n\nUsage docs.\n");
            return Ok(Self::ok("Wrote docs/guide.md".to_owned()));
        }

        if sp.contains("Senior Developer") {
            // Materialise a real, runnable Quotes API into the codebase, then
            // report what changed. Idempotent: always writes the current best
            // implementation so the app runs after any feature.
            write_codebase(&request.work_dir)?;
            return Ok(Self::ok(
                "Implemented app.py (http.server) with /health and /quote".to_owned(),
            ));
        }

        // Unknown role: succeed with an empty response.
        Ok(Self::ok(String::new()))
    }
}

const BA_BACKLOG: &str = r#"[
  {"title":"Health endpoint","description":"GET /health returns {\"status\":\"ok\"}","priority":"high","complexity":"small","has_ui":false},
  {"title":"Random quote endpoint","description":"GET /quote returns a random quote with author","priority":"high","complexity":"small","has_ui":false},
  {"title":"Quotes by author filter","description":"GET /quote?author=X filters by author","priority":"medium","complexity":"small","has_ui":false}
]"#;

const PO_MILESTONES: &str = r#"[
  {"name":"Walking skeleton","goal":"A deployable service with health + one real endpoint","target_version":"0.2.0"},
  {"name":"Core API","goal":"All primary endpoints working with filters","target_version":"0.5.0"},
  {"name":"Launch","goal":"Hardened, documented, production-ready v1","target_version":"1.0.0"}
]"#;

const SA_DESIGN: &str = r#"{
  "approach":"Python stdlib http.server; a single app.py with a request handler routing /health and /quote; quotes held in an in-memory list.",
  "files":["app.py","README.md"],
  "api_contract":"GET /health -> 200 {status}; GET /quote -> 200 {quote,author}",
  "data_changes":"none (in-memory)",
  "test_plan":"curl /health and /quote, assert 200 and JSON shape"
}"#;

const PD_DESIGN_SYSTEM: &str = r#"{
  "principles":"calm, focused, content-first; generous whitespace",
  "palette":["primary: cyan #0891B2","text: slate #0f172a","bg: white #ffffff"],
  "typography":"Inter; 16px base; 1.25 scale; 600 weight for headings",
  "components":["buttons: 8px radius, filled primary","cards: 12px radius, subtle border"]
}"#;

const PD_UX: &str = r#"{
  "user_flow":"User opens the page, sees a quote, clicks refresh for a new one",
  "screens":["quote view"],
  "component_states":["loading","loaded","error"],
  "responsive_notes":"single column; button full-width on mobile"
}"#;

/// Write a working, deployable Quotes API into `dir` (code + docker files).
fn write_codebase(dir: &std::path::Path) -> Result<(), PortError> {
    let w = |name: &str, body: &str| {
        std::fs::write(dir.join(name), body).map_err(|e| PortError::Backend(e.to_string()))
    };
    std::fs::create_dir_all(dir).map_err(|e| PortError::Backend(e.to_string()))?;
    w("app.py", APP_PY)?;
    w("README.md", README_MD)?;
    w("Dockerfile", DOCKERFILE)?;
    w("docker-compose.yml", COMPOSE)?;
    Ok(())
}

const DOCKERFILE: &str = "FROM python:3.12-slim\nWORKDIR /app\nCOPY app.py .\nEXPOSE 8000\nCMD [\"python3\", \"app.py\"]\n";

const COMPOSE: &str = "services:\n  quotes:\n    build: .\n    ports:\n      - \"8000:8000\"\n";

const APP_PY: &str = r#"#!/usr/bin/env python3
"""Quotes API — Python stdlib only. Run: python3 app.py (listens on :8000)."""
import json
import random
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import urlparse, parse_qs

QUOTES = [
    {"quote": "Simplicity is the soul of efficiency.", "author": "Austin Freeman"},
    {"quote": "Make it work, make it right, make it fast.", "author": "Kent Beck"},
    {"quote": "Programs must be written for people to read.", "author": "Harold Abelson"},
    {"quote": "The best way to predict the future is to invent it.", "author": "Alan Kay"},
]


class Handler(BaseHTTPRequestHandler):
    def _send(self, code, payload):
        body = json.dumps(payload).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        parsed = urlparse(self.path)
        if parsed.path == "/health":
            self._send(200, {"status": "ok"})
        elif parsed.path == "/quote":
            author = parse_qs(parsed.query).get("author", [None])[0]
            pool = [q for q in QUOTES if not author or q["author"] == author]
            if not pool:
                self._send(404, {"error": "no quote for author"})
            else:
                self._send(200, random.choice(pool))
        else:
            self._send(404, {"error": "not found"})

    def log_message(self, *_):
        pass


if __name__ == "__main__":
    HTTPServer(("0.0.0.0", 8000), Handler).serve_forever()
"#;

const README_MD: &str = r#"# Quotes API

A tiny JSON API built with the Python standard library.

## Run
```sh
python3 app.py   # listens on :8000
```

## Endpoints
- `GET /health` -> `{"status":"ok"}`
- `GET /quote` -> a random `{"quote":..,"author":..}`
- `GET /quote?author=Kent Beck` -> filter by author
"#;
