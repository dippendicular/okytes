# `@okytes/sdk`

Typed Node.js client for an existing authenticated Okytes daemon. It never opens the daemon database or starts a service. Node.js 20+ and ESM are supported.

```ts
import { OkytesClient } from "@okytes/sdk";

const client = new OkytesClient({
  endpoint: "http://127.0.0.1:17841/",
  token: () => process.env.OKYTES_TOKEN ?? "",
});

console.log(await client.capabilities());
```

Plain HTTP is restricted to loopback. Use verified HTTPS for remote access. A destination path always refers to the daemon host.
