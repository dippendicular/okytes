//! Headless service host. Authentication, request policy and bounded transport
//! surround the same queue owner used by embedded desktop sessions.
pub mod auth;
pub mod discovery;
pub mod peer_identity;
pub mod peer_session;
pub mod peer_transfer;
pub mod quick;
pub mod server;
pub mod transport;
