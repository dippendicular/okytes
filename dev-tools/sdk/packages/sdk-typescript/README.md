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

Use `download`, `add`, `inspectMedia`, `inspectTorrent`, `addTorrent`, `authorizeFile`, and `sendToPeer` for high-level operations. Check `supports(protocol, operation)` before optional protocol calls. Creation calls use durable idempotency keys, retry only retryable failures with bounded backoff, and the event iterator reconnects with its last SSE cursor before emitting `Resync`.

## Plugins (preview, 0.0.2)

The separate `@okytes/sdk/plugin` entry point (Node.js only) loads and
validates an `okytes-plugin.json` manifest and connects with the plugin's own
permission-restricted token from `OKYTES_PLUGIN_TOKEN_FILE`:

```ts
import { Plugin, loadManifest } from "@okytes/sdk/plugin";

const plugin = await Plugin.fromEnvironment(await loadManifest("."));
plugin.require("events:read");
for await (const update of plugin.client.events()) console.log(update.kind);
```

See the [plugin guide](../../../../plugins/README.md) and the official
[webhook-notify](../../../../plugins/webhook-notify/) example.

Plain HTTP is restricted to loopback. Use verified HTTPS for remote access. A destination path always refers to the daemon host. FTP/SFTP credentials remain request-local; SFTP requires a confirmed host-key fingerprint. LAN operations require an already paired and approved device.
