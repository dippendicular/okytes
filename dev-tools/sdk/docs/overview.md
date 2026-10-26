# Okytes SDK

**Reliable file transfers without rebuilding the infrastructure.**

Okytes provides two separate integration models. Rust applications can own an isolated embedded engine through `okytes-sdk`. TypeScript, Python, and Rust applications can control an existing daemon through its authenticated `/v1` API. A daemon client never opens the daemon database, and an embedded engine never starts a network listener by construction.

Use the embedded model when the application owns the transfer lifecycle and state directory. Use the daemon model for automation, multiple clients, background continuity, or language interoperability. See [ownership details](embedding-vs-daemon.md), [security](security.md), and [compatibility](compatibility.md).
