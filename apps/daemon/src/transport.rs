//! Connection admission before HTTP/TLS parsing. Permits live with the IO stream,
//! so idle, handshaking and streaming clients all count toward the same hard cap.
use axum_server::accept::Accept;
use std::{
    future::Ready,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::{OwnedSemaphorePermit, Semaphore},
};

/// Bounded accepted-stream wrapper.
pub struct Limited<I> {
    inner: I,
    _permit: OwnedSemaphorePermit,
}
impl<I: AsyncRead + Unpin> AsyncRead for Limited<I> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buffer)
    }
}
impl<I: AsyncWrite + Unpin> AsyncWrite for Limited<I> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
/// Cloneable per-listener connection gate; excess connections close immediately.
#[derive(Clone)]
pub struct Gate(Arc<Semaphore>);
impl Gate {
    /// Set a finite cap; production uses 64 simultaneous connections.
    pub fn new(cap: usize) -> Self {
        Self(Arc::new(Semaphore::new(cap)))
    }
}
impl<I, S> Accept<I, S> for Gate {
    type Stream = Limited<I>;
    type Service = S;
    type Future = Ready<io::Result<(Self::Stream, S)>>;
    fn accept(&self, inner: I, service: S) -> Self::Future {
        std::future::ready(
            self.0
                .clone()
                .try_acquire_owned()
                .map(|permit| {
                    (
                        Limited {
                            inner,
                            _permit: permit,
                        },
                        service,
                    )
                })
                .map_err(|_| io::Error::new(io::ErrorKind::ConnectionRefused, "connection limit")),
        )
    }
}
