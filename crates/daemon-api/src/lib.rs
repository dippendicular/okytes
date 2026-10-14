//! Version-one management contract and GUI-independent daemon client. Core owns
//! service DTOs; native apps select this backend without changing webview commands.
pub mod client;
pub mod contract;
pub use client::Client;
