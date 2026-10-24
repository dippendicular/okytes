//! Version-one management contract and GUI-independent daemon client. Core owns
//! service DTOs; native apps select this backend without changing webview commands.
pub mod client;
pub mod contract;
mod quick;
pub use client::{Client, DaemonError};
pub use quick::open_quick;
