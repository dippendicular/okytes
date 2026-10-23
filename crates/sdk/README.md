# Okytes Rust SDK

`okytes-sdk` provides two explicit models:

- `EmbeddedEngine` owns an isolated state database and transfer workers.
- `DaemonClient` controls an existing authenticated Okytes daemon and never opens its database.

See `docs/sdk/rust.md` for setup, ownership, recovery, and examples.

