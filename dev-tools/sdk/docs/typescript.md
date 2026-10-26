# TypeScript and Node.js SDK

`@okytes/sdk` is an ESM package for Node.js 20 or newer. Its public models are generated from the reviewed OpenAPI document.

```ts
import { OkytesClient } from "@okytes/sdk";
const client = new OkytesClient({ endpoint: "http://127.0.0.1:17841/", token: () => process.env.OKYTES_TOKEN ?? "" });
console.log(await client.capabilities());
```

The token provider is evaluated for every request. SDK errors expose stable `code`, `category`, `retryable`, and HTTP `status` fields without including the token. `events()` reconnects with bounded exponential backoff and emits `Resync` after a gap. This Node client is not a general webpage bridge.
