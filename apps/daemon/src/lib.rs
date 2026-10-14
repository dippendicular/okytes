//! Headless service host. Authentication, request policy and bounded transport
//! surround the same queue owner used by embedded desktop sessions.
pub mod auth;
pub mod server;
pub mod transport;
