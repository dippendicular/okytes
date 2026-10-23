# Embedded engine or daemon client

| | Embedded Rust | Daemon client |
| --- | --- | --- |
| Engine owner | Calling process | Okytes daemon |
| State access | Exclusive isolated state directory | Authenticated API only |
| Languages | Rust | Rust, TypeScript/Node, Python |
| Job lifetime | Host process; durable recovery on reopen | Continues after client disconnect |
| Network listener | None created by SDK | Configured separately on daemon |
| Shutdown | Explicit graceful checkpoint/drain | Closing client only closes client resources |

Never point an embedded engine at a state directory currently owned by a daemon.
