//! SQLite WAL checkpoint store. A bounded mailbox serializes transactions on
//! one dedicated writer thread; acknowledgements follow FULL synchronous commit.
pub mod files;
pub mod peer;
#[cfg(test)]
mod tests;

use okytes_core::{
    engine::JobStore,
    error::{EngineError, PersistenceError},
    state::JobState,
    types::{JobId, JobRecord, SegmentRecord},
};
use rusqlite::{params, Connection};
use std::{future::Future, path::Path, pin::Pin};
use tokio::sync::{mpsc, oneshot};

type Reply<T> = Pin<Box<dyn Future<Output = Result<T, EngineError>> + Send>>;
type Command = Box<dyn FnOnce(&mut Connection) + Send>;

/// A cloneable handle to a dedicated SQLite writer.
#[derive(Clone)]
pub struct SqliteStore {
    tx: mpsc::Sender<Command>,
    path: std::sync::Arc<std::path::PathBuf>,
}

fn db(error: impl std::fmt::Display) -> EngineError {
    PersistenceError::Database(error.to_string()).into()
}

/// Exclusive process ownership of transfer writes. The OS releases it on crash.
pub struct EngineLease {
    _file: std::fs::File,
}

impl SqliteStore {
    /// Profile path for native lease ownership, never selected by remote requests.
    pub fn database_path(&self) -> &Path {
        &self.path
    }
    /// Whether an executor still owns native data I/O for this job.
    pub fn torrent_active(&self, id: JobId) -> Reply<bool> {
        self.call(move|conn|conn.query_row("SELECT COALESCE(json_extract(descriptor,'$.active'),0) FROM torrent_jobs WHERE job_id=?1",[id.to_string()],|r|r.get(0)).map_err(db))
    }
    /// Acknowledge quiescence after sockets, storage and callbacks have drained.
    pub fn torrent_idle(&self, id: JobId) -> Reply<()> {
        self.call(move|conn|{conn.execute("UPDATE torrent_jobs SET descriptor=json_set(descriptor,'$.active',json('false')) WHERE job_id=?1",[id.to_string()]).map_err(db)?;Ok(())})
    }
    /// Control intent is atomic with the generic state; publication cannot race it.
    pub fn torrent_control(&self, id: JobId, state: JobState) -> Reply<()> {
        self.call(move|conn|{let tx=conn.transaction().map_err(db)?;let text:String=tx.query_row("SELECT record FROM jobs WHERE id=?1",[id.to_string()],|r|r.get(0)).map_err(db)?;let mut job:JobRecord=serde_json::from_str(&text).map_err(db)?;if job.protocol!="torrent" || job.state==JobState::Verifying{return Err(db("torrent publication is protected"));}
if job.state!=JobState::Completed{job.state=job.state.transition(state,id)?;job.updated_at=chrono::Utc::now();save_job(&tx,&job)?;}tx.execute("UPDATE torrent_jobs SET descriptor=json_set(descriptor,'$.stop_requested',json('true')) WHERE job_id=?1",[id.to_string()]).map_err(db)?;tx.commit().map_err(db)?;Ok(())})
    }
    /// Reset a stopped executor only after its job has explicitly entered probing.
    pub fn torrent_begin(&self, id: JobId) -> Reply<()> {
        self.call(move|conn|{let changed=conn.execute("UPDATE torrent_jobs SET descriptor=json_set(descriptor,'$.stop_requested',json('false'),'$.active',json('true')) WHERE job_id=?1 AND EXISTS(SELECT 1 FROM jobs WHERE id=?1 AND state='probing')",[id.to_string()]).map_err(db)?;if changed!=1{return Err(db("torrent startup was interrupted"));}Ok(())})
    }
    /// Small control polling query; never repeatedly fetch a metainfo blob.
    pub fn torrent_stop_requested(&self, id: JobId) -> Reply<bool> {
        self.call(move|conn|conn.query_row("SELECT COALESCE(json_extract(descriptor,'$.stop_requested'),0) FROM torrent_jobs WHERE job_id=?1",[id.to_string()],|r|r.get(0)).map_err(db))
    }
    /// Update protocol progress without overwriting a concurrent pause/cancel.
    pub fn torrent_progress(&self, id: JobId, bytes: u64, total: u64) -> Reply<()> {
        self.call(move |conn| {
            if bytes > total || total > 64 * 1024 * 1024 * 1024 {
                return Err(db("invalid torrent progress"));
            }
            // An independent controller may use another writer connection.
            // Reserve the write lock before reading its state, so a later
            // progress update cannot resurrect a paused/cancelled job.
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(db)?;
            let text: String = tx
                .query_row(
                    "SELECT record FROM jobs WHERE id=?1",
                    [id.to_string()],
                    |row| row.get(0),
                )
                .map_err(db)?;
            let mut job: JobRecord = serde_json::from_str(&text).map_err(db)?;
            if job.protocol != "torrent" {
                return Err(db("wrong progress executor"));
            }
            job.bytes_downloaded = bytes;
            job.total_size = Some(total);
            job.updated_at = chrono::Utc::now();
            save_job(&tx, &job)?;
            tx.commit().map_err(db)
        })
    }
    /// Atomically add an opaque protocol descriptor and its generic job record.
    pub fn create_torrent(&self, job: &JobRecord, descriptor: &str, metadata: &[u8]) -> Reply<()> {
        let job = job.clone();
        let descriptor = descriptor.to_owned();
        let metadata = metadata.to_vec();
        self.call(move |conn|{if descriptor.len()>32768 || metadata.len()>512*1024{return Err(db("torrent record exceeds bounds"));}let tx=conn.transaction().map_err(db)?;save_job(&tx,&job)?;tx.execute("INSERT INTO torrent_jobs(job_id,descriptor,metadata,upload_reserved) VALUES (?1,?2,?3,0)",params![job.id.to_string(),descriptor,metadata]).map_err(db)?;tx.commit().map_err(db)?;Ok(())})
    }
    /// Read bounded opaque torrent state and original metainfo bytes.
    pub fn torrent_record(&self, id: JobId) -> Reply<Option<(String, Vec<u8>)>> {
        self.call(move |conn| {
            use rusqlite::OptionalExtension;
            conn.query_row(
                "SELECT descriptor,metadata FROM torrent_jobs WHERE job_id=?1",
                [id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db)
        })
    }
    /// Persist app-level torrent state without altering HTTP segment tables.
    pub fn set_torrent_record(&self, id: JobId, descriptor: &str, metadata: &[u8]) -> Reply<()> {
        let descriptor = descriptor.to_owned();
        let metadata = metadata.to_vec();
        self.call(move |conn| {
            if descriptor.len() > 32768 || metadata.len() > 512 * 1024 {
                return Err(db("torrent record exceeds bounds"));
            }
            conn.execute(
                "UPDATE torrent_jobs SET descriptor=json_set(?2,'$.stop_requested',json(CASE WHEN json_extract(descriptor,'$.stop_requested')=1 THEN 'true' ELSE 'false' END),'$.active',json(CASE WHEN json_extract(descriptor,'$.active')=1 THEN 'true' ELSE 'false' END)),metadata=?3 WHERE job_id=?1",
                params![id.to_string(), descriptor, metadata],
            )
            .map_err(db)?;
            Ok(())
        })
    }
    /// Reserve upload quota durably before a sharing protocol can send payload.
    pub fn reserve_torrent_upload(&self, id: JobId, bytes: u64, cap: Option<u64>) -> Reply<bool> {
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db)?;
            let old: u64 = tx
                .query_row(
                    "SELECT upload_reserved FROM torrent_jobs WHERE job_id=?1",
                    [id.to_string()],
                    |r| r.get(0),
                )
                .map_err(db)?;
            let Some(next) = old.checked_add(bytes).filter(|n| *n <= i64::MAX as u64) else {
                return Err(db("upload ledger overflow"));
            };
            if cap.is_some_and(|cap| next > cap) {
                return Ok(false);
            }
            tx.execute(
                "UPDATE torrent_jobs SET upload_reserved=?2 WHERE job_id=?1",
                params![id.to_string(), next],
            )
            .map_err(db)?;
            tx.commit().map_err(db)?;
            Ok(true)
        })
    }
    /// Read verified piece markers, bounded by the adapter's validated metadata.
    pub fn torrent_pieces(&self, id: JobId, count: usize) -> Reply<Vec<bool>> {
        self.call(move |conn| {
            if count > 16384 {
                return Err(db("piece bitmap exceeds bounds"));
            }
            let mut bits = vec![false; count];
            let mut statement = conn
                .prepare("SELECT piece FROM torrent_pieces WHERE job_id=?1")
                .map_err(db)?;
            let rows = statement
                .query_map([id.to_string()], |r| r.get::<_, usize>(0))
                .map_err(db)?;
            for row in rows {
                let index = row.map_err(db)?;
                if let Some(bit) = bits.get_mut(index) {
                    *bit = true;
                }
            }
            Ok(bits)
        })
    }
    /// Replace restart-rechecked verified markers in one transaction.
    pub fn replace_torrent_pieces(&self, id: JobId, bits: Vec<bool>) -> Reply<()> {
        self.call(move |conn| {
            if bits.len() > 16384 {
                return Err(db("piece bitmap exceeds bounds"));
            }
            let tx = conn.transaction().map_err(db)?;
            tx.execute(
                "DELETE FROM torrent_pieces WHERE job_id=?1",
                [id.to_string()],
            )
            .map_err(db)?;
            for (index, verified) in bits.into_iter().enumerate() {
                if verified {
                    tx.execute(
                        "INSERT INTO torrent_pieces VALUES (?1,?2)",
                        params![id.to_string(), index],
                    )
                    .map_err(db)?;
                }
            }
            tx.commit().map_err(db)?;
            Ok(())
        })
    }
    /// Synchronous storage callback: verified data must already be synced.
    /// A finite writer mailbox and acknowledgement timeout fail closed.
    pub fn commit_torrent_piece(&self, id: JobId, piece: usize) -> Result<(), EngineError> {
        if piece >= 16384 {
            return Err(db("piece index exceeds bounds"));
        }
        let (reply, receive) = std::sync::mpsc::sync_channel(1);
        self.tx
            .try_send(Box::new(move |conn| {
                let result = (|| {
                    let tx = conn.transaction().map_err(db)?;
                    let active: bool = tx
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM jobs WHERE id=?1 AND state='downloading')",
                            [id.to_string()],
                            |r| r.get(0),
                        )
                        .map_err(db)?;
                    if !active {
                        return Err(db("piece acknowledgement was stopped"));
                    }
                    tx.execute(
                        "INSERT OR IGNORE INTO torrent_pieces VALUES (?1,?2)",
                        params![id.to_string(), piece],
                    )
                    .map_err(db)?;
                    tx.commit().map_err(db)?;
                    Ok(())
                })();
                let _ = reply.send(result);
            }))
            .map_err(db)?;
        receive
            .recv_timeout(std::time::Duration::from_secs(5))
            .map_err(db)?
    }
    /// Read a bounded admission window without scanning terminal history.
    pub fn runnable(&self) -> Reply<Vec<JobRecord>> {
        self.call(|conn| {
            let mut stmt = conn.prepare("SELECT record FROM jobs WHERE state IN ('queued','probing','downloading','verifying') ORDER BY priority DESC,rowid ASC LIMIT 64").map_err(db)?;
            let rows = stmt.query_map([], |r|r.get::<_,String>(0)).map_err(db)?;
            rows.map(|row|serde_json::from_str(&row.map_err(db)?).map_err(db)).collect()
        })
    }
    /// Read a named non-secret preference from the serialized writer.
    pub fn preference(&self, key: &str) -> Reply<Option<String>> {
        let key = key.to_owned();
        self.call(move |conn| {
            use rusqlite::OptionalExtension;
            conn.query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .optional()
            .map_err(db)
        })
    }

    /// Persist a bounded non-secret preference.
    pub fn set_preference(&self, key: &str, value: &str) -> Reply<()> {
        let key = key.to_owned();
        let value = value.to_owned();
        self.call(move |conn| {
            if key.len() > 64 || value.len() > 16384 { return Err(db("preference exceeds size limit")); }
            conn.execute("INSERT INTO settings VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key,value]).map_err(db)?;
            Ok(())
        })
    }

    /// Commit related non-secret preferences together, without partial settings
    /// updates when engine configuration and queue preferences change together.
    pub fn set_preferences(&self, values: Vec<(String, String)>) -> Reply<()> {
        self.call(move |conn|{
            if values.len()>32 || values.iter().any(|(key,value)|key.len()>64 || value.len()>16384){return Err(db("preferences exceed size limit"));}
            let transaction=conn.transaction().map_err(db)?;
            for (key,value) in values {transaction.execute("INSERT INTO settings VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value]).map_err(db)?;}
            transaction.commit().map_err(db)
        })
    }

    /// Return a retained daemon idempotency result. A different digest for the
    /// same owner, route and key is reported to the caller as a conflict.
    pub fn idempotency_result(
        &self,
        owner: &str,
        route: &str,
        key: &str,
        digest: &str,
    ) -> Reply<Option<String>> {
        let owner = owner.to_owned();
        let route = route.to_owned();
        let key = key.to_owned();
        let digest = digest.to_owned();
        self.call(move |conn| {
            use rusqlite::OptionalExtension;
            let cutoff = chrono::Utc::now().timestamp() - 86_400;
            conn.execute("DELETE FROM api_idempotency WHERE created_at < ?1", [cutoff])
                .map_err(db)?;
            let stored: Option<(String, String)> = conn
                .query_row(
                    "SELECT digest,response FROM api_idempotency WHERE owner=?1 AND route=?2 AND key=?3",
                    params![owner, route, key],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(db)?;
            match stored {
                Some((stored_digest, response)) if stored_digest == digest => Ok(Some(response)),
                Some(_) => Err(EngineError::Config("idempotency key conflicts with an earlier request".into())),
                None => Ok(None),
            }
        })
    }

    /// Retain a successful creation response for 24 hours. The unique key is
    /// scoped to the authenticated token owner and API route.
    pub fn store_idempotency_result(
        &self,
        owner: &str,
        route: &str,
        key: &str,
        digest: &str,
        response: &str,
    ) -> Reply<()> {
        let values = (
            owner.to_owned(),
            route.to_owned(),
            key.to_owned(),
            digest.to_owned(),
            response.to_owned(),
        );
        self.call(move |conn| {
            if values.0.len() > 128 || values.1.len() > 64 || values.2.len() > 128 || values.3.len() > 128 || values.4.len() > 1_048_576 {
                return Err(db("idempotency record exceeds size limit"));
            }
            conn.execute(
                "INSERT INTO api_idempotency(owner,route,key,digest,response,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
                params![values.0, values.1, values.2, values.3, values.4, chrono::Utc::now().timestamp()],
            ).map_err(db)?;
            Ok(())
        })
    }

    /// Open/migrate a database and launch its bounded writer.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, EngineError> {
        let path = path.as_ref().to_owned();
        let location = std::sync::Arc::new(std::path::absolute(&path).map_err(db)?);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(db(e)),
            }
        }
        let (tx, mut rx) = mpsc::channel::<Command>(64);
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new().name("okytes-sqlite".into()).spawn(move || {
            let conn = (|| -> Result<Connection, EngineError> {
                let conn = Connection::open(path).map_err(db)?;
                conn.busy_timeout(std::time::Duration::from_secs(10)).map_err(db)?;
                let journal:String=conn.query_row("PRAGMA journal_mode",[],|r|r.get(0)).map_err(db)?;
                if journal != "wal" {conn.execute_batch("PRAGMA journal_mode=WAL;").map_err(db)?;}
                conn.execute_batch("PRAGMA synchronous=FULL;").map_err(db)?;
                let version:u32=conn.query_row("PRAGMA user_version",[],|r|r.get(0)).map_err(db)?;
                if version==0 {conn.execute_batch("
                    CREATE TABLE IF NOT EXISTS jobs (id TEXT PRIMARY KEY, record TEXT NOT NULL, priority INTEGER NOT NULL, state TEXT NOT NULL);
                    CREATE TABLE IF NOT EXISTS segments (id TEXT PRIMARY KEY, job_id TEXT NOT NULL, record TEXT NOT NULL);
                    CREATE INDEX IF NOT EXISTS segments_job ON segments(job_id);
                    CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                    PRAGMA user_version=1;").map_err(db)?;}
                else if version!=1 {return Err(PersistenceError::Migration(format!("unsupported schema version {version}")).into());}
                conn.execute_batch("CREATE INDEX IF NOT EXISTS jobs_runnable ON jobs(priority DESC) WHERE state IN ('queued','probing','downloading','verifying');
                    CREATE TABLE IF NOT EXISTS torrent_jobs(job_id TEXT PRIMARY KEY REFERENCES jobs(id),descriptor TEXT NOT NULL,metadata BLOB NOT NULL,upload_reserved INTEGER NOT NULL DEFAULT 0);
                    CREATE TABLE IF NOT EXISTS torrent_pieces(job_id TEXT NOT NULL REFERENCES jobs(id),piece INTEGER NOT NULL,PRIMARY KEY(job_id,piece));
                    CREATE TABLE IF NOT EXISTS peer_requests(id TEXT PRIMARY KEY,record TEXT NOT NULL CHECK(length(CAST(record AS BLOB))<=1048576));
                    CREATE TABLE IF NOT EXISTS peer_progress(request_id TEXT NOT NULL,path TEXT NOT NULL,offset INTEGER NOT NULL,PRIMARY KEY(request_id,path));
                    CREATE TABLE IF NOT EXISTS peer_outbound(request_id TEXT PRIMARY KEY,record TEXT NOT NULL CHECK(length(CAST(record AS BLOB))<=32768));
                    CREATE TABLE IF NOT EXISTS peer_inbound(request_id TEXT PRIMARY KEY,destination TEXT NOT NULL CHECK(length(CAST(destination AS BLOB))<=4096));
                    CREATE TABLE IF NOT EXISTS peer_failure(request_id TEXT PRIMARY KEY,message TEXT NOT NULL CHECK(length(CAST(message AS BLOB))<=512));
                    CREATE TABLE IF NOT EXISTS peer_trust(id TEXT PRIMARY KEY,fingerprint TEXT NOT NULL,certificate BLOB NOT NULL CHECK(length(certificate)<=4096),revision INTEGER NOT NULL,revoked INTEGER NOT NULL);
                    CREATE TABLE IF NOT EXISTS api_idempotency(owner TEXT NOT NULL,route TEXT NOT NULL,key TEXT NOT NULL,digest TEXT NOT NULL,response TEXT NOT NULL,created_at INTEGER NOT NULL,PRIMARY KEY(owner,route,key));
                    CREATE INDEX IF NOT EXISTS api_idempotency_expiry ON api_idempotency(created_at);").map_err(db)?;
                Ok(conn)
            })();
            match conn {
                Ok(mut conn) => {
                    let _ = ready_tx.send(Ok(()));
                    while let Some(command) = rx.blocking_recv() { command(&mut conn); }
                }
                Err(error) => { let _ = ready_tx.send(Err(error)); }
            }
        }).map_err(db)?;
        ready_rx.recv().map_err(db)??;
        Ok(Self { tx, path: location })
    }

    /// Prevent two CLI runners from writing the same partial files.
    pub fn acquire_engine(path: &Path) -> Result<EngineLease, EngineError> {
        let resolved = if path.exists() {
            std::fs::canonicalize(path).map_err(db)?
        } else {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            std::fs::canonicalize(parent).map_err(db)?.join(
                path.file_name()
                    .ok_or_else(|| db("database has no filename"))?,
            )
        };
        let mut lock = resolved.as_os_str().to_owned();
        lock.push(".engine-lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock)
            .map_err(db)?;
        file.try_lock().map_err(|e| {
            EngineError::Scheduler(format!("another runner owns this database: {e}"))
        })?;
        Ok(EngineLease { _file: file })
    }

    fn call<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, EngineError> + Send + 'static,
    ) -> Reply<T> {
        let tx = self.tx.clone();
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            tx.send(Box::new(move |conn| {
                let _ = reply.send(operation(conn));
            }))
            .await
            .map_err(db)?;
            result.await.map_err(db)?
        })
    }

    /// Read at most 256 jobs in priority order without loading the entire queue.
    pub fn page(&self, offset: usize) -> Reply<Vec<JobRecord>> {
        self.call(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT record FROM jobs ORDER BY priority DESC, rowid ASC LIMIT 256 OFFSET ?1",
                )
                .map_err(db)?;
            let rows = stmt
                .query_map([offset as i64], |row| row.get::<_, String>(0))
                .map_err(db)?;
            rows.map(|row| serde_json::from_str(&row.map_err(db)?).map_err(db))
                .collect()
        })
    }

    /// Persist validated non-secret configuration.
    pub fn set_config(&self, config: &okytes_core::types::EngineConfig) -> Reply<()> {
        let config = serde_json::to_string(config);
        self.call(move |conn| {
            conn.execute("INSERT INTO settings VALUES ('engine',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [config.map_err(db)?]).map_err(db)?;
            Ok(())
        })
    }

    /// Load configuration, or defaults on first run.
    pub fn config(&self) -> Reply<okytes_core::types::EngineConfig> {
        self.call(|conn| {
            use rusqlite::OptionalExtension;
            let value: Option<String> = conn
                .query_row("SELECT value FROM settings WHERE key='engine'", [], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(db)?;
            value
                .map(|s| serde_json::from_str(&s).map_err(db))
                .transpose()
                .map(|c| c.unwrap_or_default())
        })
    }
}

fn save_job(conn: &Connection, job: &JobRecord) -> Result<(), EngineError> {
    // Explicit session headers are never serialized. Job URLs are persisted
    // because the user explicitly adds them to the durable queue.
    let mut persisted = job.clone();
    persisted.headers.clear();
    conn.execute("INSERT INTO jobs VALUES (?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET record=excluded.record,priority=excluded.priority,state=excluded.state",
        params![job.id.to_string(), serde_json::to_string(&persisted).map_err(db)?, job.priority, job.state.to_string()]).map_err(db)?;
    Ok(())
}

fn save_segment(conn: &Connection, segment: &SegmentRecord) -> Result<(), EngineError> {
    if segment
        .end
        .checked_sub(segment.start)
        .and_then(|n| n.checked_add(1))
        .is_none_or(|length| segment.bytes_downloaded > length)
    {
        return Err(PersistenceError::Inconsistent("invalid segment checkpoint".into()).into());
    }
    conn.execute("INSERT INTO segments VALUES (?1,?2,?3) ON CONFLICT(id) DO UPDATE SET record=excluded.record",
        params![segment.id.to_string(), segment.job_id.to_string(), serde_json::to_string(segment).map_err(db)?]).map_err(db)?;
    Ok(())
}

impl JobStore for SqliteStore {
    fn record_diagnostic(&self, id: JobId, error: Option<String>, retry: bool) -> Reply<()> {
        self.call(move |conn| {
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(db)?;
            let record: String = tx
                .query_row(
                    "SELECT record FROM jobs WHERE id=?1",
                    [id.to_string()],
                    |r| r.get(0),
                )
                .map_err(db)?;
            let mut job: JobRecord = serde_json::from_str(&record).map_err(db)?;
            job.error_message = error;
            job.retry_count = job.retry_count.saturating_add(u32::from(retry));
            job.updated_at = chrono::Utc::now();
            save_job(&tx, &job)?;
            tx.commit().map_err(db)
        })
    }
    fn transition_job(&self, id: JobId, state: JobState) -> Reply<JobRecord> {
        self.call(move |conn| {
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(db)?;
            let record: String = tx
                .query_row(
                    "SELECT record FROM jobs WHERE id=?1",
                    [id.to_string()],
                    |r| r.get(0),
                )
                .map_err(|_| EngineError::JobNotFound(id))?;
            let mut job: JobRecord = serde_json::from_str(&record).map_err(db)?;
            if job.state == JobState::Verifying
                && job.verified_sha256.is_some()
                && matches!(state, JobState::Paused | JobState::Cancelled)
            {
                return Err(EngineError::Scheduler(
                    "atomic publication in progress; retry control after it finishes".into(),
                ));
            }
            job.state = job.state.transition(state, id)?;
            if matches!(state, JobState::Completed | JobState::Queued) {
                job.error_message = None;
            }
            job.updated_at = chrono::Utc::now();
            save_job(&tx, &job)?;
            tx.commit().map_err(db)?;
            Ok(job)
        })
    }
    fn save_job(&self, job: &JobRecord) -> Reply<()> {
        let job = job.clone();
        self.call(move |conn| {
            use rusqlite::OptionalExtension;
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(db)?;
            let previous: Option<String> = tx
                .query_row(
                    "SELECT record FROM jobs WHERE id=?1",
                    [job.id.to_string()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(db)?;
            if let Some(previous) = previous {
                let previous: JobRecord = serde_json::from_str(&previous).map_err(db)?;
                if previous.state != job.state {
                    return Err(EngineError::Scheduler(
                        "job state changed during metadata update".into(),
                    ));
                }
            }
            save_job(&tx, &job)?;
            tx.commit().map_err(db)
        })
    }
    fn load_job(&self, id: JobId) -> Reply<Option<JobRecord>> {
        self.call(move |conn| {
            use rusqlite::OptionalExtension;
            let record: Option<String> = conn
                .query_row(
                    "SELECT record FROM jobs WHERE id=?1",
                    [id.to_string()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(db)?;
            record
                .map(|s| serde_json::from_str(&s).map_err(db))
                .transpose()
        })
    }
    fn list_jobs(&self, filter: Option<JobState>) -> Reply<Vec<JobRecord>> {
        self.call(move |conn| {
            let mut stmt = conn.prepare("SELECT record FROM jobs WHERE (?1 IS NULL OR state=?1) ORDER BY priority DESC,rowid LIMIT 256").map_err(db)?;
            let rows = stmt.query_map([filter.map(|s| s.to_string())], |r| r.get::<_, String>(0)).map_err(db)?;
            rows.map(|row| serde_json::from_str(&row.map_err(db)?).map_err(db)).collect()
        })
    }
    fn save_segments(&self, segments: &[SegmentRecord]) -> Reply<()> {
        let segments = segments.to_vec();
        self.call(move |conn| {
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(db)?;
            for segment in &segments {
                save_segment(&tx, segment)?;
            }
            tx.commit().map_err(db)
        })
    }
    fn load_segments(&self, id: JobId) -> Reply<Vec<SegmentRecord>> {
        self.call(move |conn| {
            let mut stmt = conn
                .prepare("SELECT record FROM segments WHERE job_id=?1 LIMIT 1025")
                .map_err(db)?;
            let rows = stmt
                .query_map([id.to_string()], |r| r.get::<_, String>(0))
                .map_err(db)?;
            let mut segments: Vec<SegmentRecord> = rows
                .map(|r| serde_json::from_str(&r.map_err(db)?).map_err(db))
                .collect::<Result<_, _>>()?;
            segments.sort_by_key(|s| s.start);
            if segments.len() > 1024 {
                return Err(PersistenceError::Inconsistent(
                    "segment count exceeds resource limit".into(),
                )
                .into());
            }
            Ok(segments)
        })
    }
    fn checkpoint_segment(&self, segment: &SegmentRecord) -> Reply<()> {
        let segment = segment.clone();
        self.call(move |conn| save_segment(conn, &segment))
    }
    fn delete_job(&self, id: JobId) -> Reply<()> {
        self.call(move |conn| {
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(db)?;
            tx.execute(
                "DELETE FROM torrent_pieces WHERE job_id=?1",
                [id.to_string()],
            )
            .map_err(db)?;
            tx.execute("DELETE FROM torrent_jobs WHERE job_id=?1", [id.to_string()])
                .map_err(db)?;
            tx.execute("DELETE FROM segments WHERE job_id=?1", [id.to_string()])
                .map_err(db)?;
            tx.execute("DELETE FROM jobs WHERE id=?1", [id.to_string()])
                .map_err(db)?;
            tx.commit().map_err(db)
        })
    }
}
