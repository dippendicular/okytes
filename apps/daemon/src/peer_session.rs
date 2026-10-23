//! Authenticated peer socket execution. Control metadata uses bounded JSON
//! frames; payload uses a compact bounded binary frame and never enters JSON.

use crate::peer_identity::{ALPN, SERVER_NAME};
use crate::peer_transfer::{Receiver, Source, CHUNK_SIZE};
use okytes_core::peer::{Direction, Entry, Hash, Manifest, State, MAX_FRAME};
use okytes_persistence::SqliteStore;
use rustls::{pki_types::ServerName, ClientConfig, ServerConfig};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{collections::BTreeMap, io, net::SocketAddr, path::Path, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use uuid::Uuid;

const CONTROL: u8 = 1;
const CHUNK: u8 = 2;
const IO_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Control {
    Offer {
        request_id: Uuid,
        manifest: Manifest,
    },
    Pending {
        request_id: Uuid,
    },
    Resume {
        request_id: Uuid,
        offsets: BTreeMap<String, u64>,
    },
    Finish {
        request_id: Uuid,
    },
    Complete {
        request_id: Uuid,
    },
    Ack {
        path: String,
        offset: u64,
    },
    Error {
        code: String,
    },
}

fn invalid() -> io::Error {
    io::Error::other("invalid or unauthorized peer session")
}

async fn timed<T>(future: impl std::future::Future<Output = io::Result<T>>) -> io::Result<T> {
    tokio::time::timeout(IO_TIMEOUT, future)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "peer operation timed out"))?
}

async fn write_control(stream: &mut (impl AsyncWrite + Unpin), value: &Control) -> io::Result<()> {
    let body = serde_json::to_vec(value).map_err(|_| invalid())?;
    if body.is_empty() || body.len() > MAX_FRAME {
        return Err(invalid());
    }
    timed(stream.write_all(&[CONTROL])).await?;
    timed(stream.write_all(&(body.len() as u32).to_be_bytes())).await?;
    timed(stream.write_all(&body)).await?;
    timed(stream.flush()).await
}

async fn read_control<T: DeserializeOwned>(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<T> {
    let kind = timed(stream.read_u8()).await?;
    if kind != CONTROL {
        return Err(invalid());
    }
    let size = timed(stream.read_u32()).await? as usize;
    if size == 0 || size > MAX_FRAME {
        return Err(invalid());
    }
    let mut body = vec![0; size];
    timed(stream.read_exact(&mut body)).await?;
    serde_json::from_slice(&body).map_err(|_| invalid())
}

async fn write_chunk(
    stream: &mut (impl AsyncWrite + Unpin),
    path: &str,
    offset: u64,
    bytes: &[u8],
) -> io::Result<()> {
    let path = path.as_bytes();
    if path.is_empty() || path.len() > 1024 || bytes.is_empty() || bytes.len() > CHUNK_SIZE {
        return Err(invalid());
    }
    timed(stream.write_all(&[CHUNK])).await?;
    timed(stream.write_all(&(path.len() as u16).to_be_bytes())).await?;
    timed(stream.write_all(&(bytes.len() as u32).to_be_bytes())).await?;
    timed(stream.write_all(&offset.to_be_bytes())).await?;
    timed(stream.write_all(path)).await?;
    timed(stream.write_all(bytes)).await?;
    timed(stream.flush()).await
}

async fn read_chunk(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<(String, u64, Vec<u8>)> {
    if timed(stream.read_u8()).await? != CHUNK {
        return Err(invalid());
    }
    let path_len = timed(stream.read_u16()).await? as usize;
    let data_len = timed(stream.read_u32()).await? as usize;
    let offset = timed(stream.read_u64()).await?;
    if path_len == 0 || path_len > 1024 || data_len == 0 || data_len > CHUNK_SIZE {
        return Err(invalid());
    }
    let mut path = vec![0; path_len];
    let mut data = vec![0; data_len];
    timed(stream.read_exact(&mut path)).await?;
    timed(stream.read_exact(&mut data)).await?;
    Ok((
        String::from_utf8(path).map_err(|_| invalid())?,
        offset,
        data,
    ))
}

/// Send an immutable source tree to one explicitly approved manual address.
pub async fn send(
    addr: SocketAddr,
    tls: ClientConfig,
    source: &Source,
    request_id: Uuid,
) -> io::Result<()> {
    send_inner(addr, tls, source, request_id, None).await
}

/// Send while mirroring receiver-confirmed offsets and lifecycle into the
/// outbound profile. A chunk advances locally only after the receiver synced
/// and committed the same offset.
pub async fn send_managed(
    addr: SocketAddr,
    tls: ClientConfig,
    source: &Source,
    request_id: Uuid,
    store: &SqliteStore,
    peer: Hash,
) -> io::Result<()> {
    send_inner(addr, tls, source, request_id, Some((store, peer))).await
}

async fn send_inner(
    addr: SocketAddr,
    tls: ClientConfig,
    source: &Source,
    request_id: Uuid,
    managed: Option<(&SqliteStore, Hash)>,
) -> io::Result<()> {
    let socket = timed(TcpStream::connect(addr)).await?;
    socket.set_nodelay(true)?;
    let name = ServerName::try_from(SERVER_NAME)
        .map_err(|_| invalid())?
        .to_owned();
    let mut stream = timed(TlsConnector::from(Arc::new(tls)).connect(name, socket)).await?;
    if stream.get_ref().1.alpn_protocol() != Some(ALPN) {
        return Err(invalid());
    }
    write_control(
        &mut stream,
        &Control::Offer {
            request_id,
            manifest: source.manifest().clone(),
        },
    )
    .await?;
    let offsets = match read_control::<Control>(&mut stream).await? {
        Control::Resume {
            request_id: id,
            offsets,
        } if id == request_id => offsets,
        Control::Pending { request_id: id } if id == request_id => {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "receiver approval is pending",
            ))
        }
        _ => return Err(invalid()),
    };
    let mut local = if let Some((store, peer)) = &managed {
        let current = store
            .peer_request(request_id)
            .await
            .map_err(|_| invalid())?
            .ok_or_else(invalid)?;
        let negotiating = if current.state == State::AwaitingApproval {
            store
                .peer_transition(
                    request_id,
                    peer.clone(),
                    current.revision,
                    State::Negotiating,
                )
                .await
                .map_err(|_| invalid())?
        } else {
            current
        };
        if negotiating.state == State::Negotiating {
            Some(
                store
                    .peer_transition(
                        request_id,
                        peer.clone(),
                        negotiating.revision,
                        State::Transferring,
                    )
                    .await
                    .map_err(|_| invalid())?,
            )
        } else if negotiating.state == State::Transferring {
            Some(negotiating)
        } else {
            return Err(invalid());
        }
    } else {
        None
    };
    for entry in &source.manifest().entries {
        let Entry::File { path, size, .. } = entry else {
            continue;
        };
        let mut offset = offsets.get(path).copied().unwrap_or(0);
        if offset > *size {
            return Err(invalid());
        }
        while offset < *size {
            if let Some((store, peer)) = &managed {
                authorized(store, request_id, peer).await?;
            }
            let bytes = source.read(path, offset)?;
            write_chunk(&mut stream, path, offset, &bytes).await?;
            offset += bytes.len() as u64;
            match read_control::<Control>(&mut stream).await? {
                Control::Ack {
                    path: ack_path,
                    offset: ack_offset,
                } if ack_path == *path && ack_offset == offset => {}
                _ => return Err(invalid()),
            }
            if let Some((store, peer)) = &managed {
                let previous = offset - bytes.len() as u64;
                store
                    .peer_checkpoint(request_id, peer.clone(), path.clone(), previous, offset)
                    .await
                    .map_err(|_| invalid())?;
            }
        }
    }
    write_control(&mut stream, &Control::Finish { request_id }).await?;
    match read_control::<Control>(&mut stream).await? {
        Control::Complete { request_id: id } if id == request_id => {
            if let (Some((store, peer)), Some(current)) = (managed, local.take()) {
                let verifying = store
                    .peer_transition(request_id, peer.clone(), current.revision, State::Verifying)
                    .await
                    .map_err(|_| invalid())?;
                store
                    .peer_transition(request_id, peer, verifying.revision, State::Completed)
                    .await
                    .map_err(|_| invalid())?;
            }
            Ok(())
        }
        _ => Err(invalid()),
    }
}

/// Accept one mutually authenticated connection. Offers without a separate
/// persisted receiver approval are admitted but receive no payload authority.
pub async fn receive_one(
    listener: &TcpListener,
    tls: ServerConfig,
    store: &SqliteStore,
    destination: &Path,
) -> io::Result<Uuid> {
    let (socket, _) = timed(listener.accept()).await?;
    receive_socket(socket, tls, store, destination).await
}

/// Process one already-admitted TCP socket. Listener owners use this entry
/// point to enforce a global connection bound before TLS allocation.
pub async fn receive_socket(
    socket: TcpStream,
    tls: ServerConfig,
    store: &SqliteStore,
    destination: &Path,
) -> io::Result<Uuid> {
    socket.set_nodelay(true)?;
    let mut stream = timed(TlsAcceptor::from(Arc::new(tls)).accept(socket)).await?;
    if stream.get_ref().1.alpn_protocol() != Some(ALPN) {
        return Err(invalid());
    }
    let certificate = stream
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|values| values.first())
        .ok_or_else(invalid)?;
    let fingerprint = Hash::digest(certificate.as_ref());
    active(store, &fingerprint).await?;
    let (request_id, manifest) = match read_control::<Control>(&mut stream).await? {
        Control::Offer {
            request_id,
            manifest,
        } => (request_id, manifest),
        _ => return Err(invalid()),
    };
    let request = store
        .peer_offer(
            request_id,
            fingerprint.clone(),
            Direction::Inbound,
            manifest.clone(),
        )
        .await
        .map_err(|_| invalid())?;
    if !matches!(request.state, State::Negotiating | State::Transferring) {
        write_control(&mut stream, &Control::Pending { request_id }).await?;
        return Ok(request_id);
    }
    let outcome: io::Result<()> = async {
        active(store, &fingerprint).await?;
        let request = if request.state == State::Negotiating {
            store
                .peer_transition(
                    request_id,
                    fingerprint.clone(),
                    request.revision,
                    State::Transferring,
                )
                .await
                .map_err(|_| invalid())?
        } else {
            request
        };
        let selected_destination = store
            .peer_inbound_destination(request_id)
            .await
            .map_err(|_| invalid())?
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| destination.to_path_buf());
        let receiver = Receiver::open(
            &selected_destination,
            request_id,
            &format!("Okytes transfer {request_id}"),
            manifest.clone(),
        )?;
        let durable = store
            .peer_offsets(request_id, fingerprint.clone())
            .await
            .map_err(|_| invalid())?;
        let mut offsets = BTreeMap::new();
        for entry in &manifest.entries {
            if let Entry::File { path, .. } = entry {
                let offset = durable.get(path).copied().unwrap_or(0);
                receiver.resume(path, offset)?;
                offsets.insert(path.clone(), offset);
            }
        }
        write_control(
            &mut stream,
            &Control::Resume {
                request_id,
                offsets: offsets.clone(),
            },
        )
        .await?;
        let expected_bytes = manifest.validate().map_err(io::Error::other)?;
        let mut remaining = expected_bytes.saturating_sub(offsets.values().sum::<u64>());
        while remaining > 0 {
            authorized(store, request_id, &fingerprint).await?;
            let (path, offset, bytes) = read_chunk(&mut stream).await?;
            if offsets.get(&path).copied().unwrap_or(0) != offset {
                return Err(invalid());
            }
            let next = receiver.write(&path, offset, &bytes)?;
            store
                .peer_checkpoint(request_id, fingerprint.clone(), path.clone(), offset, next)
                .await
                .map_err(|_| invalid())?;
            write_control(
                &mut stream,
                &Control::Ack {
                    path: path.clone(),
                    offset: next,
                },
            )
            .await?;
            offsets.insert(path, next);
            remaining = remaining
                .checked_sub(bytes.len() as u64)
                .ok_or_else(invalid)?;
        }
        authorized(store, request_id, &fingerprint).await?;
        match read_control::<Control>(&mut stream).await? {
            Control::Finish { request_id: id } if id == request_id => {}
            _ => return Err(invalid()),
        }
        let verifying = store
            .peer_transition(
                request_id,
                fingerprint.clone(),
                request.revision,
                State::Verifying,
            )
            .await
            .map_err(|_| invalid())?;
        receiver.publish()?;
        active(store, &fingerprint).await?;
        store
            .peer_transition(
                request_id,
                fingerprint,
                verifying.revision,
                State::Completed,
            )
            .await
            .map_err(|_| invalid())?;
        write_control(&mut stream, &Control::Complete { request_id }).await?;
        Ok(())
    }
    .await;
    if let Err(error) = outcome {
        let _ = store
            .peer_set_failure(
                request_id,
                format!("Incoming peer transfer stopped: {}", error.kind()),
            )
            .await;
        if let Ok(Some(current)) = store.peer_request(request_id).await {
            if matches!(
                current.state,
                State::Negotiating | State::Transferring | State::Verifying | State::Retrying
            ) {
                let _ = store
                    .peer_transition(
                        request_id,
                        current.peer,
                        current.revision,
                        State::Interrupted,
                    )
                    .await;
            }
        }
        return Err(error);
    }
    Ok(request_id)
}

async fn active(store: &SqliteStore, fingerprint: &Hash) -> io::Result<()> {
    store
        .peer_trust_by_fingerprint(fingerprint.clone())
        .await
        .map_err(|_| invalid())?
        .ok_or_else(invalid)
        .map(|_| ())
}

async fn authorized(store: &SqliteStore, id: Uuid, fingerprint: &Hash) -> io::Result<()> {
    active(store, fingerprint).await?;
    let request = store
        .peer_request(id)
        .await
        .map_err(|_| invalid())?
        .ok_or_else(invalid)?;
    if request.peer != *fingerprint || request.state != State::Transferring {
        return Err(invalid());
    }
    Ok(())
}
