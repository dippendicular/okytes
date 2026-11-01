# Changelog

Release sections use `## [MAJOR.MINOR.PATCH]` followed by Added, Changed, Fixed
or Security entries. Release automation requires an exact matching section.

## [0.0.2]

Versioning: `MAJOR.BETA.TEST`. 0.0.2 is the second test release. The previous
release, labelled 0.1.0 at the time, is 0.0.1.

### Added

- Per-download window: live file map, connection table, overview with average
  and peak speed, Pause/Resume/Cancel, and diagnostics (response status, range
  support, ETag, Last-Modified, concurrency, retries, verification).
- Per-download speed limiter and completion options (open file, show in folder,
  notify, upload to cloud storage), with an option to open the window when a
  download starts.
- Engine connection telemetry, sampled on demand without slowing transfers.
- Expired-link recovery: the `Refresh` control action replaces a link on a
  paused or failed download and keeps progress when the server identifies the
  same file.
- Website crawler with depth, page and type limits that honours robots.txt.
- Remote ZIP preview (including ZIP64) using byte-range requests.
- M3U, PLS and XSPF playlist expansion.
- Audio/video/subtitle merging into MKV with ffmpeg, without re-encoding.
- Google Drive, Dropbox and OneDrive upload connectors with resumable,
  retried chunked uploads and progress.
- Permission-restricted daemon tokens (`jobs:read`, `jobs:add`, `jobs:control`,
  `events:read`, `media:inspect`, `settings:read`).
- Plugin foundation: `okytes-plugin.json` manifests, `okytes-cli plugin-grant`,
  `plugin-list` and `plugin-revoke`, plugin support in the Rust, TypeScript
  (`@okytes/sdk/plugin`) and Python SDKs, and the official `webhook-notify` example.
- `okytes-mcp`: local Model Context Protocol server for AI assistants.
- `okytes-cloud`: self-hosted accounts (email, GitHub, Google) with an
  allowlist, device pairing, phone-to-PC queue, two-way history sync, analytics,
  dashboard, optional SMTP confirmation and password reset, and an owner
  `reset-link` command.
- Desktop pairing with Okytes Cloud.
- `okytes-cli watch`: live terminal view of daemon downloads (progress, speed,
  ETA; `--active`, `--interval`).
- Headless server packaging: `Dockerfile` (daemon and CLI only, non-root),
  `compose.yaml` with TLS and reverse-proxy modes, and a server deployment guide.
- Daemon routes: `/v1/jobs/{id}/telemetry`, `/v1/archive/inspect`, `/v1/crawl`,
  `/v1/playlist/expand`.
- Browser extension popup switch for automatic capture, toolbar badge
  feedback, and actionable error messages.

### Changed

- Version numbering corrected to 0.0.2 across the workspace, apps, SDKs and extensions.
- The download details dialog uses the new live overview instead of one row per segment.
- Removing a download also removes its speed limit and completion options.
- The main window reloads settings before saving, so changes made in download
  windows are not overwritten.
- Extension handoffs retry once with the same request ID after a timeout or
  disconnect, so a slow acceptance does not leave a duplicate browser download.
- Extension right-click menus are re-registered at browser start; Firefox uses its `menus` API.
- SDK compatibility range is `>=0.0.2,<0.1.0`.

### Fixed

- HTML pages (login, error or expired-link pages) served with `200 OK` in place
  of a named file are refused instead of being saved as the download.
- Browser extension: native-host and app errors were silently swallowed; the
  right-click menu could fail without a tab; Chromium type rules never matched
  because the filename is assigned after the download starts; Chromium hid
  Referer and Cookie headers from session handoff.

### Security

- Plugin and MCP tokens are limited to declared permissions and can never
  manage tokens, peers, configuration or credentials.
- Refreshing a link to a different site drops captured browser cookies.
- Cloud server: Argon2 password hashing, sign-in lockout, single-use pairing
  and email tokens, rate limits, strict Content-Security-Policy, and session
  tokens kept out of URLs sent to servers.
- MCP, plugin, cloud and extension diagnostics strip link query strings, which
  can hold signed credentials.

## [0.0.1]

### Added

- Crash-safe HTTP/HTTPS downloads with durable checkpoints and SHA-256 verification.
- Desktop interface, CLI, authenticated daemon API and shared browser extensions.
- Adaptive scheduling, bandwidth limits and recurring traffic schedules.
- BitTorrent, magnet links, FTP, explicit FTPS and SFTP transfers.
- HLS and DASH VOD selection and crash-safe assembly for supported formats.
- Installer configurations, draft release automation and manually controlled signed updates.

### Release qualification

- Platform, browser and installer verification varies. Read the artifact labels
  and installation guide; a successful build is not a completed installer test.
- Live media, DRM handling, media remuxing, mobile, Flatpak and plugins are outside this release.
