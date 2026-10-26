# SDK compatibility

The first public contract is API v1 with SDK versions `>=0.1.0,<0.2.0`. The daemon reports this range from `/v1/capabilities`.

- Rust: repository MSRV 1.95, edition 2021.
- TypeScript/Node: Node.js 20+, ESM.
- Python: CPython 3.10+.
- Additions remain backward compatible within v1. Breaking changes require a new API version and migration guide.
- Unknown capabilities must be ignored unless supported. Unknown lifecycle values must never grant authority or count as completion.

The optional C ABI is deferred. No stable native ABI is published in M11.
# Compatibility and versioning

The daemon API major version is carried in its `/v1` path. SDK `0.1.x` releases accept the capability range advertised as `>=0.1.0,<0.2.0`. Additive response fields and new capability operations may appear within that range; clients must ignore fields they do not understand and must check protocol capabilities before calling optional operations.

Breaking route, field or behavior changes require a new API major version and a migration period. Rust public SDK types follow Cargo SemVer. npm and Python packages use the same release number when they target the same daemon contract.

Creation idempotency keys are scoped to the authenticated token owner and route and retained for 24 hours. Reusing a key with the same canonical request returns the original response. Reusing it with different input returns `idempotency_conflict`.

Event subscribers replace their local snapshot whenever they receive `Resync`, including initial connection, a lost connection or bounded-channel lag. Reconnection uses bounded exponential backoff with jitter.
