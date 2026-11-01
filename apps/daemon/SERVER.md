# Running Okytes headless on a server

`okytes-daemon` is Okytes without a desktop window. It runs the same core
engine as the desktop app (HTTP/HTTPS, FTP/FTPS/SFTP, BitTorrent, HLS/DASH,
crash-safe checkpoints, verification, adaptive connections, schedules and
bandwidth limits) and exposes everything through an authenticated HTTP API.

Use it on a home server, NAS, VPS or in a container, and control it from:

- the REST API and live event stream (`/v1/...`, [OpenAPI contract](api/openapi-v1.json)),
- the [Rust, TypeScript and Python SDKs](../../dev-tools/sdk/docs/overview.md),
- `okytes-cli` with `--daemon-url`,
- the desktop app attached to it (`OKYTES_DAEMON_URL`),
- plugins and the [MCP server](../mcp/README.md) with restricted tokens.

## How it works

```
 clients                         okytes-daemon (one process)                     disk
┌──────────────────┐   HTTPS   ┌───────────────────────────────────────────┐  ┌──────────────┐
│ okytes-cli/watch │──────────▶│ API layer: token auth, scopes, permissions,│  │ data dir     │
│ SDKs (Rust/TS/Py)│  Bearer   │ rate limits, Host/Origin checks, OpenAPI   │  │  okytes.db   │
│ desktop app      │  token    │            │                    ▲          │  │  (queue,     │
│ plugins / MCP    │◀──────────│            ▼                    │ SSE      │  │  checkpoints,│
│ your scripts     │  events   │ queue owner: admission, priorities,        │  │  tokens)     │
└──────────────────┘           │ schedules, bandwidth policy                │  │              │
                               │            │                               │  │ downloads/   │
                               │            ▼                               │  │  files       │
                               │ core engine: probe → segment → parallel    │─▶│  .part files │
                               │ ranged requests → write → fsync →          │  └──────────────┘
                               │ checkpoint → verify SHA-256 → publish      │
                               │ (HTTP, FTP/SFTP, torrent, HLS/DASH)        │
                               └───────────────────────────────────────────┘
```

1. **One owner.** The daemon holds an exclusive lease on its data directory, so
   no other Okytes process can write the same queue. Clients never open the
   database; they only call the API.
2. **Queue.** Added links become durable jobs in SQLite immediately. The queue
   owner starts as many as the concurrency setting and schedule allow, by priority.
3. **Engine.** Each job is probed (size, range support, ETag), split into
   segments, and downloaded with adaptive parallel connections. Bytes are written
   and synced before a checkpoint records them, so a crash or power cut loses at
   most the unsynced tail.
4. **Verification.** A finished file is hashed (SHA-256), checked against an
   expected hash if you gave one, and only then moved to its final name.
5. **Events.** Every state change is published on `GET /v1/events`
   (server-sent events). Clients redraw from these, and reload a snapshot when
   they receive `Resync`.
6. **Restart.** On start, the daemon reopens the queue and resumes interrupted
   jobs from their checkpoints, after confirming the remote file has not changed.

Because the daemon is the only owner, you can close every client, including the
terminal, desktop or SSH session, and downloads keep running.

## 1. Quick start (binary)

```sh
cargo build --release --locked -p okytes-daemon -p okytes-cli
./target/release/okytes-daemon --data-dir /srv/okytes/data --downloads /srv/okytes/downloads
```

Building from source first needs `pnpm --dir apps/desktop install --frozen-lockfile`,
`pnpm --dir apps/desktop build:quick` and `node dev-tools/scripts/prepare-vendor.mjs`.

On first start the daemon writes an owner-only admin token to
`<data-dir>/daemon-admin.token`. It is never logged. Test the API:

```sh
TOKEN=$(cat /srv/okytes/data/daemon-admin.token)
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:17841/v1/capabilities
curl -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
     -d '{"urls":["https://example.com/file.iso"],"destination":null,"allow_duplicates":false}' \
     http://127.0.0.1:17841/v1/jobs
```

Then issue narrower tokens for applications instead of sharing the admin token
(see [Tokens for applications](#5-tokens-for-applications)).

## Using it from the terminal

`okytes-cli` talks to the daemon when you pass `--daemon-url` and `--token-file`
(or set `OKYTES_DAEMON_URL` and `OKYTES_DAEMON_TOKEN_FILE` once):

```sh
export OKYTES_DAEMON_URL=http://127.0.0.1:17841/
export OKYTES_DAEMON_TOKEN_FILE=/srv/okytes/data/daemon-admin.token

okytes-cli add https://example.com/ubuntu.iso                       # queue a download
okytes-cli add https://example.com/a.zip --sha256 <hex> --filename a.zip
okytes-cli add https://example.com/big.iso --run                    # wait until it finishes
okytes-cli torrent "magnet:?xt=urn:btih:…"                          # torrent or magnet
okytes-cli media-inspect https://example.com/video.m3u8             # list HLS/DASH tracks
okytes-cli watch                                                    # live view (below)
okytes-cli list --json                                              # history for scripts
okytes-cli status <id>
okytes-cli pause <id>      okytes-cli resume <id>      okytes-cli cancel <id>
okytes-cli queue <id> 50                                            # priority -100..100
okytes-cli quick --open                                             # local Quick Download page
```

Add `--json` to any command for machine-readable output.

### Live view: `okytes-cli watch`

```
Okytes  2 downloading  ·  18.4 MiB/s total  ·  21:04:37  ·  Ctrl+C to exit

STATE       PROGRESS                      %       SPEED      ETA  NAME
Downloading ██████████████░░░░░░░░░░   58.3   12.1 MiB/s    1m42s  ubuntu-24.04.iso
Downloading ████░░░░░░░░░░░░░░░░░░░░   17.0    6.3 MiB/s    9m05s  dataset.tar.zst
Queued      ░░░░░░░░░░░░░░░░░░░░░░░░    0.0           —        —  backup.zip
Failed      ██████░░░░░░░░░░░░░░░░░░   25.0           —        —  report.pdf
            URL expired or invalid: [link refused with 403]
Completed   ████████████████████████  100.0           —        —  photos.7z
```

- `--interval 2` redraws every 2 seconds (1–60).
- `--active` hides finished downloads.
- It works over SSH and in any terminal that understands ANSI colours,
  including Windows Terminal.

Closing `watch` (Ctrl+C) never affects downloads.

### Fixing an expired link from the terminal

```sh
okytes-cli status <id>                       # shows the error
curl -H "Authorization: Bearer $(cat $OKYTES_DAEMON_TOKEN_FILE)" -H 'content-type: application/json' \
     -d '{"kind":"Refresh","value":"https://example.com/new-signed-link"}' \
     $OKYTES_DAEMON_URL/v1/jobs/<id>/control
okytes-cli resume <id>
```

## 2. Docker

The repository's [Dockerfile](../../Dockerfile) builds a small image with only
`okytes-daemon` and `okytes-cli`. It runs as an unprivileged user, with data
in `/data` and downloads in `/downloads`.

```sh
docker build -t okytes-daemon:0.0.2 .
```

The daemon never serves the API in plain text on a public address. Choose one
of the two modes in [compose.yaml](../../compose.yaml).

### Mode A: the daemon handles TLS

Mount a certificate (for example from Let's Encrypt) and name the exact
host:port clients will use:

```sh
docker compose up -d okytes
```

```yaml
command: [--remote, --bind=0.0.0.0:17841, --cert=/certs/fullchain.pem,
          --key=/certs/privkey.pem, --host=downloads.example.com:17841]
```

Clients use `https://downloads.example.com:17841/`. `--host` must match the
`Host` header exactly; repeat it for several names.

### Mode B: reverse proxy on the same host

The daemon stays on `127.0.0.1` (host networking) and Caddy or nginx provides
HTTPS. In this mode the daemon accepts only the `Host` value
`127.0.0.1:17841`. That is a protection against DNS rebinding, so the proxy
must rewrite it:

```sh
docker compose --profile proxy up -d okytes-local
```

Caddy:

```caddy
downloads.example.com {
    reverse_proxy 127.0.0.1:17841 {
        header_up Host 127.0.0.1:17841
        flush_interval -1          # stream /v1/events without buffering
    }
}
```

nginx:

```nginx
location / {
    proxy_pass http://127.0.0.1:17841;
    proxy_set_header Host 127.0.0.1:17841;
    proxy_http_version 1.1;
    proxy_buffering off;           # server-sent events
    proxy_read_timeout 1h;
    client_max_body_size 2m;
}
```

### Reading the admin token in a container

```sh
docker compose exec okytes cat /data/daemon-admin.token
```

Store it in a secrets manager. Anyone with this token controls the daemon.

## 3. systemd (Linux, without containers)

Edit and install [services/okytes-daemon.service](services/okytes-daemon.service)
as a user service (see [services/README.md](services/README.md)). For TLS, add
`--remote --bind 0.0.0.0:17841 --cert … --key … --host …` to `ExecStart`.

## 4. Downloads and storage

- `--downloads` is the default destination. Requests from non-admin tokens can
  only target folders inside it.
- The data directory holds the SQLite queue, checkpoints and tokens. Back it up
  while the daemon is stopped, or use SQLite's online backup.
- `SIGTERM` (and `docker stop`) stops new work, writes synced checkpoints and
  leaves interrupted jobs queued for the next start. Allow up to 90 seconds.

## 5. Tokens for applications

| Need | Token |
|---|---|
| Dashboard or monitoring | `scope: "read_only"` |
| Add and control downloads | `scope: "control"` |
| A specific integration | `scope: "control"` plus `permissions` such as `["jobs:add","jobs:read"]` |
| Settings and token management | `admin` (keep to yourself) |

```sh
curl -H "Authorization: Bearer $ADMIN" -H 'content-type: application/json' \
     -d '{"scope":"control","lifetime_secs":2592000,"permissions":["jobs:add","jobs:read"],"label":"my-app"}' \
     https://downloads.example.com/v1/tokens
```

The response contains the token once. Tokens expire (at most 90 days) and can be
revoked with `DELETE /v1/tokens/{id}`. For plugin manifests,
`okytes-cli plugin-grant` does this for you (see [the plugin guide](../../plugins/README.md)).

## 6. Main API routes

| Route | Purpose |
|---|---|
| `GET /v1/jobs`, `POST /v1/jobs` | List downloads; add up to 100 links |
| `POST /v1/submit` | Add one download with filename, SHA-256 and conflict policy |
| `GET /v1/jobs/{id}`, `GET /v1/jobs/{id}/telemetry` | Details; live connections and segments |
| `POST /v1/jobs/{id}/control` | Pause, Resume, Cancel, Restart, Remove, Rename, Priority, Refresh (new link) |
| `GET /v1/events` | Server-sent events; reconnect and resync on `Resync` |
| `POST /v1/media/inspect`, `/v1/torrents`, `/v1/torrents/inspect` | Media tracks and torrents |
| `POST /v1/archive/inspect`, `/v1/crawl`, `/v1/playlist/expand` | ZIP preview, website crawl, playlists |
| `GET`/`PUT /v1/settings`, `/v1/config` | Queue, bandwidth and engine settings (admin to change) |

The complete, versioned contract is `GET /v1/openapi.json`. Regenerate the
committed copy with `okytes-daemon --export-openapi apps/daemon/api/openapi-v1.json`.

## 7. Limits and security defaults

- Every route requires a token, including the contract and events. There is no
  anonymous access and no default password.
- 1 MiB request bodies, 600 requests per minute per token, at most 16 event
  subscribers, and short header and handshake deadlines.
- Browser requests that carry an `Origin` header are refused unless that origin
  is listed with `--origin`.
- The Quick Download page (`--quick`) is local-only and cannot be combined with
  remote mode.
- LAN Share peer transfers (`--peer-bind`) are optional and use their own
  mutually authenticated channel.
