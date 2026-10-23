# SDK compatibility

The first public contract is API v1 with SDK versions `>=0.1.0,<0.2.0`. The daemon reports this range from `/v1/capabilities`.

- Rust: repository MSRV 1.95, edition 2021.
- TypeScript/Node: Node.js 20+, ESM.
- Python: CPython 3.10+.
- Additions remain backward compatible within v1. Breaking changes require a new API version and migration guide.
- Unknown capabilities must be ignored unless supported. Unknown lifecycle values must never grant authority or count as completion.

The optional C ABI is deferred. No stable native ABI is published in M11.
