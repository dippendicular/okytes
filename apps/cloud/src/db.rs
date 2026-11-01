//! SQLite persistence. One connection behind a mutex is plenty for a
//! self-hosted server; every statement is short and parameterized.
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Mutex};

pub struct Db(Mutex<Connection>);

const SCHEMA: &str = "
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;
CREATE TABLE IF NOT EXISTS users (
  id TEXT PRIMARY KEY, email TEXT NOT NULL UNIQUE, password TEXT,
  github_id TEXT UNIQUE, google_sub TEXT UNIQUE, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS sessions (
  token_hash BLOB PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  device_id TEXT REFERENCES devices(id) ON DELETE CASCADE, expires_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS devices (
  id TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name TEXT NOT NULL, kind TEXT NOT NULL, created_at INTEGER NOT NULL, last_seen INTEGER);
CREATE TABLE IF NOT EXISTS pair_codes (
  code_hash BLOB PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  expires_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS email_tokens (
  token_hash BLOB PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  purpose TEXT NOT NULL, expires_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS oauth_states (
  state TEXT PRIMARY KEY, provider TEXT NOT NULL, verifier TEXT NOT NULL, expires_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS queue (
  id TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  target_device TEXT REFERENCES devices(id) ON DELETE CASCADE, url TEXT NOT NULL,
  filename TEXT, state TEXT NOT NULL, claimed_by TEXT, job_id TEXT, error TEXT,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS queue_pending ON queue(user_id, state);
CREATE TABLE IF NOT EXISTS history (
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE, job_id TEXT NOT NULL,
  name TEXT NOT NULL, source TEXT NOT NULL, state TEXT NOT NULL, bytes REAL NOT NULL,
  total REAL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, synced_at INTEGER NOT NULL,
  PRIMARY KEY (user_id, device_id, job_id));
";

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
pub fn digest(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
}
pub fn random_hex(bytes: usize) -> String {
    (0..bytes)
        .map(|_| format!("{:02x}", rand::random::<u8>()))
        .collect()
}

#[derive(Serialize, Clone)]
pub struct User {
    pub id: String,
    pub email: String,
    pub github: bool,
    pub google: bool,
    pub password: bool,
}
#[derive(Serialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub created_at: i64,
    pub last_seen: Option<i64>,
}
#[derive(Serialize)]
pub struct QueueItem {
    pub id: String,
    pub target_device: Option<String>,
    pub url: String,
    pub filename: Option<String>,
    pub state: String,
    pub claimed_by: Option<String>,
    pub job_id: Option<String>,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}
#[derive(Serialize, serde::Deserialize)]
pub struct HistoryEntry {
    pub job_id: String,
    pub name: String,
    pub source: String,
    pub state: String,
    pub bytes: f64,
    pub total: Option<f64>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub device_name: Option<String>,
}
/// Authenticated principal: a dashboard session or a paired device.
#[derive(Clone)]
pub struct Principal {
    pub user: User,
    pub device: Option<String>,
}

fn user(row: &rusqlite::Row) -> rusqlite::Result<User> {
    Ok(User {
        id: row.get(0)?,
        email: row.get(1)?,
        password: row.get::<_, Option<String>>(2)?.is_some(),
        github: row.get::<_, Option<String>>(3)?.is_some(),
        google: row.get::<_, Option<String>>(4)?.is_some(),
    })
}
const USER: &str = "SELECT id, email, password, github_id, google_sub FROM users";

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path).context("open cloud database")?;
        connection.execute_batch(SCHEMA)?;
        // 0.0.2 added email verification; existing accounts count as verified.
        let has_verified: bool = connection
            .prepare("SELECT 1 FROM pragma_table_info('users') WHERE name = 'verified'")?
            .exists([])?;
        if !has_verified {
            connection.execute_batch(
                "ALTER TABLE users ADD COLUMN verified INTEGER NOT NULL DEFAULT 1;",
            )?;
        }
        Ok(Self(Mutex::new(connection)))
    }
    fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        let connection = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("database lock poisoned"))?;
        Ok(f(&connection)?)
    }

    pub fn user_count(&self) -> Result<i64> {
        self.with(|c| c.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0)))
    }
    pub fn user_by_email(&self, email: &str) -> Result<Option<(User, Option<String>)>> {
        self.with(|c| {
            c.query_row(&format!("{USER} WHERE email = ?1"), [email], |r| {
                Ok((user(r)?, r.get(2)?))
            })
            .optional()
        })
    }
    pub fn create_user(&self, email: &str, password: Option<&str>) -> Result<User> {
        self.create_user_verified(email, password, true)
    }
    pub fn create_user_verified(
        &self,
        email: &str,
        password: Option<&str>,
        verified: bool,
    ) -> Result<User> {
        let id = uuid::Uuid::new_v4().to_string();
        self.with(|c| {
            c.execute(
                "INSERT INTO users (id, email, password, created_at, verified) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, email, password, now(), verified],
            )
        })?;
        Ok(User {
            id,
            email: email.into(),
            github: false,
            google: false,
            password: password.is_some(),
        })
    }
    /// Find by provider subject, else link to the verified email, else create.
    pub fn oauth_user(&self, provider: &str, subject: &str, email: &str) -> Result<User> {
        let column = if provider == "github" {
            "github_id"
        } else {
            "google_sub"
        };
        if let Some(found) = self.with(|c| {
            c.query_row(&format!("{USER} WHERE {column} = ?1"), [subject], user)
                .optional()
        })? {
            return Ok(found);
        }
        if self.user_by_email(email)?.is_none() {
            self.create_user(email, None)?;
        }
        self.with(|c| {
            c.execute(
                &format!("UPDATE users SET {column} = ?1 WHERE email = ?2"),
                params![subject, email],
            )
        })?;
        Ok(self
            .user_by_email(email)?
            .context("linked user vanished")?
            .0)
    }

    pub fn is_verified(&self, user: &str) -> Result<bool> {
        self.with(|c| {
            c.query_row("SELECT verified FROM users WHERE id = ?1", [user], |r| {
                r.get(0)
            })
        })
    }
    /// Single-use email token for `verify` or `reset`, valid for one hour.
    pub fn email_token(&self, user: &str, purpose: &str) -> Result<String> {
        let token = random_hex(32);
        self.with(|c| {
            c.execute(
                "DELETE FROM email_tokens WHERE user_id = ?1 AND purpose = ?2",
                params![user, purpose],
            )?;
            c.execute(
                "INSERT INTO email_tokens (token_hash, user_id, purpose, expires_at) VALUES (?1, ?2, ?3, ?4)",
                params![digest(&token), user, purpose, now() + 3600],
            )
        })?;
        Ok(token)
    }
    /// Consume a token; returns its user when valid for this purpose.
    pub fn take_email_token(&self, token: &str, purpose: &str) -> Result<Option<String>> {
        self.with(|c| {
            let user = c
                .query_row(
                    "SELECT user_id FROM email_tokens WHERE token_hash = ?1 AND purpose = ?2 AND expires_at > ?3",
                    params![digest(token), purpose, now()],
                    |r| r.get::<_, String>(0),
                )
                .optional()?;
            c.execute("DELETE FROM email_tokens WHERE token_hash = ?1", [digest(token)])?;
            Ok(user)
        })
    }
    pub fn mark_verified(&self, user: &str) -> Result<()> {
        self.with(|c| c.execute("UPDATE users SET verified = 1 WHERE id = ?1", [user]))?;
        Ok(())
    }
    /// Set a new password and end every dashboard session. Paired devices keep
    /// their own tokens (not derived from the password); remove them in the
    /// dashboard if a device itself is lost.
    pub fn reset_password(&self, user: &str, hash: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE users SET password = ?1, verified = 1 WHERE id = ?2",
                params![hash, user],
            )?;
            c.execute(
                "DELETE FROM sessions WHERE user_id = ?1 AND device_id IS NULL",
                [user],
            )
        })?;
        Ok(())
    }
    pub fn create_session(&self, user: &str, device: Option<&str>, days: i64) -> Result<String> {
        let token = random_hex(32);
        self.with(|c| {
            c.execute(
                "INSERT INTO sessions (token_hash, user_id, device_id, expires_at) VALUES (?1, ?2, ?3, ?4)",
                params![digest(&token), user, device, now() + days * 86400],
            )
        })?;
        Ok(token)
    }
    pub fn session(&self, token: &str) -> Result<Option<Principal>> {
        let found = self.with(|c| {
            c.query_row(
                "SELECT u.id, u.email, u.password, u.github_id, u.google_sub, s.device_id
                 FROM sessions s JOIN users u ON u.id = s.user_id
                 WHERE s.token_hash = ?1 AND s.expires_at > ?2",
                params![digest(token), now()],
                |r| {
                    Ok(Principal {
                        user: user(r)?,
                        device: r.get(5)?,
                    })
                },
            )
            .optional()
        })?;
        if let Some(device) = found.as_ref().and_then(|p| p.device.as_ref()) {
            self.with(|c| {
                c.execute(
                    "UPDATE devices SET last_seen = ?1 WHERE id = ?2",
                    params![now(), device],
                )
            })?;
        }
        Ok(found)
    }
    pub fn delete_session(&self, token: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "DELETE FROM sessions WHERE token_hash = ?1",
                [digest(token)],
            )
        })?;
        Ok(())
    }
    pub fn prune(&self) -> Result<()> {
        let t = now();
        self.with(|c| {
            c.execute("DELETE FROM sessions WHERE expires_at <= ?1", [t])?;
            c.execute("DELETE FROM pair_codes WHERE expires_at <= ?1", [t])?;
            c.execute("DELETE FROM email_tokens WHERE expires_at <= ?1", [t])?;
            c.execute("DELETE FROM oauth_states WHERE expires_at <= ?1", [t])
        })?;
        Ok(())
    }

    pub fn save_state(&self, state: &str, provider: &str, verifier: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO oauth_states (state, provider, verifier, expires_at) VALUES (?1, ?2, ?3, ?4)",
                params![state, provider, verifier, now() + 600],
            )
        })?;
        Ok(())
    }
    /// One-time state lookup; the row is deleted whether or not it matches.
    pub fn take_state(&self, state: &str, provider: &str) -> Result<Option<String>> {
        self.with(|c| {
            let verifier = c
                .query_row(
                    "SELECT verifier FROM oauth_states WHERE state = ?1 AND provider = ?2 AND expires_at > ?3",
                    params![state, provider, now()],
                    |r| r.get(0),
                )
                .optional()?;
            c.execute("DELETE FROM oauth_states WHERE state = ?1", [state])?;
            Ok(verifier)
        })
    }

    pub fn create_pair_code(&self, user: &str) -> Result<String> {
        // 8 base32-like characters, readable on a phone and short-lived.
        const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
        let code: String = (0..8)
            .map(|_| ALPHABET[rand::random::<usize>() % ALPHABET.len()] as char)
            .collect();
        self.with(|c| {
            c.execute(
                "INSERT INTO pair_codes (code_hash, user_id, expires_at) VALUES (?1, ?2, ?3)",
                params![digest(&code), user, now() + 600],
            )
        })?;
        Ok(code)
    }
    /// Consume a pairing code, register the device and issue its token.
    pub fn claim_pair_code(
        &self,
        code: &str,
        name: &str,
        kind: &str,
    ) -> Result<Option<(String, String)>> {
        let code = code.trim().to_ascii_uppercase();
        let user: Option<String> = self.with(|c| {
            let user = c
                .query_row(
                    "SELECT user_id FROM pair_codes WHERE code_hash = ?1 AND expires_at > ?2",
                    params![digest(&code), now()],
                    |r| r.get(0),
                )
                .optional()?;
            c.execute(
                "DELETE FROM pair_codes WHERE code_hash = ?1",
                [digest(&code)],
            )?;
            Ok(user)
        })?;
        let Some(user) = user else { return Ok(None) };
        let device = uuid::Uuid::new_v4().to_string();
        self.with(|c| {
            c.execute(
                "INSERT INTO devices (id, user_id, name, kind, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![device, user, name, kind, now()],
            )
        })?;
        let token = self.create_session(&user, Some(&device), 365)?;
        Ok(Some((device, token)))
    }
    pub fn devices(&self, user: &str) -> Result<Vec<Device>> {
        self.with(|c| {
            let mut statement = c.prepare(
                "SELECT id, name, kind, created_at, last_seen FROM devices WHERE user_id = ?1 ORDER BY created_at",
            )?;
            let rows = statement.query_map([user], |r| {
                Ok(Device {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    kind: r.get(2)?,
                    created_at: r.get(3)?,
                    last_seen: r.get(4)?,
                })
            })?;
            rows.collect()
        })
    }
    /// Removing a device revokes its sessions and pending targeted items.
    pub fn delete_device(&self, user: &str, device: &str) -> Result<bool> {
        self.with(|c| {
            c.execute(
                "DELETE FROM devices WHERE id = ?1 AND user_id = ?2",
                params![device, user],
            )
        })
        .map(|n| n > 0)
    }
    pub fn owns_device(&self, user: &str, device: &str) -> Result<bool> {
        self.with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM devices WHERE id = ?1 AND user_id = ?2",
                params![device, user],
                |r| r.get::<_, i64>(0),
            )
        })
        .map(|n| n > 0)
    }

    pub fn enqueue(
        &self,
        user: &str,
        target: Option<&str>,
        url: &str,
        filename: Option<&str>,
    ) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let t = now();
        self.with(|c| {
            let pending: i64 = c.query_row(
                "SELECT COUNT(*) FROM queue WHERE user_id = ?1 AND state = 'pending'",
                [user],
                |r| r.get(0),
            )?;
            if pending >= 500 {
                return Err(rusqlite::Error::InvalidQuery);
            }
            c.execute(
                "INSERT INTO queue (id, user_id, target_device, url, filename, state, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?6)",
                params![id, user, target, url, filename, t],
            )
        })
        .context("queue is full (500 pending items) or unavailable")?;
        Ok(id)
    }
    pub fn queue(&self, user: &str) -> Result<Vec<QueueItem>> {
        self.with(|c| {
            let mut statement = c.prepare(
                "SELECT id, target_device, url, filename, state, claimed_by, job_id, error, created_at, updated_at
                 FROM queue WHERE user_id = ?1 ORDER BY created_at DESC LIMIT 200",
            )?;
            let rows = statement.query_map([user], queue_item)?;
            rows.collect()
        })
    }
    /// Atomically hand pending items to one device. Untargeted items go to the
    /// first device that asks; targeted items only to their device.
    pub fn claim(&self, user: &str, device: &str) -> Result<Vec<QueueItem>> {
        self.with(|c| {
            c.execute(
                "UPDATE queue SET state = 'claimed', claimed_by = ?2, updated_at = ?3
                 WHERE id IN (SELECT id FROM queue WHERE user_id = ?1 AND state = 'pending'
                   AND (target_device IS NULL OR target_device = ?2) ORDER BY created_at LIMIT 20)",
                params![user, device, now()],
            )?;
            // Unreported claims are returned again: the desktop uses the queue
            // UUID as its idempotent job ID, so a crash before reporting
            // cannot create a second download.
            let mut statement = c.prepare(
                "SELECT id, target_device, url, filename, state, claimed_by, job_id, error, created_at, updated_at
                 FROM queue WHERE user_id = ?1 AND claimed_by = ?2 AND state = 'claimed'
                 ORDER BY created_at LIMIT 20",
            )?;
            let rows = statement.query_map(params![user, device], queue_item)?;
            rows.collect()
        })
    }
    pub fn report(
        &self,
        user: &str,
        device: &str,
        id: &str,
        ok: bool,
        job: Option<&str>,
        error: Option<&str>,
    ) -> Result<bool> {
        self.with(|c| {
            c.execute(
                "UPDATE queue SET state = ?4, job_id = ?5, error = ?6, updated_at = ?7
                 WHERE id = ?1 AND user_id = ?2 AND claimed_by = ?3 AND state = 'claimed'",
                params![
                    id,
                    user,
                    device,
                    if ok { "accepted" } else { "failed" },
                    job,
                    error,
                    now()
                ],
            )
        })
        .map(|n| n > 0)
    }
    pub fn delete_queue_item(&self, user: &str, id: &str) -> Result<bool> {
        self.with(|c| {
            c.execute(
                "DELETE FROM queue WHERE id = ?1 AND user_id = ?2 AND state != 'claimed'",
                params![id, user],
            )
        })
        .map(|n| n > 0)
    }

    pub fn sync_history(&self, user: &str, device: &str, entries: &[HistoryEntry]) -> Result<()> {
        self.with(|c| {
            let t = now();
            for e in entries {
                c.execute(
                    "INSERT INTO history (user_id, device_id, job_id, name, source, state, bytes, total, created_at, updated_at, synced_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                     ON CONFLICT (user_id, device_id, job_id) DO UPDATE SET name = excluded.name,
                       source = excluded.source, state = excluded.state, bytes = excluded.bytes,
                       total = excluded.total, updated_at = excluded.updated_at, synced_at = excluded.synced_at",
                    params![user, device, e.job_id, e.name, e.source, e.state, e.bytes, e.total, e.created_at, e.updated_at, t],
                )?;
            }
            Ok(())
        })
    }
    pub fn history(&self, user: &str, limit: i64) -> Result<Vec<HistoryEntry>> {
        self.with(|c| {
            let mut statement = c.prepare(
                "SELECT h.job_id, h.name, h.source, h.state, h.bytes, h.total, h.created_at, h.updated_at, h.device_id, d.name
                 FROM history h JOIN devices d ON d.id = h.device_id
                 WHERE h.user_id = ?1 ORDER BY h.updated_at DESC LIMIT ?2",
            )?;
            let rows = statement.query_map(params![user, limit], |r| {
                Ok(HistoryEntry {
                    job_id: r.get(0)?,
                    name: r.get(1)?,
                    source: r.get(2)?,
                    state: r.get(3)?,
                    bytes: r.get(4)?,
                    total: r.get(5)?,
                    created_at: r.get(6)?,
                    updated_at: r.get(7)?,
                    device_id: r.get(8)?,
                    device_name: r.get(9)?,
                })
            })?;
            rows.collect()
        })
    }
    /// Per-day completed counts and bytes for the last 30 days, plus totals by state.
    pub fn analytics(&self, user: &str) -> Result<serde_json::Value> {
        self.with(|c| {
            let mut daily = c.prepare(
                "SELECT substr(updated_at, 1, 10) AS day, COUNT(*), COALESCE(SUM(bytes), 0)
                 FROM history WHERE user_id = ?1 AND state = 'Completed'
                 GROUP BY day ORDER BY day DESC LIMIT 30",
            )?;
            let days: Vec<serde_json::Value> = daily
                .query_map([user], |r| {
                    Ok(serde_json::json!({
                        "day": r.get::<_, String>(0)?,
                        "completed": r.get::<_, i64>(1)?,
                        "bytes": r.get::<_, f64>(2)?,
                    }))
                })?
                .collect::<rusqlite::Result<_>>()?;
            let mut states = c.prepare(
                "SELECT state, COUNT(*), COALESCE(SUM(bytes), 0) FROM history WHERE user_id = ?1 GROUP BY state",
            )?;
            let by_state: Vec<serde_json::Value> = states
                .query_map([user], |r| {
                    Ok(serde_json::json!({
                        "state": r.get::<_, String>(0)?,
                        "count": r.get::<_, i64>(1)?,
                        "bytes": r.get::<_, f64>(2)?,
                    }))
                })?
                .collect::<rusqlite::Result<_>>()?;
            Ok(serde_json::json!({ "days": days, "states": by_state }))
        })
    }
}

fn queue_item(r: &rusqlite::Row) -> rusqlite::Result<QueueItem> {
    Ok(QueueItem {
        id: r.get(0)?,
        target_device: r.get(1)?,
        url: r.get(2)?,
        filename: r.get(3)?,
        state: r.get(4)?,
        claimed_by: r.get(5)?,
        job_id: r.get(6)?,
        error: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
    })
}
