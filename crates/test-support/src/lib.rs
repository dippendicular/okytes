//! # Okytes Test Support
//!
//! Fault-injection test server and test fixtures for Okytes integration tests.
//!
//! This crate provides a hyper-based HTTP server that deterministically
//! simulates various failure modes for testing the download engine.
//!
//! ## Supported fault modes
//!
//! - Low bandwidth (throttled responses)
//! - High latency (delayed responses)
//! - Mid-body disconnects
//! - Wrong Content-Length headers
//! - Invalid headers
//! - Unsupported / ignored Range requests
//! - 200 response to Range request (full body)
//! - Changed ETag mid-transfer
//! - Redirects (single and chained)
//! - Expired URLs
//! - 429/503 throttling with Retry-After
//! - Corrupted chunks
//!
//! **This crate must never be a dependency of non-test code.**

pub mod server;
