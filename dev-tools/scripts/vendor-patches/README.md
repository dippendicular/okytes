# Torrent dependency patches

`vendor/` stays ignored. Run `node dev-tools/scripts/prepare-vendor.mjs` before the first
Cargo command in a clean checkout. The script checks upstream archive SHA-256,
applies these patch sets, and verifies every Rust source and Cargo manifest.
Existing local sources are verified, never overwritten. An optional
`--cache /path/to/cargo/registry/cache/index-directory` reads existing crate
archives without accessing the network. Add `--verify-archives` to test extraction, patch application and hashes without changing local vendor files. Git and tar must be installed.

The four upstream crates retain their MIT/Apache-2.0 licenses and notices in the
downloaded sources. Patch changes are maintained under the repository license.
Versions and archive checksums are pinned in `manifest.json`.

The changes preserve M6's bounded torrent metadata, peer/discovery queues,
durable payload admission, verified piece state, shutdown, and local swarm
behavior. `librqbit-utp` uses ringbuf 0.5 to address RUSTSEC-2026-0293.
Review updates against the upstream sources and rerun the torrent swarm,
malicious-input, crash-recovery, limiter and dependency-audit tests.
