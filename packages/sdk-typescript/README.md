# Okytes TypeScript SDK

<img width="360" alt="Okytes" src="../../assets/images/brand/okytes_logo_transperant.png" />

Types are generated from the daemon's reviewed OpenAPI contract. Runtime requests
use `openapi-fetch`, with bearer authentication and redirect rejection.

```sh
pnpm install --frozen-lockfile
pnpm generate
pnpm typecheck
pnpm build
pnpm test
```

```ts
import {createOkytes} from "@okytes/sdk";

const {api,events} = createOkytes({
  baseUrl: "http://127.0.0.1:17841/",
  token: privateToken,
});

const {data,error} = await api.GET("/v1/jobs", {
  params: {query: {offset: 0}},
});
```

Use a read-only token for snapshots or a control token for job management. Settings
and token administration require admin scope. Never put tokens in URLs or logs.
Plaintext access is restricted to loopback; remote endpoints require trusted HTTPS.

`events(signal)` yields typed `Changed` and `Resync` messages. After `Resync`, fetch
and replace an authoritative snapshot. Reconnect after a closed stream; revocation
or expiry requires a valid new token. An AbortSignal or generator return cancels
the reader. Stream frames are bounded to 128 KiB.

The live Rust harness starts the production API and fault server, runs the built
SDK through Node, and checks SHA-256 output:

```sh
cargo test -p okytes-daemon --test sdk -- --ignored
```

This test is explicit because it requires Node and built SDK assets. CI runs it
after SDK generation/build, independently of the normal Rust-only test suite.
