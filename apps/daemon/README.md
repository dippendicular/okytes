# Okytes daemon

<img width="360" alt="Okytes" src="../../assets/images/brand/okytes_logo_transperant.png" />

The daemon owns the existing engine and durable queue in a headless process.
Desktop, CLI and browser clients can disconnect without stopping its downloads.
Its HTTP API is authenticated on every route, including the contract and events.

## Run locally

From the repository root:

```sh
cargo build -p okytes-daemon --locked
```

Use a dedicated absolute profile directory:

```powershell
.\target\debug\okytes-daemon.exe --data-dir D:\OkytesData --downloads D:\Downloads
```

On Linux/macOS, use `./target/debug/okytes-daemon` with absolute paths. The default
listener is `127.0.0.1:17841`. `--bind` changes the loopback address/port. No network
interface is enabled by default, and no management route is anonymous.

First startup creates an owner-protected `daemon-admin.token` file. The server
stores only SHA-256 token digests in its SQLite preferences; that bootstrap file
is a private client credential. It is not printed to logs. Keep the profile and
token files private. Tokens expire, can be revoked individually, and have one of
three scopes:

| Scope | Access |
|---|---|
| `read_only` | Jobs, history, settings/config reads, events and API contract. |
| `control` | Read access plus add, submit, capture, pause/resume/cancel and queue controls. Destination overrides stay within the configured download root. |
| `admin` | Control access plus settings/config changes, arbitrary safe destination overrides and token administration. |

Issue scoped credentials through authenticated `POST /v1/tokens`; its response
contains the new token once. `GET /v1/tokens` returns metadata only, and
`DELETE /v1/tokens/{id}` revokes it. Only admin tokens can use these routes.
Store new client tokens in private files rather than shell arguments or URLs.
Keep a working admin credential before revoking the last one. There is no default
password or unauthenticated recovery endpoint.

## Attach clients

Set these process environment variables before starting the desktop or browser:

```powershell
$env:OKYTES_DAEMON_URL = 'http://127.0.0.1:17841/'
$env:OKYTES_DAEMON_TOKEN_FILE = 'D:\OkytesData\daemon-admin.token'
```

The desktop uses the same native commands and UI in both modes. Embedded mode
remains the default when neither variable is present. A partial or invalid daemon
configuration fails explicitly; it does not silently start another engine.

The native browser host inherits these variables from its browser process. Use a
control token for that host. Its extension allowlist and caller checks still apply;
only its forwarding destination changes. To switch back to the embedded backend,
remove both daemon environment variables.

CLI clients can also choose a daemon explicitly:

```powershell
.\target\debug\okytes-cli.exe --daemon-url http://127.0.0.1:17841/ --token-file D:\OkytesData\daemon-admin.token --json list
```

CLI commands retain their record JSON and filename/hash/conflict options.
The daemon schedules queued jobs automatically; `add --run` and `resume --run`
wait for completion in the foreground. `run` waits for eligible jobs owned by the
daemon. `--db` selects an embedded profile and does not select a remote database.
Transport configuration changes apply on the next daemon startup. Default folder
and simultaneous-download changes are committed with queue preferences and apply
to current admission; already-running jobs retain their chosen destinations.

## API and events

The reviewed, generated contract is [api/openapi-v1.json](api/openapi-v1.json).
The same contract is available at authenticated `GET /v1/openapi.json`.
All operations use `/v1`; breaking changes require a new API version.

| Route | Methods | Purpose |
|---|---|---|
| `/v1/jobs` | GET, POST | Bounded snapshot page or batch creation. |
| `/v1/jobs/{id}` | GET | One job and diagnostics. |
| `/v1/jobs/{id}/control` | POST | Tagged queue action. |
| `/v1/submit` | POST | Single creation with filename, hash and conflict policy. |
| `/v1/pause-all` | POST | Atomic bulk admission/control operation. |
| `/v1/settings` | GET, PUT | Runtime desktop/queue preferences. |
| `/v1/config` | GET, PUT | Validated engine configuration. |
| `/v1/records`, `/v1/records/{id}` | GET | Legacy CLI records; session headers are omitted. |
| `/v1/capture` | POST | Idempotent M3 browser handoff. |
| `/v1/events` | GET | Authenticated server-sent events. |
| `/v1/tokens`, `/v1/tokens/{id}` | GET, POST / DELETE | Admin credential management. |

Event messages contain `Changed` engine events or `Resync`. A new subscriber gets
`Resync`; a lagged subscriber must replace its state from `GET /v1/jobs`.
The broadcast buffer is bounded, and at most 16 subscribers are admitted.
Expired/revoked tokens close existing streams. Clients reconnect and fetch a
snapshot after connection loss. The desktop continues its authoritative snapshot
refresh, so missed progress messages cannot leave its displayed state permanent.

Limits include 64 accepted connections, 32 concurrent requests, 1 MiB request
bodies, 1,200 requests/minute globally and 600/minute per authenticated token.
HTTP header and TLS handshake deadlines are five seconds; handler/body processing
has an eight-second deadline. Native clients cap JSON responses at 16 MiB.

Host authorities must match the configured listener exactly. Browser requests
with an Origin are rejected by default. There is no wildcard CORS policy.

## Remote TLS

Remote access requires every option below; plaintext non-loopback binding fails
before startup:

```sh
okytes-daemon --data-dir /absolute/profile --remote --bind 0.0.0.0:18443 \
  --cert /absolute/server-chain.pem --key /absolute/server-key.pem \
  --host downloads.example.com:18443
```

Use a certificate trusted by your clients, containing the endpoint's hostname.
The private key stays on the server. Native clients retain TLS certificate
validation and refuse to follow redirects with tokens. Explicit `--origin`
values may allow selected HTTPS browser origins; authentication remains required.
Protect the listener with the host's normal network access policy.

## Development and verification

```sh
cargo test -p okytes-daemon
cargo test -p okytes-cli --test daemon
cargo test -p okytes-native-host --test daemon
```

After reviewing an intentional contract change:

```powershell
$env:OKYTES_UPDATE_API = '1'
cargo test -p okytes-daemon --test contract
Remove-Item Env:OKYTES_UPDATE_API
```

Regenerate/build the SDK with pnpm in `packages/sdk-typescript`, then run the
explicit live integration check:

```sh
cargo test -p okytes-daemon --test sdk -- --ignored
```

The daemon crash test forcibly kills an actual process, restarts it and checks
resumed bytes and the completed SHA-256. Other tests cover scope/auth boundaries,
expiry/revocation, malformed/oversized input, flooding, slow headers, destination
handling, conflicting controls and graceful checkpoint draining.

Service templates live under `services/`. They are configuration examples, not
installers. Platform supervisor execution and remote TLS deployment require their
own verification; a generated template is not evidence of a running service.
