//! # Okytes Core
//!
//! Core engine types, state machines, scheduler, and service traits for the
//! Okytes download manager.
//!
//! ## Invariants
//!
//! This crate enforces the following transfer invariants:
//!
//! - **Non-overlapping segments**: Byte ranges assigned to workers never overlap
//!   and always cover the file exactly.
//! - **Idempotent transitions**: Every download and segment state transition is
//!   explicit, idempotent, and safe to replay after a crash.
//! - **Remote identity**: Validators (strong ETag, Last-Modified, Content-Length)
//!   observed at start are persisted and checked on resume.
//! - **Strong validators only**: Weak ETags are never used for `If-Range`.
//! - **Bounded resources**: All channels, buffers, and connections are bounded.
//!
//! ## Dependency Direction
//!
//! This crate must never depend on a GUI framework, Tauri, browser APIs, or
//! OS-specific crates. Platform code lives behind traits implemented outside core.

pub mod desktop;
pub mod engine;
pub mod error;
pub mod files;
pub mod range;
pub mod scheduler;
pub mod service;
pub mod state;
pub mod store;
pub mod transport;
pub mod types;
