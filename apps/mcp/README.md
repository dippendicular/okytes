# Okytes MCP server

`okytes-mcp` lets a local AI assistant manage Okytes downloads through the
[Model Context Protocol](https://modelcontextprotocol.io). It speaks MCP over
stdio and forwards a small set of tools to the Okytes daemon. It holds no
engine of its own.

> **Status:** preview in 0.0.2.

## Tools

| Tool | What it does | Daemon permission |
|---|---|---|
| `list_downloads` | Lists downloads with state and progress (`offset` for older history) | `jobs:read` |
| `get_download` | One download's state, progress, retries and last error | `jobs:read` |
| `add_download` | Starts an HTTP(S), FTP or SFTP download into your download folder | `jobs:add` |
| `pause_download`, `resume_download` | Pauses or resumes a download | `jobs:control` |
| `cancel_download` | Cancels and deletes partial data (marked destructive) | `jobs:control` |

Download links shown to the assistant have their query string removed, because
signed links carry credentials there.

## Setup

1. Run the Okytes daemon (see [the daemon README](../daemon/README.md)).
2. Build the server:

   ```sh
   cargo build -p okytes-mcp --release
   ```

3. Grant it a restricted token using its manifest ([okytes-plugin.json](okytes-plugin.json)):

   ```sh
   okytes-cli --daemon-url http://127.0.0.1:17841/ --token-file <admin token> plugin-grant apps/mcp
   ```

   Review the listed permissions and type `yes`. The token is written to
   `apps/mcp/.okytes-token` and never printed.

4. Add the server to your MCP client. For example, in a client configuration file:

   ```json
   {
     "mcpServers": {
       "okytes": {
         "command": "/path/to/okytes-mcp",
         "env": {
           "OKYTES_DAEMON_URL": "http://127.0.0.1:17841/",
           "OKYTES_PLUGIN_TOKEN_FILE": "/path/to/okytes/apps/mcp/.okytes-token"
         }
       }
     }
   }
   ```

`OKYTES_DAEMON_TOKEN_FILE` is also accepted, but a plugin token is strongly
recommended: the daemon then refuses anything outside the three permissions
above, whatever the assistant asks for.

## Revoking access

```sh
okytes-cli --daemon-url http://127.0.0.1:17841/ --token-file <admin token> plugin-list
okytes-cli --daemon-url http://127.0.0.1:17841/ --token-file <admin token> plugin-revoke <token id>
```

## Protocol notes

- Supports protocol versions `2025-06-18`, `2025-03-26` and `2024-11-05`.
- Implements `initialize`, `ping`, `tools/list` and `tools/call`. Batches are refused.
- Tool failures are returned as tool results with `isError: true`, so the
  assistant can read and explain them.
- stdout carries protocol messages only; diagnostics go to stderr.
