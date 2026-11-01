# Webhook notifications (official example plugin)

Posts a short message when an Okytes download completes or fails:

- `✅ ubuntu.iso finished`
- `❌ backup.zip failed: …`

Messages contain the file name and outcome only. Links are removed from error
text, because download links can contain signed tokens.

**Permissions:** `events:read`, `jobs:read`. The plugin cannot start, change
or delete downloads.

## Run

Requires Node.js 20+ and the Okytes daemon.

```sh
# 1. Build the SDK once
pnpm --dir dev-tools/sdk/packages/sdk-typescript install --frozen-lockfile
pnpm --dir dev-tools/sdk/packages/sdk-typescript build

# 2. Install the plugin's dependency
cd plugins/webhook-notify && npm install --no-package-lock

# 3. Grant its token (review the permissions, type "yes")
okytes-cli --daemon-url http://127.0.0.1:17841/ --token-file <admin token> plugin-grant plugins/webhook-notify

# 4. Start it
OKYTES_DAEMON_URL=http://127.0.0.1:17841/ \
OKYTES_PLUGIN_TOKEN_FILE=plugins/webhook-notify/.okytes-token \
OKYTES_WEBHOOK_URL=https://ntfy.sh/your-private-topic \
OKYTES_WEBHOOK_FORMAT=ntfy \
node plugins/webhook-notify/index.mjs
```

| Variable | Values |
|---|---|
| `OKYTES_WEBHOOK_URL` | `https://` address. Plain `http://` is allowed only for this computer. |
| `OKYTES_WEBHOOK_FORMAT` | `ntfy` (plain text with title and tag), `discord` (`{content}`), `slack` (`{text}`) or `json` (`{text, failed}`, the default). |

Each download is announced once per run. Restarting the plugin does not replay
old completions.
