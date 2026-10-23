# Events and errors

The daemon event stream is bounded and coalesced. It provides change notifications rather than an immutable journal. Every new connection begins with `Resync`; a slow subscriber or reconnect also receives `Resync`. Consumers then discard cached pages and fetch authoritative snapshots. Disconnecting a client never stops daemon-owned jobs.

API errors contain `error`, `category`, `retryable`, and a sanitized `message`. Retry only when `retryable` is true, with bounded exponential backoff and jitter. Cancellation is an explicit control operation; cancelling a language task or dropping a handle only stops the local wait or subscription.
