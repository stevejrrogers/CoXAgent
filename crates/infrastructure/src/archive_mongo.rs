//! `MongoTicketArchive` — a MongoDB-backed [`ArchiveStorePort`], the
//! production cold store for tickets evicted from a project's hot state
//! (CXA-F264b). Mirrors [`crate::docs_store::MongoDocStore`]: enabled by
//! `COXAGENT_MONGO_URL` (shared with the docs store — no separate knobs),
//! pinged at construction so a bad URL fails loudly at startup where the
//! composition root can log and fall back.
//!
//! One `archived_tickets` collection serves every project; documents are
//! keyed `(project, id)` with a UNIQUE index on that pair — the database
//! -level backstop of `put`'s upsert semantics, so a retried eviction can
//! never duplicate a row. A secondary `(project, archived_at)` index is the
//! cursor-paging groundwork for the eviction sweep (CXA-F264c).

use async_trait::async_trait;
use coxagent_application::error::PortError;
use coxagent_application::ports::outbound::ArchiveStorePort;
use coxagent_domain::ticket::Ticket;
use mongodb::bson::{doc, Document};
use mongodb::options::IndexOptions;
use mongodb::{Client, Collection, IndexModel};

/// The collection holding archived (evicted) tickets, tagged with their
/// project id so one cluster serves every project on the hub.
const COLLECTION: &str = "archived_tickets";

/// MongoDB adapter for the ticket archive (cold store).
#[derive(Debug)]
pub struct MongoTicketArchive {
    coll: Collection<Document>,
}

impl MongoTicketArchive {
    /// Connect from the environment, when configured:
    /// - `COXAGENT_MONGO_URL` — connection string (required to enable).
    /// - `COXAGENT_MONGO_DB` — database name (default `coxagent`).
    ///
    /// Returns `None` when unset or blank. Returns `Err` when set but the
    /// cluster cannot be reached, so startup can log and fall back rather
    /// than crash. Never panics.
    ///
    /// # Errors
    /// [`PortError::Backend`] when the URL is present but connection,
    /// ping or index creation fails.
    pub async fn from_env() -> Result<Option<Self>, PortError> {
        let Ok(url) = std::env::var("COXAGENT_MONGO_URL") else {
            return Ok(None);
        };
        if url.trim().is_empty() {
            return Ok(None);
        }
        let db = std::env::var("COXAGENT_MONGO_DB").unwrap_or_else(|_| "coxagent".to_owned());
        let client = Client::with_uri_str(&url)
            .await
            .map_err(|e| PortError::Backend(format!("mongo archive connect: {e}")))?;
        // Confirm reachability up front so a bad URL fails loudly at startup.
        client
            .database(&db)
            .run_command(doc! { "ping": 1 })
            .await
            .map_err(|e| PortError::Backend(format!("mongo archive ping: {e}")))?;
        let coll = client.database(&db).collection::<Document>(COLLECTION);
        ensure_indexes(&coll).await?;
        Ok(Some(Self { coll }))
    }
}

/// The document key: one row per `(project, ticket id)`. The upsert filter
/// and the unique index must agree on exactly this pair.
fn key_filter(project: &str, id: &str) -> Document {
    doc! { "project": project, "id": id }
}

/// Build the archive's indexes at construction, not lazily on first put: a
/// mis-configured cold store must fail at startup, where the fallback can
/// still act (AC3). The UNIQUE `(project, id)` index is the database-level
/// backstop of `put`'s upsert semantics (AC1); the `(project, archived_at)`
/// secondary is cursor-paging groundwork for the eviction sweep (CXA-F264c).
async fn ensure_indexes(coll: &Collection<Document>) -> Result<(), PortError> {
    coll.create_index(
        IndexModel::builder()
            .keys(doc! { "project": 1, "id": 1 })
            .options(IndexOptions::builder().unique(true).build())
            .build(),
    )
    .await
    .map_err(|e| PortError::Backend(format!("mongo archive index: {e}")))?;
    coll.create_index(
        IndexModel::builder()
            .keys(doc! { "project": 1, "archived_at": 1 })
            .build(),
    )
    .await
    .map_err(|e| PortError::Backend(format!("mongo archive index: {e}")))?;
    Ok(())
}

/// Serialise one archived ticket to its stored document: the keying fields
/// (`project`, `id`), the archive stamp, and the aggregate itself nested
/// under `ticket` so the keying fields can never collide with the
/// aggregate's own serialized shape.
fn to_doc(project: &str, ticket: &Ticket, archived_at: &str) -> Result<Document, PortError> {
    let ticket_doc = mongodb::bson::to_document(ticket)
        .map_err(|e| PortError::Backend(format!("mongo archive encode: {e}")))?;
    Ok(doc! {
        "project": project,
        "id": ticket.id().as_str(),
        "archived_at": archived_at,
        "ticket": ticket_doc,
    })
}

/// Inverse of [`to_doc`] — the aggregate only; the envelope's keying fields
/// (`project`, `id`, `archived_at`, `_id`) are not part of the port.
fn from_doc(d: &Document) -> Result<Ticket, PortError> {
    let ticket = d.get_document("ticket").map_err(|_| {
        PortError::Backend("mongo archive decode: document has no ticket envelope".to_owned())
    })?;
    mongodb::bson::from_document::<Ticket>(ticket.clone())
        .map_err(|e| PortError::Backend(format!("mongo archive decode: {e}")))
}

#[async_trait]
impl ArchiveStorePort for MongoTicketArchive {
    /// Idempotent upsert keyed `(project, id)`: archiving the same ticket
    /// twice succeeds and keeps one document, latest version wins (AC1).
    async fn put(&self, project: &str, ticket: &Ticket) -> Result<(), PortError> {
        let archived_at = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|e| PortError::Backend(format!("archive stamp: {e}")))?;
        let body = to_doc(project, ticket, &archived_at)?;
        self.coll
            .update_one(
                key_filter(project, ticket.id().as_str()),
                doc! { "$set": body },
            )
            .upsert(true)
            .await
            .map_err(|e| PortError::Backend(format!("mongo archive upsert: {e}")))?;
        Ok(())
    }

    async fn get(&self, project: &str, id: &str) -> Result<Option<Ticket>, PortError> {
        let found = self
            .coll
            .find_one(key_filter(project, id))
            .await
            .map_err(|e| PortError::Backend(format!("mongo archive find_one: {e}")))?;
        found.map(|d| from_doc(&d)).transpose()
    }

    async fn list(&self, project: &str) -> Result<Vec<Ticket>, PortError> {
        let mut cursor = self
            .coll
            .find(doc! { "project": project })
            .await
            .map_err(|e| PortError::Backend(format!("mongo archive find: {e}")))?;
        let mut out = Vec::new();
        while cursor
            .advance()
            .await
            .map_err(|e| PortError::Backend(format!("mongo archive cursor: {e}")))?
        {
            let d = cursor
                .deserialize_current()
                .map_err(|e| PortError::Backend(format!("mongo archive decode: {e}")))?;
            out.push(from_doc(&d)?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use coxagent_domain::{Complexity, Priority, Role, TechnicalDesign, TicketId, TicketType};

    /// A fully populated archived ticket — criteria, test cases with verdicts
    /// and evidence (note/image/repro), the SA design — exactly the shape the
    /// cold store must never lose (AC3).
    fn fully_populated_ticket(id: &str) -> Ticket {
        let mut t = Ticket::new(
            TicketId::new(id).unwrap(),
            TicketType::Feature,
            "Mongo cold store keeps archived tickets whole",
            "eviction must never lose criteria, test cases or evidence",
            Priority::High,
            Complexity::Medium,
            false,
        )
        .unwrap();
        t.set_technical_design(
            Role::Sa,
            TechnicalDesign {
                approach: "MongoTicketArchive behind the ArchiveStorePort".to_owned(),
                files: vec!["crates/infrastructure/src/archive_mongo.rs".to_owned()],
                ..TechnicalDesign::default()
            },
        )
        .unwrap();
        t.set_acceptance_criteria(vec![
            "put is an upsert keyed on (project, id)".to_owned(),
            "a fully populated ticket survives the round trip".to_owned(),
        ]);
        t.stamp_created_at("2026-09-07T00:00:00Z");
        t.ensure_test_cases_from_acceptance();
        assert!(
            t.set_test_case_result(
                "put is an upsert keyed on (project, id)",
                true,
                Some("archived twice, one row".to_owned()),
                Some("/api/projects/cxa/media/f267.png".to_owned()),
                "2026-09-07T09:00:00Z".to_owned(),
            ),
            "the fixture's first case must exist to carry its verdict"
        );
        assert!(
            t.set_test_case_repro(
                "put is an upsert keyed on (project, id)",
                "http://127.0.0.1:4000/projects".to_owned(),
            ),
            "the fixture's repro attaches to existing evidence"
        );
        assert!(
            t.set_test_case_result(
                "a fully populated ticket survives the round trip",
                false,
                Some("round trip pending the adapter".to_owned()),
                None,
                "2026-09-07T09:05:00Z".to_owned(),
            ),
            "the fixture's second case must exist to carry its verdict"
        );
        t
    }

    #[test]
    fn the_envelope_round_trips_a_fully_populated_ticket() {
        let t = fully_populated_ticket("CXC-F267-001");
        let d = to_doc("demo", &t, "2026-09-07T10:00:00Z").unwrap();
        assert_eq!(d.get_str("project").unwrap(), "demo");
        assert_eq!(d.get_str("id").unwrap(), "CXC-F267-001");
        assert_eq!(d.get_str("archived_at").unwrap(), "2026-09-07T10:00:00Z");

        let back = from_doc(&d).unwrap();
        assert_eq!(
            back, t,
            "the full aggregate survives the envelope unchanged"
        );
        assert_eq!(back.id().as_str(), "CXC-F267-001");
        assert_eq!(back.acceptance_criteria(), t.acceptance_criteria());
        assert_eq!(back.test_cases(), t.test_cases());
    }

    #[test]
    fn a_document_without_a_ticket_envelope_is_refused_not_misread() {
        let err = from_doc(&doc! { "project": "demo", "id": "CXC-F267-001" }).unwrap_err();
        assert!(err.to_string().contains("no ticket envelope"));
    }

    #[test]
    fn the_key_filter_keys_documents_on_project_and_id() {
        assert_eq!(
            key_filter("demo", "CXC-F267-001"),
            doc! { "project": "demo", "id": "CXC-F267-001" }
        );
    }

    /// Env vars are process-global: tests that read or write them hold this
    /// async-aware lock (a std guard across `.await` trips clippy).
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn the_env_contract_answers_none_for_unset_and_blank_url() {
        let _guard = ENV_LOCK.lock().await;
        let prior = std::env::var("COXAGENT_MONGO_URL").ok();

        std::env::remove_var("COXAGENT_MONGO_URL");
        assert!(
            MongoTicketArchive::from_env().await.unwrap().is_none(),
            "unset COXAGENT_MONGO_URL → Ok(None)"
        );
        std::env::set_var("COXAGENT_MONGO_URL", "   ");
        assert!(
            MongoTicketArchive::from_env().await.unwrap().is_none(),
            "blank COXAGENT_MONGO_URL → Ok(None), never a connect attempt"
        );

        match prior {
            Some(v) => std::env::set_var("COXAGENT_MONGO_URL", v),
            None => std::env::remove_var("COXAGENT_MONGO_URL"),
        }
    }

    #[tokio::test]
    async fn an_unreachable_url_maps_to_a_backend_error_carrying_the_cause() {
        let _guard = ENV_LOCK.lock().await;
        let prior = std::env::var("COXAGENT_MONGO_URL").ok();
        // Closed loopback port, tight timeouts: the ping must fail fast and
        // loudly — no server, no listener, just a refused dial (AC2's
        // "URL set but unreachable" branch; cluster reachability itself is
        // covered by the deploy smoke).
        std::env::set_var(
            "COXAGENT_MONGO_URL",
            "mongodb://127.0.0.1:1/?serverSelectionTimeoutMS=250&connectTimeoutMS=250",
        );
        let err = MongoTicketArchive::from_env()
            .await
            .expect_err("an unreachable cluster must answer Err");
        match prior {
            Some(v) => std::env::set_var("COXAGENT_MONGO_URL", v),
            None => std::env::remove_var("COXAGENT_MONGO_URL"),
        }
        let msg = err.to_string();
        assert!(
            msg.contains("mongo archive ping") || msg.contains("mongo archive connect"),
            "the error must name the failing step with the underlying cause: {msg}"
        );
    }
}
