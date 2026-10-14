//! Revocable scoped token digests. Plaintext exists only in issuance responses and
//! the private bootstrap file; it is never written into SQLite or audit logs.
use okytes_daemon_api::contract::{IssueToken, IssuedToken, Scope, TokenInfo};
use okytes_persistence::SqliteStore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io, path::Path, sync::Arc};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;

#[derive(Serialize, Deserialize)]
struct Record {
    info: TokenInfo,
    hash: Vec<u8>,
}

/// Serialized token registry. Maximum 64 credentials per profile.
pub struct Tokens {
    store: Arc<SqliteStore>,
    records: Mutex<Vec<Record>>,
}
fn invalid() -> io::Error {
    io::Error::other("token registry operation failed")
}
fn hash(value: &str) -> Vec<u8> {
    Sha256::digest(value.as_bytes()).to_vec()
}
fn equal(a: &[u8], b: &[u8]) -> bool {
    bool::from(a.ct_eq(b))
}

impl Tokens {
    /// Load hashed credentials, or provision a private first-run admin token.
    pub async fn open(store: Arc<SqliteStore>, directory: &Path) -> io::Result<Arc<Self>> {
        let records = match store
            .preference("daemon-auth-v1")
            .await
            .map_err(|_| invalid())?
        {
            Some(value) => serde_json::from_str(&value).map_err(|_| invalid())?,
            None => Vec::new(),
        };
        let tokens = Arc::new(Self {
            store,
            records: Mutex::new(records),
        });
        let first_run = {
            let records = tokens.records.lock().await;
            records.is_empty()
        };
        if first_run {
            let path = directory.join("daemon-admin.token");
            let token = match okytes_browser_bridge::read_secret(&path) {
                Ok(bytes) => String::from_utf8(bytes).map_err(|_| invalid())?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let random: [u8; 32] = rand::random();
                    let token = format!(
                        "{}.{}",
                        uuid::Uuid::new_v4(),
                        random
                            .iter()
                            .map(|byte| format!("{byte:02x}"))
                            .collect::<String>()
                    );
                    okytes_browser_bridge::write_secret(&path, token.as_bytes())?;
                    token
                }
                Err(_) => return Err(invalid()),
            };
            let (id, secret) = token.split_once('.').ok_or_else(invalid)?;
            if uuid::Uuid::parse_str(id).is_err()
                || id.len() != 36
                || secret.len() != 64
                || !secret.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(invalid());
            }
            // Secret file is synced first. A crash before this commit reuses
            // that protected file; a registry with revoked tokens is never empty
            // and therefore never silently provisions a replacement credential.
            let mut records = tokens.records.lock().await;
            records.push(Record {
                info: TokenInfo {
                    id: id.into(),
                    scope: Scope::Admin,
                    expires_at: chrono::Utc::now().timestamp() + 90 * 86400,
                    revoked: false,
                },
                hash: hash(&token),
            });
            tokens.persist(&records).await?;
        }
        Ok(tokens)
    }
    /// Authenticate by digest, checking revocation and expiry on every request.
    pub async fn authenticate(&self, token: &str) -> Option<TokenInfo> {
        if token.len() != 101 {
            return None;
        }
        let presented = hash(token);
        let records = self.records.lock().await;
        let mut found = None;
        for record in records.iter() {
            if equal(&record.hash, &presented)
                && !record.info.revoked
                && record.info.expires_at > chrono::Utc::now().timestamp()
            {
                found = Some(record.info.clone());
            }
        }
        found
    }
    /// Issue one bounded-lifetime credential and commit its digest before replying.
    pub async fn issue(&self, input: IssueToken) -> io::Result<IssuedToken> {
        if input.lifetime_secs == 0 || input.lifetime_secs > 90 * 86400 {
            return Err(invalid());
        }
        let mut records = self.records.lock().await;
        // Revoked/expired entries can be reclaimed; no unbounded credential history.
        records.retain(|r| !r.info.revoked && r.info.expires_at > chrono::Utc::now().timestamp());
        if records.len() >= 64 {
            return Err(invalid());
        }
        let info = TokenInfo {
            id: uuid::Uuid::new_v4().to_string(),
            scope: input.scope,
            expires_at: chrono::Utc::now().timestamp() + i64::from(input.lifetime_secs),
            revoked: false,
        };
        let random: [u8; 32] = rand::random();
        let token = format!(
            "{}.{}",
            info.id,
            random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        records.push(Record {
            info: info.clone(),
            hash: hash(&token),
        });
        if self.persist(&records).await.is_err() {
            records.pop();
            return Err(invalid());
        }
        Ok(IssuedToken { info, token })
    }
    async fn persist(&self, records: &[Record]) -> io::Result<()> {
        self.store
            .set_preference(
                "daemon-auth-v1",
                &serde_json::to_string(records).map_err(|_| invalid())?,
            )
            .await
            .map_err(|_| invalid())
    }
    /// Return public metadata only.
    pub async fn list(&self) -> Vec<TokenInfo> {
        self.records
            .lock()
            .await
            .iter()
            .map(|r| r.info.clone())
            .collect()
    }
    /// Recheck an existing SSE subscriber without retaining its plaintext token.
    pub async fn active(&self, id: &str) -> bool {
        self.records.lock().await.iter().any(|record| {
            record.info.id == id
                && !record.info.revoked
                && record.info.expires_at > chrono::Utc::now().timestamp()
        })
    }
    /// Revoke a single credential; a failed commit leaves it active.
    pub async fn revoke(&self, id: &str) -> io::Result<bool> {
        let mut records = self.records.lock().await;
        let Some(index) = records.iter().position(|r| r.info.id == id) else {
            return Ok(false);
        };
        let previous = records[index].info.revoked;
        records[index].info.revoked = true;
        if let Err(error) = self.persist(&records).await {
            records[index].info.revoked = previous;
            return Err(error);
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn partial_provisioning_reuses_private_secret_and_restart_does_not_undo_revocation() {
        let root = tempfile::tempdir().unwrap();
        let token = format!("{}.{}", uuid::Uuid::new_v4(), "a".repeat(64));
        okytes_browser_bridge::write_secret(
            &root.path().join("daemon-admin.token"),
            token.as_bytes(),
        )
        .unwrap();
        let store = Arc::new(SqliteStore::open(root.path().join("auth.db")).unwrap());
        let registry = Tokens::open(store.clone(), root.path()).await.unwrap();
        let info = registry.authenticate(&token).await.unwrap();
        assert!(registry.revoke(&info.id).await.unwrap());
        drop(registry);
        let reopened = Tokens::open(store, root.path()).await.unwrap();
        assert!(reopened.authenticate(&token).await.is_none());
        assert_eq!(
            okytes_browser_bridge::read_secret(&root.path().join("daemon-admin.token")).unwrap(),
            token.as_bytes()
        );
    }
}
