//! `MongoDocStore` — a MongoDB-backed [`DocStorePort`]. Documentation pages are
//! kept in a single `docs` collection, each tagged with its `project` id so one
//! cluster serves every project on the hub. Enabled by `COXAGENT_MONGO_URL`.

use async_trait::async_trait;
use coxagent_application::error::PortError;
use coxagent_application::ports::outbound::DocStorePort;
use coxagent_application::state::DocPage;
use mongodb::bson::{doc, Document};
use mongodb::{Client, Collection};

/// MongoDB adapter for the documentation store.
pub struct MongoDocStore {
    coll: Collection<Document>,
}

impl MongoDocStore {
    /// Connect from the environment, when configured:
    /// - `COXAGENT_MONGO_URL` — connection string (required to enable).
    /// - `COXAGENT_MONGO_DB` — database name (default `coxagent`).
    ///
    /// Returns `None` when unset. Returns `Err` when set but the cluster cannot
    /// be reached, so startup can log and fall back rather than crash.
    ///
    /// # Errors
    /// [`PortError::Backend`] when the URL is present but connection fails.
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
            .map_err(|e| PortError::Backend(format!("mongo connect: {e}")))?;
        // Confirm reachability up front so a bad URL fails loudly at startup.
        client
            .database(&db)
            .run_command(doc! { "ping": 1 })
            .await
            .map_err(|e| PortError::Backend(format!("mongo ping: {e}")))?;
        let coll = client.database(&db).collection::<Document>("docs");
        Ok(Some(Self { coll }))
    }
}

/// Serialise a page (plus its project tag) to a BSON document.
fn to_doc(project: &str, page: &DocPage) -> Result<Document, PortError> {
    let mut d = mongodb::bson::to_document(page)
        .map_err(|e| PortError::Backend(format!("mongo encode: {e}")))?;
    d.insert("project", project);
    Ok(d)
}

/// Deserialise a stored document back to a page (ignores `project`/`_id`).
fn from_doc(d: Document) -> Result<DocPage, PortError> {
    mongodb::bson::from_document(d).map_err(|e| PortError::Backend(format!("mongo decode: {e}")))
}

#[async_trait]
impl DocStorePort for MongoDocStore {
    async fn list(&self, project: &str) -> Result<Vec<DocPage>, PortError> {
        let mut cursor = self
            .coll
            .find(doc! { "project": project })
            .await
            .map_err(|e| PortError::Backend(format!("mongo find: {e}")))?;
        let mut out = Vec::new();
        while cursor
            .advance()
            .await
            .map_err(|e| PortError::Backend(format!("mongo cursor: {e}")))?
        {
            let d = cursor
                .deserialize_current()
                .map_err(|e| PortError::Backend(format!("mongo decode: {e}")))?;
            out.push(from_doc(d)?);
        }
        Ok(out)
    }

    async fn get(&self, project: &str, id: &str) -> Result<Option<DocPage>, PortError> {
        let found = self
            .coll
            .find_one(doc! { "project": project, "id": id })
            .await
            .map_err(|e| PortError::Backend(format!("mongo find_one: {e}")))?;
        found.map(from_doc).transpose()
    }

    async fn upsert(&self, project: &str, page: &DocPage) -> Result<(), PortError> {
        let body = to_doc(project, page)?;
        self.coll
            .update_one(
                doc! { "project": project, "id": &page.id },
                doc! { "$set": body },
            )
            .upsert(true)
            .await
            .map_err(|e| PortError::Backend(format!("mongo upsert: {e}")))?;
        Ok(())
    }

    async fn delete(&self, project: &str, id: &str) -> Result<bool, PortError> {
        let res = self
            .coll
            .delete_one(doc! { "project": project, "id": id })
            .await
            .map_err(|e| PortError::Backend(format!("mongo delete: {e}")))?;
        Ok(res.deleted_count > 0)
    }
}
