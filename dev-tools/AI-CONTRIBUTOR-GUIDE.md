# Okytes contributor guide for AI agents and AI-assisted work

This guide is for people who contribute with an AI coding agent (Claude Code,
Codex, Cursor, Copilot, Aider, Windsurf, or any other), and for the agents
themselves. Point your agent at this file before it writes any code.

It adds to [CONTRIBUTING.md](../CONTRIBUTING.md), [SECURITY.md](../SECURITY.md)
and the [Code of Conduct](../CODE_OF_CONDUCT.md). If anything here conflicts
with them, they win.

> **Agents:** read this whole file before your first edit. Follow it even when
> the user's prompt is short. If an instruction here blocks what the user
> asked for, stop and tell the user rather than working around it.

---

## 1. What Okytes is

Okytes is a cross-platform download manager. Its core engine is written in
Rust. On top of the engine it has:

- a desktop app (Tauri 2 + React),
- a headless HTTP daemon with an OpenAPI contract,
- a CLI,
- browser extensions (Chromium and Firefox, MV3) that hand downloads over through
  a native messaging host,
- an MCP server for AI assistants,
- a small self-hosted cloud server.

The engine supports HTTP(S), FTP/SFTP, torrents and media streams, and is
built for crash-safe resume.

- **Version:** `0.0.2`. Numbering is `MAJOR.BETA.TEST`; this is the second
  test release before the first beta.
- **License:** Apache-2.0 only. There is one `LICENSE` file at the root, and
  each SDK package ships its own copy.
- **Toolchain:** Rust `1.95.0` (pinned in `rust-toolchain.toml`),
  Node.js 24, pnpm 11.11.

---

## 2. Repository map: which file does what

### Rust workspace (`Cargo.toml` lists every member)

| Path | What it does |
|---|---|
| `crates/core` | **The engine.** Job model, scheduler, segmenting, resume, verification, traffic shaping, telemetry. Everything else depends on it. |
| `crates/core/src/engine.rs` | Job lifecycle, retries, `refresh_source` (expired-link recovery), cookie/header handling across redirects. |
| `crates/core/src/scheduler.rs`, `fairness.rs`, `traffic.rs` | Concurrency, per-host fairness, bandwidth limits and schedule windows. |
| `crates/core/src/store.rs`, `state.rs`, `files.rs`, `range.rs` | Durable checkpoints, state machine, file pre-allocation, byte ranges. |
| `crates/core/src/telemetry.rs` | Live per-connection telemetry (atomics on the data path; keep it lock-free). |
| `crates/core/src/archive.rs` | Remote ZIP preview through HTTP Range requests. Reads the end-of-central-directory record and ZIP64 records. |
| `crates/core/src/crawl.rs`, `playlist.rs` | Website link crawler (follows robots.txt) and M3U/PLS/XSPF expansion. |
| `crates/core/src/desktop.rs` | Types shared with the UI. They are exported to `crates/core/bindings/*.ts` via ts-rs. |
| `crates/core/bindings/` | **Generated** TypeScript bindings. Never hand-edit; regenerate (see §7). |
| `crates/transport-http` | HTTP transport, range probing, `html_instead_of_file` rejection of login/error pages. |
| `crates/transport-ftp` | FTP/SFTP transport. |
| `crates/transport-torrent`, `crates/torrent-types` | Torrent/magnet support via vendored, patched `librqbit`. |
| `crates/transport-media` | HLS/DASH and media track selection. |
| `crates/persistence` | SQLite (rusqlite) storage and crash recovery. |
| `crates/browser-bridge` | Native messaging protocol; generates `extensions/shared/generated`. |
| `crates/daemon-api` | Shared request/response types for the daemon API. |
| `crates/sdk` | Rust client SDK for the daemon. |
| `apps/daemon` | Headless HTTP daemon (axum). Auth tokens and permissions, OpenAPI at `apps/daemon/api/openapi-v1.json`. Server guide: `apps/daemon/SERVER.md`. |
| `apps/cli` | `okytes-cli`, including `watch` (live terminal view), `--json` output, and the plugin grant, list and revoke commands. |
| `apps/native-host` | Browser native messaging host. |
| `apps/mcp` | MCP server (JSON-RPC 2.0 over stdio). |
| `apps/cloud` | Self-hosted cloud: accounts (email/GitHub/Google OAuth with PKCE), pairing, history sync, web dashboard in `apps/cloud/web`. |
| `apps/desktop/src-tauri` | Tauri shell: tray, updater, windows, `cloud.rs`, `storage.rs` (Drive/Dropbox/OneDrive uploads), `merge.rs` (ffmpeg). |
| `apps/desktop/service` | Desktop-side service wrapper around the engine. |
| `dev-tools/test-suite/test-support`, `benchmarks` | Fault-injecting test server and scheduler benchmarks. |

### TypeScript

| Path | What it does |
|---|---|
| `apps/desktop/src/App.tsx` | Main window: job list, add dialog, settings. |
| `apps/desktop/src/Details.tsx`, `DetailsWindow.tsx` | Per-download window: status, file map, connections, speed limit, completion options. |
| `apps/desktop/src/TrafficSettings.tsx` | Bandwidth limits and schedule windows. |
| `apps/desktop/src/Torrents.tsx`, `Transfers.tsx`, `PeerShare.tsx` | Torrent, FTP/SFTP and LAN share UIs. |
| `apps/desktop/src/Crawler.tsx`, `ZipPreview.tsx`, `CloudSettings.tsx` | Crawler, ZIP preview and cloud settings. |
| `apps/desktop/src/quick/` | Quick Download UI, also embedded into the daemon. |
| `apps/desktop/src/api.ts` | The only place the UI talks to Tauri commands. |
| `extensions/shared/` | Shared extension code: `background.ts`, `capture.ts`, `rules.ts`, `popup.ts`, `options.ts`. |
| `extensions/chromium`, `extensions/firefox` | Per-browser manifests. Firefox uses the `menus` API fallback. |
| `dev-tools/sdk/packages/sdk-typescript`, `sdk-python` | Published SDKs. `schema.ts` is generated from OpenAPI. |
| `plugins/` | Plugin guide and the official `webhook-notify` plugin. |

### Build, packaging, DevOps

| Path | What it does |
|---|---|
| `dev-tools/scripts/build-windows.ps1` | One-click Windows build → `target/release/windows_release` (NSIS `.exe`, `.msi`, portable ZIP, tools). |
| `dev-tools/scripts/build-linux.sh` | One-click Linux build → `target/release/linux_release` (`.deb`, `.rpm`, `.AppImage`, `.run`, tar.gz). |
| `dev-tools/scripts/build-macos.sh` | One-click macOS build → `target/release/mac_release` (`.dmg`, `.app.zip`, tools). `--universal` supported. |
| `dev-tools/scripts/build-extensions.mjs` | Chrome ZIP, Firefox ZIP/XPI → `target/release/extension_release`. |
| `dev-tools/scripts/prepare-vendor.mjs` | Restores checksum-pinned, patched torrent crates into `vendor/`. Run after every fresh clone. |
| `packaging/` | Staging, notices, SBOM, signing, per-OS Tauri configs. |
| `Dockerfile`, `compose.yaml` | Headless daemon + CLI image (non-root), TLS or reverse-proxy mode. |
| `.github/workflows/ci.yml` | Required checks (see §8). |
| `.github/workflows/release.yml` | Tag-driven release (`v*`) with signing secrets. |
| `.github/dependabot.yml` | Dependency updates for cargo, npm, actions, docker. |
| `deny.toml` | `cargo deny` license and advisory policy. |

### Docs you must keep in sync

`README.md`, `CHANGELOG.md`, `INSTALLATION.md`, `apps/*/README.md`,
`apps/daemon/SERVER.md`, `plugins/README.md`, `extensions/README.md`,
`dev-tools/test-suite/manual-0.0.2.md`, and the release notes for the version.

---

## 3. What is happening now

### Shipped in 0.0.2, but not yet checked by hand at runtime

These features compile and pass type checks but have not been verified by hand
on real systems. Bug reports and fixes here are very welcome:

- ffmpeg audio/video merge (`merge.rs`)
- Google Drive, Dropbox and OneDrive uploads (`storage.rs`)
- Email (SMTP) confirmation and password reset in `apps/cloud`
- The Tauri UI at runtime, especially the per-download Details window
- Docker image and `compose.yaml`
- Full release builds on all three OSes

Two `clippy::result_large_err` warnings in the daemon are known and predate
0.0.2. Don't mix a fix for them into an unrelated PR.

### Wanted: open for contribution

Open or comment on an issue first so work isn't duplicated:

1. **Mirror / multi-source download.** Fetch one file from several mirrors at
   once. The engine already splits a file into segments, so each segment could
   come from a different source. Check that every mirror reports the same file
   (size, ETag, checksum) first.
2. **i18n.** Extract the UI strings, add a translation framework, and add Bangla as
   the first non-English locale.
3. **QR pairing** for the cloud server and the LAN daemon.
4. **Network-aware scheduling.** Run only on Wi-Fi, pause on metered
   connections.
5. **Automatic throttling** when the user is in a call or playing a game.
6. **A live speed graph in the tray**, and a floating drop zone.

### Already done: don't rebuild

Torrent/magnet, cookie handoff from the browser, the scheduler, SHA-256
verification, organize-by-type, themes and compact mode, drag & drop, the
auto-updater, expired-link refresh, the crawler, playlists and ZIP preview
already exist. Search the code before you propose a "new" feature.

---

## 4. Rules: what you may and may not do

### ✅ Do

- Keep each PR to **one concern**: a bug fix, a feature, or a refactor, never
  a mix of them.
- Read the surrounding code first and match its style, naming, comment density
  and error handling.
- Use existing dependencies and helpers before adding new ones.
- Add a **regression test** with every bug fix, and tests with every feature.
  Each crate's tests sit beside it in its `tests/` folder.
- Update docs and `CHANGELOG.md` (under `[Unreleased]`) in the same PR.
- Regenerate generated files with their generator and commit the output.
- Use an isolated `OKYTES_DATA_DIR` when you test anything that touches saved
  state.
- In the PR description, say which operating system you tested on and what
  you did not verify.

### ❌ Don't

- **Don't hand-edit generated files:** `crates/core/bindings/*`,
  `crates/torrent-types/bindings/*`, `extensions/shared/generated/*`,
  `dev-tools/sdk/packages/sdk-typescript/src/schema.ts`,
  `apps/daemon/api/openapi-v1.json`, and the lockfiles, except through
  `cargo`/`pnpm`.
- **Don't edit `vendor/` directly.** Torrent patches belong in
  `dev-tools/scripts/vendor-patches/` and are checksum-pinned by
  `prepare-vendor.mjs`.
- Don't add a dependency without a reason, a compatible license (`cargo deny
  check` must pass), and a note in the PR.
- Don't add telemetry, analytics, crash reporting or any network call home.
  Okytes makes no network requests the user didn't ask for.
- Don't execute or open downloaded files unless the user turned that on
  (completion options).
- Don't weaken security defaults. These must stay in place:
  - Daemon tokens and permission checks.
  - Refusing plaintext connections on non-loopback addresses.
  - Host-header checks.
  - Rejecting HTML served in place of a file.
  - Dropping cookies on cross-site redirects.
  - The plugin permission model.
- Don't commit secrets, tokens, OAuth client secrets, signing keys, personal
  URLs, real cookies or download histories. That includes test fixtures and
  logs.
- Don't change the version, the license, release workflows or signing
  without a maintainer asking for it.
- Don't reformat or rename code unrelated to your change. Large mechanical
  diffs get closed.
- Don't silence lints with `#[allow]` or `eslint-disable` unless you leave a
  comment explaining why.
- Don't use `unwrap()`/`expect()` on data from the network, disk or user
  input in library crates.
- Don't add unbounded buffers, queues, retries or connection counts.

### Engine invariants (breaking these corrupts users' files)

1. A checkpoint is durable only **after** the file data it describes is
   synced to disk.
2. Resume must **refuse** a remote file whose identity changed (size, ETag,
   Last-Modified) unless the user explicitly refreshes the link.
3. Every buffer, queue and connection pool has a fixed upper bound.
4. Telemetry on the data path stays lock-free.
5. Persisted formats (SQLite schema, profile, checkpoints) change only with a
   migration and a test that opens the old format.
6. The public daemon API is versioned (`/v1`). Removing or renaming a field is
   a breaking change and needs maintainer approval.

---

## 5. Precautions for AI-generated code

AI agents tend to make the same mistakes. Check for each of these before you
open a PR:

- **Invented APIs.** Confirm that every crate function, Tauri API, browser API
  and npm export you call exists **at the version in the lockfile**.
- **Edits to the wrong copy.** There are two extension manifests, generated
  bindings and vendored crates. Edit the source, not a copy.
- **Half-wired features.**
  - A new engine type needs its bindings regenerated.
  - A new daemon route needs utoipa annotations, a regenerated OpenAPI file and
    a regenerated TS schema.
  - A new permission needs a CLI grant path and docs.
- **Platform blindness.** Code that only works on your OS. Check path
  separators, `.exe` suffixes, the PowerShell vs bash quoting rules (see the
  comments in `build-windows.ps1`), and file locking on Windows.
- **Escapes broken in transit.** Scripts that write code through heredocs or
  Python can turn `\n` and `\x1b` into real bytes. Review the diff.
- **Overreach.** Don't "clean up" files you didn't need to touch, add speculative
  abstractions, or write comments that narrate the change ("now uses X instead
  of Y").
- **Claims you didn't check.** Don't say "tested" unless you ran it. Write
  "compiles; not run" when that is the truth.
- **Leaking context.** Don't paste the contents of your local `.env`, tokens or
  personal paths into code, comments or PR text.

Agents should run the checks in §7 themselves and paste the real output
summary into the PR, not an expected one.

---

## 6. What kind of PR is accepted

### Likely accepted

- A bug fix with a regression test that fails without the fix.
- An item from the "Wanted" list (§3) that was discussed in an issue first.
- Platform fixes (Linux distros, macOS, Windows ARM) with a note of what was
  tested.
- Corrections to docs, translations, and accessibility improvements to the
  UI (labels, keyboard navigation, contrast).
- Performance work with before/after numbers from `dev-tools/test-suite/benchmarks`.
- Small, focused refactors that make an upcoming change easier, and say so.

### Likely closed

- PRs over ~800 changed lines (excluding generated files) without a prior issue.
- "Improve code quality" sweeps, mass renames, reformatting, or a different
  style imposed on code you aren't otherwise changing.
- New features that duplicate something in "Already done" (§3).
- Changes that weaken an invariant or security default in §4.
- Changes that add network calls, ads, telemetry or bundled third-party keys.
- PRs whose description doesn't match the diff, or that claim tests that
  weren't run.
- A new dependency where a few lines of code would do.
- Anything that downloads DRM-protected content or bypasses access controls.

### PR checklist

Use `.github/PULL_REQUEST_TEMPLATE.md`. In addition:

- [ ] One concern per PR; title in the imperative ("Fix resume after redirect").
- [ ] All checks in §7 pass locally (list the ones you ran).
- [ ] Tests added or updated.
- [ ] Generated files regenerated, not hand-edited.
- [ ] Docs and `CHANGELOG.md` `[Unreleased]` updated.
- [ ] OS/arch tested; untested areas listed.
- [ ] AI assistance disclosed (which tool). This is welcome, not penalized;
      it tells reviewers what to look at more closely.

---

## 7. Local workflow

### First setup

```bash
git clone https://github.com/dippendicular/okytes && cd okytes
node dev-tools/scripts/prepare-vendor.mjs
pnpm --dir apps/desktop install --frozen-lockfile
pnpm --dir apps/desktop build:quick      # needed before the daemon compiles
cargo build --workspace --locked
```

On Linux, run `bash dev-tools/scripts/build-linux.sh --help` for the system
packages it needs. On Windows, install the MSVC build tools and WebView2. On
macOS, run `xcode-select --install`.

### Run things

```bash
pnpm --dir apps/desktop tauri dev                 # desktop app
cargo run -p okytes-daemon -- --help              # headless daemon
cargo run -p okytes-cli -- watch                  # live terminal view
cd extensions && pnpm install && pnpm build       # then load dist/chromium unpacked
```

### Checks: the same as CI

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny check

cd apps/desktop && pnpm format:check && pnpm typecheck && pnpm lint && pnpm test && pnpm build
cd extensions   && pnpm typecheck && pnpm lint && pnpm test && pnpm build
```

### Regenerate generated files

```bash
# Engine → UI bindings
cargo test -p okytes-core --lib --locked desktop::export_bindings
cargo test -p okytes-core --lib --locked traffic::export_bindings
cargo test -p okytes-torrent-types --lib --locked
node apps/desktop/scripts/normalize-bindings.mjs

# Browser protocol
cargo test -p okytes-browser-bridge --lib --locked protocol::export_bindings

# Daemon OpenAPI + TS SDK schema
cargo run -p okytes-daemon -- --export-openapi apps/daemon/api/openapi-v1.json
pnpm --dir dev-tools/sdk/packages/sdk-typescript generate
```

CI fails if `git diff` shows that a generated file differs from its generator.

### Manual test plan

`dev-tools/test-suite/manual-0.0.2.md` lists the hands-on checks for each
feature. If your change affects one of them, run it and say so in the PR.

---

## 8. DevOps

### CI (`.github/workflows/ci.yml`)

CI runs on push, on pull requests and on a schedule.

**Linux, Windows and macOS:** `cargo test --workspace`.

**Linux only:**
- Packaging regression tests.
- Generated-binding drift checks.
- Frontend and extension checks.
- `cargo fmt`.
- Daemon contract and SDK checks.
- `clippy -D warnings`.
- `cargo deny`.

**Separate jobs:** a stress job, workflow linting, and a Quick Download
browser job.

Every required check must be green before merge. Don't "fix" CI by deleting or
skipping a check. Fix the cause, or explain in the PR why the check is wrong.

### Releases (`.github/workflows/release.yml`)

- **Trigger:** push a tag `vX.Y.Z` that matches the workspace version. Only
  maintainers do this. `workflow_dispatch` re-validates an existing tag without
  publishing.
- **Version bump:** `[workspace.package] version` in `Cargo.toml`, every app's
  `package.json` and `tauri.conf.json`, the SDK packages, the OpenAPI `info.version`,
  `CHANGELOG.md` and a new `RELEASE-NOTES-X.Y.Z.md`. Search for the old version
  string; nothing should still show it.
- **Signing secrets** (repo settings, never in files):
  - Updater: `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
  - Windows: `WINDOWS_CERTIFICATE`, `WINDOWS_CERTIFICATE_PASSWORD`.
  - macOS: `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`,
    `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`.
  - npm: `NPM_TOKEN`.

  Without the signing secrets, the workflow builds unsigned packages and says so.
- **Artifacts:** each one ships with SHA-256 checksums, an SBOM
  (`packaging/sbom.mjs`) and third-party notices (`packaging/notices.mjs`).

### Local release builds

| OS | Command | Output |
|---|---|---|
| Windows | `powershell -ExecutionPolicy Bypass -File dev-tools\scripts\build-windows.ps1 [-SkipInstall] [-Arm64]` | `target\release\windows_release` |
| Linux | `bash dev-tools/scripts/build-linux.sh [--skip-install]` | `target/release/linux_release` |
| macOS | `bash dev-tools/scripts/build-macos.sh [--skip-install] [--universal]` | `target/release/mac_release` |
| Extensions | `node dev-tools/scripts/build-extensions.mjs [--skip-install] [--skip-checks]` | `target/release/extension_release` |

Local builds are unsigned: `UPDATE_SIGNING_ENABLED=false`. Under WSL, build
from the Linux filesystem (`~/okytes`), not `/mnt/c` or `/mnt/d`; it is many
times faster. `.gitattributes` keeps `*.sh` files at LF line endings. Don't
commit CRLF shell scripts.

### Docker / server deployment

- `docker compose up -d` builds the daemon + CLI image and runs it as a
  non-root user.
- Use either TLS mode (with your own certificate) or proxy mode (behind
  Caddy or nginx).
- The daemon refuses plaintext on non-loopback addresses on purpose. Put TLS
  in front of it; don't remove the check.
- Mount the data directory as a volume. Checkpoints live there.
- Create a token for each client, limited to the permissions it needs:
  `jobs:read`, `jobs:add`, `jobs:control`, `events:read`, `media:inspect`,
  `settings:read`.
- Full guide: `apps/daemon/SERVER.md`.

### Dependencies

Dependabot opens grouped PRs for cargo, npm (desktop, extensions, SDK),
GitHub Actions and Docker.

- **Review a dependency PR like code:** read the changelog, run the checks,
  and watch for license changes (`cargo deny`).
- **Vendored torrent crates** are excluded from Dependabot. To update them,
  bump them in `prepare-vendor.mjs`, refresh the patches in `vendor-patches/`,
  and update the checksums.

### Security reports

Never open a public issue for a vulnerability. Follow
[SECURITY.md](../SECURITY.md).

---

## 9. Quick prompt for agents

Paste this at the start of an agent session:

> You are contributing to Okytes. First read `dev-tools/AI-CONTRIBUTOR-GUIDE.md`
> and `CONTRIBUTING.md`. Make one focused change. Match the surrounding style.
> Never hand-edit generated files, `vendor/`, lockfiles, version numbers,
> license files or release workflows. Keep every security default and the
> engine invariants in §4. Add or update tests and docs. Run the checks in §7
> and report their real results, including anything you could not verify.
