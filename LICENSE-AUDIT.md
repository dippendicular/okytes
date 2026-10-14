# M8 dependency license audit

Audit date: 2026-10-15. Platform: Windows x86_64. A fresh online
`cargo deny check` passed advisories, bans, licenses and sources after adding
the updater dependencies. Existing permitted duplicate-version warnings remain;
they are not suppressed or described as errors. The detailed local log is kept
outside the public source tree; CI repeats the audit against the committed lockfile.

New direct dependencies use compatible licenses:

| Dependency | Locked version | License |
|---|---|---|
| tauri-plugin-updater | 2.14.0 | Apache-2.0 OR MIT |
| minisign-verify | 0.3.0 | MIT |
| base64 | 0.22.1 | MIT OR Apache-2.0 |
| semver | 1.0.28 | MIT OR Apache-2.0 |

The new archive support includes `tar` 0.4.46 (MIT OR Apache-2.0) and
`zip` 4.6.1 (MIT). All transitive dependencies are subject to the same deny
policy. Existing vendored torrent libraries retain their upstream licenses and
notices; the bootstrap verifies the reviewed source patches.

Release automation emits a Cargo CycloneDX SBOM and a separate frontend license
inventory. This inventory is not a substitute for distributing upstream notices.
Platform packaging tools, Apple SDKs, WebView2 and system libraries have their
own distribution terms. Their availability does not establish that a particular
installer was tested or that redistribution rights were audited beyond the
project's Rust dependency policy.
