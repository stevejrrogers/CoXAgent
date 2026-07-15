//! `RedisCoord` — Redis-backed ephemeral coordination for the distributed
//! runtime: leader election, per-ticket stage leases, and the worker registry.
//!
//! Each is a key with a native TTL, so expiry is automatic (no manual age
//! checks). Postgres still owns durable state and the transactional
//! `claim_ticket`; Redis just handles the short-lived leases where its `SET NX
//! PX` semantics shine. This is the "combine Postgres + Redis" split: durable
//! data in Postgres, fast concurrency primitives in Redis.

use coxagent_application::ports::outbound::WorkerEntry;
use coxagent_application::PortError;

/// Leader lease (ms) — a runner must renew within this or another takes over.
const LEADER_TTL_MS: u64 = 90_000;
/// Per-ticket stage lease (ms).
const STAGE_TTL_MS: u64 = 1_800_000;
/// Worker presence (ms) — how long a team shows online after its last beat.
const WORKER_TTL_MS: u64 = 120_000;

/// Redis coordinator scoped to one project id. Cloneable-cheap (holds a client).
pub struct RedisCoord {
    client: redis::Client,
    project_id: String,
}

impl RedisCoord {
    /// Open a Redis client for `url` (e.g. `redis://host:6379`), scoped to
    /// `project_id`. Lazy: the first command establishes the connection.
    ///
    /// # Errors
    /// [`PortError::Backend`] if the URL is invalid.
    pub fn connect(url: &str, project_id: impl Into<String>) -> Result<Self, PortError> {
        let client =
            redis::Client::open(url).map_err(|e| PortError::Backend(format!("redis open: {e}")))?;
        Ok(Self {
            client,
            project_id: project_id.into(),
        })
    }

    async fn conn(&self) -> Result<redis::aio::MultiplexedConnection, PortError> {
        self.client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| PortError::Backend(format!("redis conn: {e}")))
    }

    /// Take or renew a lease with a Lua CAS: win iff the key is absent or already
    /// ours, refreshing the TTL. Atomic — two workers can't both win.
    async fn take(&self, key: &str, worker: &str, ttl_ms: u64) -> Result<bool, PortError> {
        let mut c = self.conn().await?;
        let script = redis::Script::new(
            "local v = redis.call('GET', KEYS[1])\n\
             if v == false or v == ARGV[1] then\n\
               redis.call('SET', KEYS[1], ARGV[1], 'PX', ARGV[2])\n\
               return 1\n\
             end\n\
             return 0",
        );
        let got: i64 = script
            .key(key)
            .arg(worker)
            .arg(ttl_ms)
            .invoke_async(&mut c)
            .await
            .map_err(|e| PortError::Backend(format!("redis lease: {e}")))?;
        Ok(got == 1)
    }

    /// Acquire or renew project leadership.
    ///
    /// # Errors
    /// [`PortError`] on a Redis failure.
    pub async fn acquire_leader(&self, worker: &str) -> Result<bool, PortError> {
        self.take(
            &format!("cox:{}:leader", self.project_id),
            worker,
            LEADER_TTL_MS,
        )
        .await
    }

    /// Claim a per-ticket stage lease.
    ///
    /// # Errors
    /// [`PortError`] on a Redis failure.
    pub async fn claim_stage(
        &self,
        ticket: &str,
        stage: &str,
        worker: &str,
    ) -> Result<bool, PortError> {
        self.take(
            &format!("cox:{}:stage:{ticket}:{stage}", self.project_id),
            worker,
            STAGE_TTL_MS,
        )
        .await
    }

    /// Refresh this worker's presence in the registry.
    ///
    /// # Errors
    /// [`PortError`] on a Redis failure.
    pub async fn heartbeat_worker(
        &self,
        worker: &str,
        role: &str,
        ticket: &str,
        now: &str,
    ) -> Result<(), PortError> {
        let mut c = self.conn().await?;
        let key = format!("cox:{}:worker:{worker}", self.project_id);
        let val = serde_json::to_string(&WorkerEntry {
            worker: worker.to_owned(),
            role: role.to_owned(),
            ticket: ticket.to_owned(),
            at: now.to_owned(),
        })
        .unwrap_or_default();
        redis::cmd("SET")
            .arg(&key)
            .arg(val)
            .arg("PX")
            .arg(WORKER_TTL_MS)
            .query_async::<()>(&mut c)
            .await
            .map_err(|e| PortError::Backend(format!("redis heartbeat: {e}")))?;
        Ok(())
    }

    /// List every worker currently online (keys still within TTL).
    ///
    /// # Errors
    /// [`PortError`] on a Redis failure.
    pub async fn workers(&self) -> Result<Vec<WorkerEntry>, PortError> {
        let mut c = self.conn().await?;
        let pattern = format!("cox:{}:worker:*", self.project_id);
        let mut keys: Vec<String> = Vec::new();
        let mut cursor = 0u64;
        loop {
            let (next, batch): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(&mut c)
                .await
                .map_err(|e| PortError::Backend(format!("redis scan: {e}")))?;
            keys.extend(batch);
            cursor = next;
            if cursor == 0 {
                break;
            }
        }
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let vals: Vec<Option<String>> = redis::cmd("MGET")
            .arg(&keys)
            .query_async(&mut c)
            .await
            .map_err(|e| PortError::Backend(format!("redis mget: {e}")))?;
        Ok(vals
            .into_iter()
            .flatten()
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect())
    }
}
