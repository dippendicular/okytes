# Okytes plugins (preview)

A plugin is a separate program that talks to the Okytes daemon with its **own
revocable token**. The token carries an explicit list of permissions, and the
daemon checks that list on every request. A plugin cannot reach anything it
was not granted, whatever its code tries.

> **Status:** foundation in 0.0.2. Manifest schema 1 may gain fields. There is
> no plugin marketplace and Okytes does not launch plugins for you yet; you run
> them yourself.

## How it works

1. A plugin ships an `okytes-plugin.json` manifest that lists the permissions it needs.
2. You review it and grant a token with `okytes-cli plugin-grant <plugin folder>`.
3. The plugin connects with `OKYTES_DAEMON_URL` and `OKYTES_PLUGIN_TOKEN_FILE`.
4. You can list and revoke plugin tokens at any time.

Plugins require the daemon (see [the daemon README](../apps/daemon/README.md)).

## Manifest: `okytes-plugin.json`

```json
{
  "schema": 1,
  "id": "org.example.my-plugin",
  "name": "My plugin",
  "version": "1.0.0",
  "description": "One line shown when the user grants access.",
  "permissions": ["jobs:read", "events:read"]
}
```

| Field | Rules |
|---|---|
| `schema` | Must be `1`. |
| `id` | Reverse-domain, lowercase letters, digits and hyphens, at least two parts, at most 100 characters. |
| `name` | Required, at most 64 characters. |
| `version` | Required, at most 32 characters. |
| `description` | Optional, at most 200 characters. |
| `permissions` | At least one, each listed once. Unknown fields are rejected. |

## Permissions

| Permission | Allows | Daemon routes |
|---|---|---|
| `jobs:read` | See downloads, including URLs and file paths | `GET /v1/jobs`, `/v1/jobs/{id}`, `/v1/jobs/{id}/telemetry`, `/v1/records…`, `GET /v1/torrents/{id}` |
| `jobs:add` | Start downloads inside the configured download folder | `POST /v1/jobs`, `/v1/submit`, `/v1/torrents` |
| `jobs:control` | Pause, resume, cancel, remove, reprioritize and replace links | `POST /v1/jobs/{id}/control` |
| `events:read` | Live change notifications | `GET /v1/events` |
| `media:inspect` | Inspect media, torrents, ZIP archives, websites and playlists without creating jobs | `POST /v1/media/inspect`, `/v1/torrents/inspect`, `/v1/archive/inspect`, `/v1/crawl`, `/v1/playlist/expand` |
| `settings:read` | Read settings | `GET /v1/settings` |

Plugin tokens can never use token management, peers (LAN Share), configuration
changes, browser capture or file-login credentials. They are never admin tokens.

## Granting, listing and revoking

```sh
# Review and grant (asks you to type "yes"; use --yes in scripts)
okytes-cli --daemon-url http://127.0.0.1:17841/ --token-file <admin token> plugin-grant plugins/webhook-notify

# Options: --days 1-90 (default 30), --out <file> (default .okytes-token beside the manifest)
okytes-cli --daemon-url http://127.0.0.1:17841/ --token-file <admin token> plugin-list
okytes-cli --daemon-url http://127.0.0.1:17841/ --token-file <admin token> plugin-revoke <token id>
```

The token file is created new (an existing file is never overwritten) and the
token is never printed. `.okytes-token` is git-ignored in this repository.

## Writing a plugin

### TypeScript / Node.js

```ts
import { Plugin, loadManifest } from "@okytes/sdk/plugin";

const plugin = await Plugin.fromEnvironment(await loadManifest("."));
plugin.require("jobs:read");
const page = await plugin.client.jobs();
```

### Python

```python
from okytes import Plugin, PluginManifest

plugin = Plugin.from_environment(PluginManifest.load("."))
plugin.require("jobs:read")
snapshot = await plugin.client.jobs()
```

### Rust

```rust
use okytes_sdk::plugin::{Manifest, Permission, Plugin};

let plugin = Plugin::from_environment(Manifest::load(std::path::Path::new("."))?)?;
plugin.require(Permission::JobsRead)?;
```

`require` fails early with a clear message when you call outside your declared
permissions. The daemon enforces the same rule independently.

## Official example

[webhook-notify](webhook-notify/) posts to ntfy, Discord, Slack or any JSON
webhook when a download completes or fails, using only `events:read` and `jobs:read`.

## Guidelines

- Request the fewest permissions that work.
- Never log or forward download URLs without need; they can contain signed tokens.
- Treat the token file as a password.
