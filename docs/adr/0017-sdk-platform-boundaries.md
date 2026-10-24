# ADR 0017: SDK platform boundaries

## Context

M11 exposes the transfer system to embedded Rust applications and remote language clients. Sharing internal engine types would couple external users to storage, workers and transport dependencies. Network retries also require a durable duplicate-prevention boundary.

## Decision

`okytes-sdk` is the embedded Rust facade and owns an isolated `Session`. `okytes-daemon-api` is the lightweight remote Rust client and defaults to no browser integration; native browser capture is an explicit feature. TypeScript and Python clients use the authenticated `/v1` contract.

Creation requests may carry `Idempotency-Key`. The daemon scopes keys by token owner and route, hashes the canonical JSON body, stores successful responses in SQLite for 24 hours, replays matching requests and rejects conflicting bodies. Language clients generate keys for retryable creation calls.

SDKs discover installed operations through `/v1/capabilities`. Destinations are paths on the engine or daemon host. File credentials remain request-local and peer operations continue to require paired identities and explicit approval.

## Consequences

Embedded and remote ownership remain visibly different. Remote Rust tools avoid browser dependencies unless requested. A retry after a lost response cannot create another job while the retained key exists. Event delivery remains bounded and uses deterministic snapshot resynchronization after connection loss or lag.

