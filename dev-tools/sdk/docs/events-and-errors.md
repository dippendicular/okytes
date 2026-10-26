# Events and errors

The daemon event stream is bounded and coalesced. It provides change notifications rather than an immutable journal. SSE frames carry monotonic connection cursors and clients send the last observed cursor when reconnecting. Every new connection begins with `Resync`; a slow subscriber or reconnect also receives `Resync` because cursors are not a durable event journal. Consumers then discard cached pages and fetch authoritative snapshots. Disconnecting a client never stops daemon-owned jobs.

API errors contain `error`, `category`, `retryable`, and a sanitized `message`. Retry only when `retryable` is true, with bounded exponential backoff and jitter. Cancellation is an explicit control operation; cancelling a language task or dropping a handle only stops the local wait or subscription.

Creation requests accept `Idempotency-Key`. SDK-generated keys are durably reserved before admission and retained for 24 hours, scoped to the credential owner and route. A matching retry receives the original job identifier. A retry that finds a crash-interrupted pending reservation receives retryable `idempotency_pending` instead of creating a duplicate; a different body receives `idempotency_conflict`.
