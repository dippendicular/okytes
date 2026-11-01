# Okytes Cloud (self-hosted preview)

`okytes-cloud` is a small server you run yourself. It adds accounts, paired
devices, a "send to my PC" download queue, history sync across devices and a
web dashboard. Everything is stored in one local SQLite file. There is no
Okytes-hosted service.

> **Status:** preview in 0.0.2. The API may change before 1.0.

## What it does

| Feature | Description |
|---|---|
| Accounts | Email and password, GitHub, and Google sign-in. Only authorized accounts can sign up or sign in (see [Authorization](#authorization)). |
| Device pairing | The dashboard shows an 8-character code. It works once, expires after 10 minutes, and pairing attempts are rate-limited. |
| Cloud queue | Paste a link on any device (for example your phone's browser) and pick a paired PC. The PC starts it within about 10 seconds. |
| History sync | Each desktop uploads its download history every minute (link query strings removed). The dashboard and every paired desktop can see history from all devices. |
| Analytics | 30-day chart of completed downloads and totals by state. |
| Email | Optional SMTP for account confirmation and password-reset links. |

## Build and run

```sh
cargo build -p okytes-cloud --release
./target/release/okytes-cloud --db /path/to/okytes-cloud.db
```

Open <http://127.0.0.1:17900>. With no allowlist set, the first account you
create becomes the owner and further sign-ups are refused.

### Options and environment

| Flag / variable | Default | Meaning |
|---|---|---|
| `--bind` | `127.0.0.1:17900` | Listen address. Keep loopback unless a TLS proxy is in front. |
| `--db` | `okytes-cloud.db` | SQLite file. Back it up like any database. |
| `--public-url` / `OKYTES_CLOUD_PUBLIC_URL` | `http://127.0.0.1:17900` | Address users reach. Used for OAuth redirects and email links. |
| `--allow` / `OKYTES_CLOUD_ALLOW` | empty | Comma-separated emails or `@domain` entries that may use the server. |
| `OKYTES_CLOUD_GITHUB_CLIENT_ID`, `OKYTES_CLOUD_GITHUB_CLIENT_SECRET` | — | Enables "Continue with GitHub". |
| `OKYTES_CLOUD_GOOGLE_CLIENT_ID`, `OKYTES_CLOUD_GOOGLE_CLIENT_SECRET` | — | Enables "Continue with Google". |
| `OKYTES_CLOUD_SMTP_URL` | — | For example `smtps://user:password@smtp.example.com`. Enables email. |
| `OKYTES_CLOUD_MAIL_FROM` | — | Sender, for example `Okytes <okytes@example.com>`. Required with SMTP. |

## Authorization

- **With an allowlist** (`--allow you@example.com,@family.example`): only listed
  addresses can create accounts or sign in. Removing an address blocks that
  account immediately, including existing sessions.
- **Without an allowlist:** only the first account (the owner) can be created.
- OAuth sign-in accepts only emails the provider has verified. Accounts are
  linked by email, so a GitHub and a Google login with the same verified email
  share one account.
- Ten wrong passwords for an email lock it for 15 minutes. Passwords are stored
  as Argon2 hashes.

## Sign-in providers

Register an OAuth app with each provider and use this callback URL, replacing
the base with your `--public-url`:

| Provider | Where to register | Callback URL |
|---|---|---|
| GitHub | GitHub → Settings → Developer settings → OAuth Apps | `<public-url>/v1/auth/github/callback` |
| Google | Google Cloud Console → APIs & Services → Credentials → OAuth client (Web application) | `<public-url>/v1/auth/google/callback` |

Both flows use PKCE and a one-time state value.

## Email confirmation and password reset

With SMTP configured:

- New email/password accounts receive a confirmation link valid for one hour and
  cannot sign in until they open it.
- "Forgot password?" on the sign-in page sends a one-hour, single-use reset link.
  The page answers the same way whether or not the account exists.
- A reset ends every dashboard session. Paired devices keep working; remove a
  lost device from the dashboard.

Without SMTP, accounts need no confirmation, and the owner creates reset links
from the command line:

```sh
okytes-cloud --db /path/to/okytes-cloud.db --public-url https://okytes.example.com reset-link --email user@example.com
```

Give the printed link to the user privately. It works once and expires in one hour.

## Using it from a phone (TLS required)

Phones reach the server over your network, so put it behind HTTPS. Passwords
and session tokens must never cross a network in plain text. The server logs a
warning when it listens beyond loopback without an `https://` public URL.

### Caddy (automatic certificates)

```caddy
okytes.example.com {
    reverse_proxy 127.0.0.1:17900
}
```

```sh
okytes-cloud --bind 127.0.0.1:17900 --public-url https://okytes.example.com --allow you@example.com
```

### nginx

```nginx
server {
    listen 443 ssl;
    server_name okytes.example.com;
    ssl_certificate     /etc/letsencrypt/live/okytes.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/okytes.example.com/privkey.pem;
    client_max_body_size 4m;
    location / {
        proxy_pass http://127.0.0.1:17900;
        proxy_set_header Host $host;
    }
}
```

Keep `okytes-cloud` bound to `127.0.0.1` so only the proxy can reach it. For a
home network without a public domain, a private tunnel such as Tailscale
(with `tailscale serve`) gives you HTTPS without opening ports.

## Pair a desktop

1. In the dashboard, choose **Pair a device** and note the code.
2. In Okytes, open **Settings → Okytes Cloud**, enter the server address, the
   code and a name, and choose **Pair**.
3. The desktop now picks up queued links and syncs history every minute.
   **Downloads on my other devices** shows what your other desktops downloaded.

The desktop refuses plain `http://` for servers that are not on the same
computer. To unpair, choose **Unpair this computer**, and also remove the
device in the dashboard to revoke its token.

Cloud sync runs when Okytes uses its built-in engine. It is not available when
the desktop is attached to a separate daemon.

## Limits and privacy

- Queue: 500 pending items per account. History sync: 500 entries per request.
- History stores file name, link without its query string, state and sizes. The
  query is removed because signed links carry credentials there. A synced link
  therefore may not work for "Download here" if the site requires the signature.
- The queue cannot hand a browser login session to the PC. Links that need you
  to be signed in on a website will fail on the PC.

## API summary

All `/v1` routes except sign-in, pairing claims and email links need
`Authorization: Bearer <token>`. Dashboard sessions last 7 days; device tokens
last 365 days.

| Method and path | Who | Purpose |
|---|---|---|
| `GET /v1/config` | anyone | Enabled sign-in methods and email support |
| `POST /v1/auth/register`, `POST /v1/auth/login`, `POST /v1/auth/logout` | anyone / session | Email accounts |
| `GET /v1/auth/{github,google}/start` | anyone | Begin OAuth sign-in |
| `GET /v1/auth/verify?token=` | email link | Confirm an email address |
| `POST /v1/auth/forgot`, `POST /v1/auth/reset` | anyone | Password reset |
| `GET /v1/me` | session or device | Current account |
| `GET /v1/devices`, `POST /v1/devices/pair-code`, `DELETE /v1/devices/{id}` | dashboard | Manage devices |
| `POST /v1/devices/claim` | pairing code | Pair a device and receive its token |
| `GET /v1/queue`, `POST /v1/queue`, `DELETE /v1/queue/{id}` | session or device | Queue links |
| `POST /v1/queue/claim`, `POST /v1/queue/{id}/result` | device | Take and report queued links |
| `GET /v1/history`, `PUT /v1/history` | read: any / write: device | History sync |
| `GET /v1/analytics` | session or device | Activity summary |
