//! Authenticated per-user local browser bridge. Native hosts forward requests;
//! transfer ownership remains with the desktop even if every client disappears.
mod platform;
pub mod protocol;
use protocol::{Request, Response, MAX_FRAME};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{Mutex, Semaphore},
    task::JoinHandle,
};

/// Handler implemented by the queue owner, without introducing a GUI dependency.
pub trait Handler: Send + Sync + 'static {
    /// Accept a validated, scoped operation; return sanitized response data.
    fn handle(&self, request: Request) -> Pin<Box<dyn Future<Output = Response> + Send + '_>>;
}

/// Erased local duplex stream for platform adapters.
pub trait IoStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> IoStream for T {}

/// Installation credential, stored separately from download history.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
    /// Protocol version.
    pub version: u32,
    /// Random 256-bit secret; never passed to the extension.
    pub token: String,
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid bridge message")
}

/// Read little-endian native framing, bounded before allocation. A partial length
/// or body is malformed; clean EOF before a new length ends the connection.
pub async fn read_frame<T: DeserializeOwned>(
    reader: &mut (impl AsyncRead + Unpin),
) -> io::Result<Option<T>> {
    let mut prefix = [0u8; 4];
    if reader.read(&mut prefix[..1]).await? == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut prefix[1..]).await?;
    let len = u32::from_le_bytes(prefix) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(invalid());
    }
    let mut body = vec![0; len];
    reader.read_exact(&mut body).await?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|_| invalid())
}

/// Write one bounded native JSON frame.
pub async fn write_frame<T: Serialize>(
    writer: &mut (impl AsyncWrite + Unpin),
    value: &T,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| invalid())?;
    if bytes.len() > MAX_FRAME {
        return Err(invalid());
    }
    writer
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .await?;
    writer.write_all(&bytes).await?;
    writer.flush().await
}

/// Load the private install token with native no-follow protections.
pub fn credentials(directory: &Path) -> io::Result<Credentials> {
    let file = platform::read_private(&directory.join("browser-token.json"))?;
    if file.len() > 512 {
        return Err(invalid());
    }
    let credentials: Credentials = serde_json::from_slice(&file).map_err(|_| invalid())?;
    if credentials.version != 1
        || credentials.token.len() != 64
        || !credentials.token.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(invalid());
    }
    Ok(credentials)
}

/// Provision once at host installation. Existing tokens are preserved.
pub fn provision(directory: &Path) -> io::Result<()> {
    std::fs::create_dir_all(directory)?;
    let token: [u8; 32] = rand::random();
    let value = Credentials {
        version: 1,
        token: hex(&token),
    };
    let path = directory.join("browser-token.json");
    match platform::write_private(&path, &serde_json::to_vec(&value).map_err(|_| invalid())?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            credentials(directory).map(|_| ())
        }
        Err(error) => Err(error),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// RFC 2104 HMAC-SHA256 with a fixed 64-byte ASCII key. Domain-separated roles
// prevent a server proof from being reflected as a client authentication proof.
fn proof(token: &str, challenge: &str, role: &str) -> String {
    let mut inner = [0x36u8; 64];
    let mut outer = [0x5cu8; 64];
    for (index, byte) in token.bytes().enumerate().take(64) {
        inner[index] ^= byte;
        outer[index] ^= byte;
    }
    let mut hash = Sha256::new();
    hash.update(inner);
    hash.update(role);
    hash.update(challenge);
    let digest = hash.finalize();
    let mut hash = Sha256::new();
    hash.update(outer);
    hash.update(digest);
    hex(&hash.finalize())
}
fn equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Challenge {
    version: u32,
    nonce: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    proof: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    sequence: u32,
    request: Request,
}

/// A per-profile address with no TCP listener. Windows uses a named pipe; Unix
/// uses a socket in a private subdirectory of the profile.
pub fn endpoint(directory: &Path) -> io::Result<PathBuf> {
    platform::endpoint(directory)
}

/// Connect, mutually authenticate and forward one operation. Returning does not
/// take ownership of any engine task; disconnecting cannot cancel a download.
pub async fn forward(directory: &Path, request: Request) -> io::Result<Response> {
    request.validate().map_err(|_| invalid())?;
    let credentials = credentials(directory)?;
    tokio::time::timeout(Duration::from_secs(8), async {
        let mut stream = platform::connect(&endpoint(directory)?).await?;
        let challenge: Challenge = read_frame(&mut stream).await?.ok_or_else(invalid)?;
        if challenge.version != 1 || challenge.nonce.len() != 64 {
            return Err(invalid());
        }
        write_frame(
            &mut stream,
            &Hello {
                proof: proof(&credentials.token, &challenge.nonce, "client-v1"),
            },
        )
        .await?;
        let response: Hello = read_frame(&mut stream).await?.ok_or_else(invalid)?;
        if !equal(
            &response.proof,
            &proof(&credentials.token, &challenge.nonce, "server-v1"),
        ) {
            return Err(invalid());
        }
        write_frame(
            &mut stream,
            &Envelope {
                sequence: 1,
                request,
            },
        )
        .await?;
        read_frame(&mut stream).await?.ok_or_else(invalid)
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "desktop bridge timeout"))?
}

struct Rate {
    start: Instant,
    count: u32,
}
/// Starts a bounded local listener. The caller owns and aborts the returned task.
/// The server never provisions tokens: the native-host installer does that.
pub async fn serve(directory: &Path, handler: Arc<dyn Handler>) -> io::Result<JoinHandle<()>> {
    let credentials = Arc::new(credentials(directory)?);
    let listener = platform::listen(&endpoint(directory)?).await?;
    Ok(tokio::spawn(async move {
        let slots = Arc::new(Semaphore::new(8));
        let rate = Arc::new(Mutex::new(Rate {
            start: Instant::now(),
            count: 0,
        }));
        loop {
            let Ok(stream) = listener.accept().await else {
                break;
            };
            let Ok(slot) = slots.clone().try_acquire_owned() else {
                continue;
            };
            let credentials = credentials.clone();
            let handler = handler.clone();
            let rate = rate.clone();
            tokio::spawn(async move {
                let _slot = slot;
                let _ = tokio::time::timeout(
                    Duration::from_secs(10),
                    connection(stream, &credentials.token, handler, rate),
                )
                .await;
            });
        }
    }))
}

async fn connection(
    mut stream: platform::Stream,
    token: &str,
    handler: Arc<dyn Handler>,
    rate: Arc<Mutex<Rate>>,
) -> io::Result<()> {
    let challenge = Challenge {
        version: 1,
        nonce: hex(&rand::random::<[u8; 32]>()),
    };
    write_frame(&mut stream, &challenge).await?;
    let hello: Hello = read_frame(&mut stream).await?.ok_or_else(invalid)?;
    if !equal(&hello.proof, &proof(token, &challenge.nonce, "client-v1")) {
        return Err(invalid());
    }
    write_frame(
        &mut stream,
        &Hello {
            proof: proof(token, &challenge.nonce, "server-v1"),
        },
    )
    .await?;
    // Single operation per mutually authenticated connection. Replayed envelopes
    // on this connection are never accepted; reconnect produces a fresh challenge.
    let envelope: Envelope = read_frame(&mut stream).await?.ok_or_else(invalid)?;
    if envelope.sequence != 1 {
        return Err(invalid());
    }
    envelope.request.validate().map_err(|_| invalid())?;
    let allowed = {
        let mut rate = rate.lock().await;
        if rate.start.elapsed() >= Duration::from_secs(60) {
            rate.start = Instant::now();
            rate.count = 0;
        }
        rate.count += 1;
        rate.count <= 120
    };
    let response = if allowed {
        handler.handle(envelope.request).await
    } else {
        Response::reply(envelope.request.id, Err("rate_limited".into()))
    };
    write_frame(&mut stream, &response).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hmac_matches_rfc_and_roles_cannot_reflect() {
        assert_eq!(
            proof("Jefe", "what do ya want for nothing?", ""),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_ne!(
            proof(&"a".repeat(64), "nonce", "client-v1"),
            proof(&"a".repeat(64), "nonce", "server-v1")
        );
    }

    #[tokio::test]
    async fn fresh_challenges_reject_unauthenticated_and_replayed_proofs() {
        struct Echo;
        impl Handler for Echo {
            fn handle(
                &self,
                request: Request,
            ) -> Pin<Box<dyn Future<Output = Response> + Send + '_>> {
                Box::pin(async move { Response::reply(request.id, Ok(None)) })
            }
        }
        let root = tempfile::tempdir().unwrap();
        provision(root.path()).unwrap();
        let token = credentials(root.path()).unwrap().token;
        let task = serve(root.path(), Arc::new(Echo)).await.unwrap();
        let path = endpoint(root.path()).unwrap();
        let mut first = platform::connect(&path).await.unwrap();
        let challenge: Challenge = read_frame(&mut first).await.unwrap().unwrap();
        let previous = proof(&token, &challenge.nonce, "client-v1");
        write_frame(
            &mut first,
            &Hello {
                proof: "wrong".into(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            read_frame::<Hello>(&mut first).await,
            Ok(None) | Err(_)
        ));
        let mut second = platform::connect(&path).await.unwrap();
        let fresh: Challenge = read_frame(&mut second).await.unwrap().unwrap();
        assert_ne!(fresh.nonce, challenge.nonce);
        write_frame(&mut second, &Hello { proof: previous })
            .await
            .unwrap();
        assert!(matches!(
            read_frame::<Hello>(&mut second).await,
            Ok(None) | Err(_)
        ));
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn sequence_and_global_rate_limits_are_enforced_after_authentication() {
        struct Echo;
        impl Handler for Echo {
            fn handle(
                &self,
                request: Request,
            ) -> Pin<Box<dyn Future<Output = Response> + Send + '_>> {
                Box::pin(async move { Response::reply(request.id, Ok(None)) })
            }
        }
        let root = tempfile::tempdir().unwrap();
        provision(root.path()).unwrap();
        let token = credentials(root.path()).unwrap().token;
        let task = serve(root.path(), Arc::new(Echo)).await.unwrap();
        let mut stream = platform::connect(&endpoint(root.path()).unwrap())
            .await
            .unwrap();
        let challenge: Challenge = read_frame(&mut stream).await.unwrap().unwrap();
        write_frame(
            &mut stream,
            &Hello {
                proof: proof(&token, &challenge.nonce, "client-v1"),
            },
        )
        .await
        .unwrap();
        let _: Hello = read_frame(&mut stream).await.unwrap().unwrap();
        let ping = || Request {
            version: 1,
            id: uuid::Uuid::new_v4().to_string(),
            operation: protocol::Operation::Ping,
        };
        write_frame(
            &mut stream,
            &Envelope {
                sequence: 2,
                request: ping(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            read_frame::<Response>(&mut stream).await,
            Ok(None) | Err(_)
        ));
        for _ in 0..120 {
            assert!(forward(root.path(), ping()).await.unwrap().ok);
        }
        let rejected = forward(root.path(), ping()).await.unwrap();
        assert!(!rejected.ok);
        assert_eq!(rejected.error.as_deref(), Some("rate_limited"));
        task.abort();
        let _ = task.await;
    }
}
