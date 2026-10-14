//! Torrent adapter boundary. Validation precedes library parsing and filesystem
//! access; HTTP range scheduling is never used for torrent data.
mod adapter;
pub mod metainfo;
mod storage;
pub use adapter::Adapter;
