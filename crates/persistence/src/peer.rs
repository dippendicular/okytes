//! Peer offer admission in the existing profile writer. No second database,
//! engine, filesystem authority or transport is created here. Callers must hold
//! the profile engine lease and establish peer authentication separately.
use crate::{db, Reply, SqliteStore};
use okytes_core::peer::{Direction, Entry, Hash, Manifest, State, MAX_CHUNK};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

/// Native trust slot; a revision binds explicit approval to one certificate.
/// No caller may infer trust merely from an unauthenticated discovery record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trust {
    /// Locally assigned stable slot, not a remote-advertised identity.
    pub id: Uuid,
    /// Full certificate commitment approved by the native user.
    pub fingerprint: Hash,
    /// Public certificate DER; private key material never enters this table.
    pub certificate: Vec<u8>,
    /// Monotonic decision revision, including revocations and replacements.
    pub revision: u64,
    /// Revoked slots remain tombstones until explicitly approved again.
    pub revoked: bool,
}

fn read_trust(
    conn: &Connection,
    id: Uuid,
) -> Result<Option<Trust>, okytes_core::error::EngineError> {
    let row = conn
        .query_row(
            "SELECT fingerprint,certificate,revision,revoked FROM peer_trust WHERE id=?1",
            [id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, u64>(2)?,
                    row.get::<_, bool>(3)?,
                ))
            },
        )
        .optional()
        .map_err(db)?;
    row.map(|(fingerprint, certificate, revision, revoked)| {
        let fingerprint = Hash::try_from(fingerprint).map_err(db)?;
        if certificate.is_empty()
            || certificate.len() > 4096
            || Hash::digest(&certificate) != fingerprint
            || revision > i64::MAX as u64
        {
            return Err(db("invalid peer trust record"));
        }
        Ok(Trust {
            id,
            fingerprint,
            certificate,
            revision,
            revoked,
        })
    })
    .transpose()
}

/// Immutable offer plus its revisioned lifecycle; contains no local source path,
/// credential or receiver destination. A fingerprint alone does not imply trust.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Random request identifier, globally bound to this immutable offer.
    pub id: Uuid,
    /// Authenticated remote identity commitment, supplied by the trusted adapter.
    pub peer: Hash,
    /// Local inbound/outbound role.
    pub direction: Direction,
    /// Ordered relative metadata; never absolute filesystem authority.
    pub manifest: Manifest,
    /// Domain-separated immutable metadata commitment.
    pub commitment: Hash,
    /// Peer-specific lifecycle; no speculative byte progress is persisted.
    pub state: State,
    /// Monotonic optimistic-concurrency revision.
    pub revision: u64,
}

/// Local-only information needed to reopen an interrupted outbound source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outbound {
    pub request_id: Uuid,
    pub peer_id: Uuid,
    pub address: String,
    pub paths: Vec<String>,
}

fn read(conn: &Connection, id: Uuid) -> Result<Option<Request>, okytes_core::error::EngineError> {
    let text: Option<String> = conn
        .query_row(
            "SELECT record FROM peer_requests WHERE id=?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(db)?;
    text.map(|text| {
        let request: Request =
            serde_json::from_str(&text).map_err(|_| db("invalid stored peer record"))?;
        if request.id != id
            || request.revision > i64::MAX as u64
            || request.manifest.commitment().map_err(db)? != request.commitment
        {
            return Err(db("invalid stored peer commitment"));
        }
        Ok(request)
    })
    .transpose()
}

impl SqliteStore {
    /// Save bounded local source authority before the sender task starts.
    pub fn peer_save_outbound(&self, outbound: Outbound) -> Reply<()> {
        self.call(move |conn| {
            if outbound.request_id.is_nil()
                || outbound.peer_id.is_nil()
                || outbound.address.len() > 255
                || outbound.paths.is_empty()
                || outbound.paths.len() > 32
                || outbound.paths.iter().any(|path| path.is_empty() || path.len() > 4096)
            {
                return Err(db("invalid outbound peer source"));
            }
            let request = read(conn, outbound.request_id)?
                .ok_or_else(|| db("peer request not found"))?;
            if request.direction != Direction::Outbound {
                return Err(db("peer request is not outbound"));
            }
            let text = serde_json::to_string(&outbound).map_err(db)?;
            conn.execute(
                "INSERT INTO peer_outbound(request_id,record) VALUES (?1,?2) ON CONFLICT(request_id) DO UPDATE SET record=excluded.record",
                params![outbound.request_id.to_string(), text],
            )
            .map_err(db)?;
            Ok(())
        })
    }

    /// Load all resumable outbound tasks; the global request bound is 4096.
    pub fn peer_outbounds(&self) -> Reply<Vec<Outbound>> {
        self.call(move |conn| {
            let mut statement = conn
                .prepare("SELECT record FROM peer_outbound ORDER BY rowid LIMIT 4096")
                .map_err(db)?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(db)?;
            rows.map(|row| serde_json::from_str(&row.map_err(db)?).map_err(db))
                .collect()
        })
    }

    pub fn peer_remove_outbound(&self, request_id: Uuid) -> Reply<()> {
        self.call(move |conn| {
            conn.execute(
                "DELETE FROM peer_outbound WHERE request_id=?1",
                [request_id.to_string()],
            )
            .map_err(db)?;
            Ok(())
        })
    }

    pub fn peer_set_inbound_destination(&self, request_id: Uuid, destination: String) -> Reply<()> {
        self.call(move |conn| {
            if destination.is_empty() || destination.len() > 4096 {
                return Err(db("invalid inbound destination"));
            }
            let request = read(conn, request_id)?.ok_or_else(|| db("peer request not found"))?;
            if request.direction != Direction::Inbound || request.state != State::AwaitingApproval {
                return Err(db("peer request cannot select a destination"));
            }
            conn.execute(
                "INSERT INTO peer_inbound(request_id,destination) VALUES (?1,?2) ON CONFLICT(request_id) DO UPDATE SET destination=excluded.destination",
                params![request_id.to_string(), destination],
            )
            .map_err(db)?;
            Ok(())
        })
    }

    pub fn peer_inbound_destination(&self, request_id: Uuid) -> Reply<Option<String>> {
        self.call(move |conn| {
            conn.query_row(
                "SELECT destination FROM peer_inbound WHERE request_id=?1",
                [request_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(db)
        })
    }

    pub fn peer_set_failure(&self, request_id: Uuid, message: String) -> Reply<()> {
        self.call(move |conn| {
            if message.is_empty() || message.len() > 512 || read(conn, request_id)?.is_none() {
                return Err(db("invalid peer failure"));
            }
            conn.execute(
                "INSERT INTO peer_failure(request_id,message) VALUES (?1,?2) ON CONFLICT(request_id) DO UPDATE SET message=excluded.message",
                params![request_id.to_string(), message],
            )
            .map_err(db)?;
            Ok(())
        })
    }

    pub fn peer_failure(&self, request_id: Uuid) -> Reply<Option<String>> {
        self.call(move |conn| {
            conn.query_row(
                "SELECT message FROM peer_failure WHERE request_id=?1",
                [request_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(db)
        })
    }
    /// Read current native trust, including revocation tombstones. A future peer
    /// adapter must check this before connections and privileged operations.
    pub fn peer_trust(&self, id: Uuid) -> Reply<Option<Trust>> {
        self.call(move |conn| read_trust(conn, id))
    }
    /// List the bounded active trust set used to build an incoming TLS snapshot.
    /// Callers still recheck the authenticated fingerprint before each command.
    pub fn peer_active_trusts(&self) -> Reply<Vec<Trust>> {
        self.call(move |conn| {
            let mut statement = conn
                .prepare("SELECT id FROM peer_trust WHERE revoked=0 ORDER BY id LIMIT 65")
                .map_err(db)?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(db)?;
            let mut result = Vec::new();
            for row in rows {
                let id = Uuid::parse_str(&row.map_err(db)?).map_err(db)?;
                result.push(read_trust(conn, id)?.ok_or_else(|| db("peer trust missing"))?);
            }
            if result.len() > 64 {
                return Err(db("peer trust registry exceeds bound"));
            }
            Ok(result)
        })
    }
    /// List all bounded trust slots, including revocation tombstones.
    pub fn peer_trusts(&self) -> Reply<Vec<Trust>> {
        self.call(move |conn| {
            let mut statement = conn
                .prepare("SELECT id FROM peer_trust ORDER BY id LIMIT 65")
                .map_err(db)?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(db)?;
            let mut result = Vec::new();
            for row in rows {
                let id = Uuid::parse_str(&row.map_err(db)?).map_err(db)?;
                result.push(read_trust(conn, id)?.ok_or_else(|| db("peer trust missing"))?);
            }
            if result.len() > 64 {
                return Err(db("peer trust registry exceeds bound"));
            }
            Ok(result)
        })
    }
    /// Resolve an authenticated certificate to its current non-revoked slot.
    pub fn peer_trust_by_fingerprint(&self, fingerprint: Hash) -> Reply<Option<Trust>> {
        self.call(move |conn| {
            let mut statement = conn
                .prepare("SELECT id FROM peer_trust WHERE fingerprint=?1 AND revoked=0 LIMIT 2")
                .map_err(db)?;
            let rows = statement
                .query_map([fingerprint.as_str()], |row| row.get::<_, String>(0))
                .map_err(db)?;
            let ids = rows.collect::<Result<Vec<_>, _>>().map_err(db)?;
            if ids.len() > 1 {
                return Err(db("duplicate active peer identity"));
            }
            ids.first()
                .map(|id| {
                    let id = Uuid::parse_str(id).map_err(db)?;
                    read_trust(conn, id)?.ok_or_else(|| db("peer trust missing"))
                })
                .transpose()
        })
    }
    /// Persist an explicitly approved certificate. None expects a new local
    /// slot; replacements/re-pairing require its exact current revision. This
    /// internal method is never callable with browser/remote credentials alone.
    /// Certificate parsing and proof of possession belong to the TLS adapter.
    pub fn peer_approve(
        &self,
        id: Uuid,
        certificate: Vec<u8>,
        confirmed: Hash,
        expected: Option<u64>,
    ) -> Reply<Trust> {
        self.call(move|conn|{
            if id.is_nil() || certificate.is_empty() || certificate.len()>4096 || Hash::digest(&certificate)!=confirmed {return Err(db("peer fingerprint approval does not match"));}
            let tx=conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(db)?;
            let previous=read_trust(&tx,id)?;
            if previous.as_ref().map(|record|record.revision)!=expected {return Err(db("peer trust changed; review identity again"));}
            let duplicate:Option<String>=tx.query_row("SELECT id FROM peer_trust WHERE fingerprint=?1 AND revoked=0 AND id<>?2 LIMIT 1",params![confirmed.as_str(),id.to_string()],|row|row.get(0)).optional().map_err(db)?;
            if duplicate.is_some() {return Err(db("peer identity is already approved"));}
            let revision=match previous {
                Some(record)=>record.revision.checked_add(1).filter(|n|*n<=i64::MAX as u64).ok_or_else(||db("peer trust revision exhausted"))?,
                None=>{
                    let count:u64=tx.query_row("SELECT count(*) FROM peer_trust",[],|row|row.get(0)).map_err(db)?;
                    if count>=64 {return Err(db("peer trust registry is full"));} 0
                }
            };
            tx.execute("INSERT INTO peer_trust(id,fingerprint,certificate,revision,revoked) VALUES (?1,?2,?3,?4,0) ON CONFLICT(id) DO UPDATE SET fingerprint=excluded.fingerprint,certificate=excluded.certificate,revision=excluded.revision,revoked=0",params![id.to_string(),confirmed.as_str(),certificate,revision]).map_err(db)?;
            let result=read_trust(&tx,id)?.ok_or_else(||db("peer trust missing"))?;
            tx.commit().map_err(db)?;Ok(result)
        })
    }
    /// Revoke one native trust slot transactionally. Repeating the immediately
    /// committed revoke is idempotent; stale decisions cannot revoke a new pin.
    pub fn peer_revoke(&self, id: Uuid, expected: u64) -> Reply<Trust> {
        self.call(move |conn| {
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(db)?;
            let mut record = read_trust(&tx, id)?.ok_or_else(|| db("peer trust not found"))?;
            if record.revoked
                && (record.revision == expected || expected.checked_add(1) == Some(record.revision))
            {
                return Ok(record);
            }
            if record.revision != expected {
                return Err(db("peer trust changed; refresh before revoking"));
            }
            record.revision = record
                .revision
                .checked_add(1)
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or_else(|| db("peer trust revision exhausted"))?;
            record.revoked = true;
            tx.execute(
                "UPDATE peer_trust SET revoked=1,revision=?2 WHERE id=?1",
                params![id.to_string(), record.revision],
            )
            .map_err(db)?;
            tx.commit().map_err(db)?;
            Ok(record)
        })
    }
    /// Transactionally admit an offer, replaying an identical request without
    /// resetting its approval or lifecycle. Reusing an ID with changed identity,
    /// direction or manifest fails. At most 4096 offers may occupy a profile.
    pub fn peer_offer(
        &self,
        id: Uuid,
        peer: Hash,
        direction: Direction,
        manifest: Manifest,
    ) -> Reply<Request> {
        self.call(move |conn| {
            if id.is_nil() {
                return Err(db("invalid peer request ID"));
            }
            let commitment = manifest.commitment().map_err(db)?;
            let tx = conn.transaction().map_err(db)?;
            if let Some(existing) = read(&tx, id)? {
                if existing.peer != peer
                    || existing.direction != direction
                    || existing.commitment != commitment
                {
                    return Err(db("peer request ID is bound to another offer"));
                }
                return Ok(existing);
            }
            let count: u64 = tx
                .query_row("SELECT count(*) FROM peer_requests", [], |row| row.get(0))
                .map_err(db)?;
            if count >= 4096 {
                return Err(db("peer request history is full"));
            }
            let request = Request {
                id,
                peer,
                direction,
                manifest,
                commitment,
                state: match direction {
                    Direction::Inbound => State::AwaitingApproval,
                    Direction::Outbound => State::Created,
                },
                revision: 0,
            };
            let text = serde_json::to_string(&request).map_err(db)?;
            tx.execute(
                "INSERT INTO peer_requests(id,record) VALUES (?1,?2)",
                params![id.to_string(), text],
            )
            .map_err(db)?;
            tx.commit().map_err(db)?;
            Ok(request)
        })
    }

    /// Read one immutable offer and its current lifecycle through the same
    /// serialized writer as HTTP jobs. Missing IDs return None.
    pub fn peer_request(&self, id: Uuid) -> Reply<Option<Request>> {
        self.call(move |conn| read(conn, id))
    }

    /// Bounded history page, at most 64 metadata-only records per request.
    pub fn peer_requests(&self, offset: usize) -> Reply<Vec<Request>> {
        self.call(move |conn| {
            if offset > 4096 {
                return Err(db("invalid peer history offset"));
            }
            let mut query = conn
                .prepare("SELECT id FROM peer_requests ORDER BY rowid LIMIT 64 OFFSET ?1")
                .map_err(db)?;
            let ids = query
                .query_map([offset], |row| row.get::<_, String>(0))
                .map_err(db)?;
            let mut records = Vec::new();
            for id in ids {
                let id = Uuid::parse_str(&id.map_err(db)?).map_err(db)?;
                records.push(read(conn, id)?.ok_or_else(|| db("missing peer request"))?);
            }
            Ok(records)
        })
    }

    /// Return durable received offsets after validating them against the
    /// immutable manifest. Missing files have an implicit zero offset.
    pub fn peer_offsets(&self, id: Uuid, peer: Hash) -> Reply<BTreeMap<String, u64>> {
        self.call(move |conn| {
            let request = read(conn, id)?.ok_or_else(|| db("peer request not found"))?;
            if request.peer != peer {
                return Err(db("peer identity does not match request"));
            }
            let sizes: BTreeMap<&str, u64> = request
                .manifest
                .entries
                .iter()
                .filter_map(|entry| match entry {
                    Entry::File { path, size, .. } => Some((path.as_str(), *size)),
                    Entry::Directory { .. } => None,
                })
                .collect();
            let mut statement = conn
                .prepare("SELECT path,offset FROM peer_progress WHERE request_id=?1 ORDER BY path")
                .map_err(db)?;
            let rows = statement
                .query_map([id.to_string()], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
                })
                .map_err(db)?;
            let mut offsets = BTreeMap::new();
            for row in rows {
                let (path, offset) = row.map_err(db)?;
                if sizes.get(path.as_str()).is_none_or(|size| offset > *size) {
                    return Err(db("invalid peer progress record"));
                }
                offsets.insert(path, offset);
            }
            Ok(offsets)
        })
    }

    /// Commit a received chunk only after its file bytes were synced. Exact
    /// offset compare-and-swap makes duplicate delivery idempotent and prevents
    /// gaps, overlap, or stale sessions from advancing durable progress.
    pub fn peer_checkpoint(
        &self,
        id: Uuid,
        peer: Hash,
        path: String,
        expected: u64,
        next: u64,
    ) -> Reply<u64> {
        self.call(move |conn| {
            if next <= expected || next - expected > MAX_CHUNK as u64 {
                return Err(db("invalid peer checkpoint range"));
            }
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(db)?;
            let request = read(&tx, id)?.ok_or_else(|| db("peer request not found"))?;
            if request.peer != peer || request.state != State::Transferring {
                return Err(db("peer transfer is not authorized to write"));
            }
            let size = request
                .manifest
                .entries
                .iter()
                .find_map(|entry| match entry {
                    Entry::File { path: item, size, .. } if item == &path => Some(*size),
                    _ => None,
                })
                .ok_or_else(|| db("peer checkpoint path is not a file"))?;
            if next > size {
                return Err(db("peer checkpoint exceeds manifest"));
            }
            let current: Option<u64> = tx
                .query_row(
                    "SELECT offset FROM peer_progress WHERE request_id=?1 AND path=?2",
                    params![id.to_string(), path],
                    |row| row.get(0),
                )
                .optional()
                .map_err(db)?;
            let current = current.unwrap_or(0);
            if current == next {
                return Ok(current);
            }
            if current != expected {
                return Err(db("peer checkpoint changed; resume from durable offset"));
            }
            tx.execute(
                "INSERT INTO peer_progress(request_id,path,offset) VALUES (?1,?2,?3) ON CONFLICT(request_id,path) DO UPDATE SET offset=excluded.offset",
                params![id.to_string(), path, next],
            )
            .map_err(db)?;
            tx.commit().map_err(db)?;
            Ok(next)
        })
    }

    /// Compare-and-swap a trusted local lifecycle decision. Repeating exactly
    /// the preceding decision returns its committed result; stale or conflicting
    /// decisions fail. This internal method grants no network authorization.
    /// Only an executor that proved integrity may request Completed.
    pub fn peer_transition(
        &self,
        id: Uuid,
        peer: Hash,
        expected_revision: u64,
        next: State,
    ) -> Reply<Request> {
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db)?;
            let mut request = read(&tx, id)?.ok_or_else(|| db("peer request not found"))?;
            if request.peer != peer {
                return Err(db("peer identity does not match request"));
            }
            if request.revision != expected_revision {
                if expected_revision.checked_add(1) == Some(request.revision)
                    && request.state == next
                {
                    return Ok(request);
                }
                return Err(db("peer request changed; refresh before deciding"));
            }
            request.state.transition(next).map_err(db)?;
            if request.state == next {
                return Ok(request);
            }
            request.state = next;
            request.revision = request
                .revision
                .checked_add(1)
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or_else(|| db("peer revision exhausted"))?;
            let text = serde_json::to_string(&request).map_err(db)?;
            tx.execute(
                "UPDATE peer_requests SET record=?2 WHERE id=?1",
                params![id.to_string(), text],
            )
            .map_err(db)?;
            tx.commit().map_err(db)?;
            Ok(request)
        })
    }
}
