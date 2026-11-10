# 0.0.2 manual test checklist

Everything below compiles, type-checks and lints, but most of it has not been
run in the real app. Tick items as you go.

## 0. Before testing (regenerate generated files)

Several generated files were edited by hand. Regenerate them and review the diff:

```sh
cargo test -p okytes-core --lib desktop::export_bindings
cargo test -p okytes-core --lib traffic::export_bindings
node apps/desktop/scripts/normalize-bindings.mjs
git diff crates/core/bindings
```

The daemon's committed contract `apps/daemon/api/openapi-v1.json` is now out of
date (new routes and token fields). Regenerate it with the daemon's contract
test, then run `pnpm --dir dev-tools/sdk/packages/sdk-typescript generate`.

Known pre-existing issue: `cargo clippy -p okytes-daemon -- -D warnings`
reports two `result_large_err` errors in code that 0.0.2 did not change.

## 1. Download window

- [ ] Details opens a separate window; clicking again focuses it.
- [ ] File map colours change while downloading a large multi-connection file; stalled shows after 5 s without data.
- [ ] Connections table rows appear and disappear as requests start and finish.
- [ ] Unknown-size download shows the striped "size unknown" bar.
- [ ] Hide details collapses and restores the lower part.
- [ ] Pause / Resume / Cancel work from the window.
- [ ] Speed limiter: custom limit visibly slows only this download; "Use default" removes it.
- [ ] Completion: open file, show in folder and notify each run once; a failed download keeps its options.
- [ ] Settings → open window on start: a new download opens its window; launching the app opens none.
- [ ] Daemon mode: telemetry appears through the daemon.

## 2. Expired links and invalid responses

- [ ] A `.zip` link answered with an HTML page fails with "server sent a web page".
- [ ] Failed download → paste a link to the same file → Refresh link and resume continues from the saved progress.
- [ ] A link to a different file is refused with an identity error.

## 3. Crawler, playlists, ZIP preview

- [ ] Crawl a site with depth 1; robots.txt-disallowed pages are skipped; Add selected creates jobs.
- [ ] Expand an M3U/PLS/XSPF playlist; an HLS `.m3u8` stream is refused with a hint.
- [ ] Preview a remote ZIP (small and large); a server without range support gives a clear error.

## 4. Media merge (needs ffmpeg)

- [ ] Add media → select video + audio → Merge: an `… (merged).mkv` appears after both finish; tracks are kept.
- [ ] Without ffmpeg the option is disabled with an explanation.
- [ ] Merge plan survives an app restart between the two tracks finishing.

## 5. Cloud storage

- [ ] Google Drive, Dropbox, OneDrive: set up app, Sign in…, Connected.
- [ ] Upload on completion lands in the `Okytes` folder; footer shows progress; notification on success.
- [ ] Disconnect network briefly during a large upload: it resumes or fails with a notification.

## 6. Okytes Cloud

- [ ] Owner sign-up, second sign-up refused (no allowlist); allowlisted sign-up works.
- [ ] GitHub and Google sign-in (needs OAuth apps).
- [ ] SMTP: confirmation email required before sign-in; forgot-password email resets.
- [ ] `okytes-cloud reset-link` works without SMTP.
- [ ] Pair desktop with a code; phone dashboard (behind HTTPS) sends a link that starts on the PC.
- [ ] History from a second paired PC appears under "Downloads on my other devices".

## 7. Plugins and MCP

- [ ] `okytes-cli plugin-grant plugins/webhook-notify` lists permissions and writes `.okytes-token`.
- [ ] webhook-notify posts on completion and failure (try ntfy).
- [ ] A plugin token calling an undeclared route gets `403 missing_permission`.
- [ ] `plugin-list` and `plugin-revoke` work; a revoked plugin stops working.
- [ ] `okytes-mcp` in an MCP client lists, adds, pauses and resumes downloads.

## 8. Browser extension

- [ ] Popup switch enables automatic capture; clicking a file link hands it to Okytes and cancels the browser copy.
- [ ] Okytes closed: browser keeps the download and the badge shows "!" with a reason.
- [ ] Right-click "Download with Okytes" on a link; on failure the browser downloads it.
- [ ] Chrome with a type rule (e.g. `zip`) captures a link without an extension that resolves to a `.zip`.
- [ ] Firefox: menus appear after a browser restart.

## 9. Upgrade

- [ ] Install 0.0.2 over 0.0.1: history, settings and the browser host registration survive.

## 10. Scheduler evidence

- [ ] Configure a recurring queue window in Settings using the intended IANA
  timezone. A queued job starts when the window opens; an active job yields
  when it closes; a manually paused job stays paused when it reopens.
- [ ] Run the scheduler harness from an idle machine with a new output
  directory: `cargo run -p okytes-bench --release --locked -- run --runs 3 --mode both --output .tools/scheduler-benchmark`.
  Review `summary.json` and every raw result before making a performance claim.
- [ ] Confirm the `flaky-resets` scenario reports successful restart-resume
  and a verified final SHA-256. CI archives the same fixed-versus-adaptive
  evidence weekly and on manual dispatch.
