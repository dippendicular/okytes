# Okytes desktop

The desktop app runs the HTTP engine in-process and stores its download history
and settings in SQLite. Closing or force-quitting the app leaves durable checkpoints
for recovery on the next launch. The CLI and desktop may control the same database,
but only one process can own transfer writes.

Use **Add downloads** for one link or a batch of up to 100 links. Paste from the
clipboard, import a text file, or drop links onto the window. Search by filename,
URL or destination, and filter by file category or download state. Pause a job
before renaming it. **Download again** creates a fresh entry and preserves existing
files; **Remove from history** leaves completed files on disk.

Details show durable segment checkpoints, validators, retry counts, the last
engine error, and session HTTP response diagnostics. The speed graph measures
changes in durable downloaded bytes. Settings control the default folder,
simultaneous downloads, appearance and native notifications. The tray offers
show/hide, pause-all and quit.

## Running from source

Install the pinned Rust toolchain, Node.js 24, pnpm 11 and the platform prerequisites
listed in the [Tauri documentation](https://v2.tauri.app/start/prerequisites/).
On Windows, this includes the MSVC build tools and WebView2 runtime.

From `apps/desktop`:

```sh
pnpm install --frozen-lockfile
pnpm tauri dev
```

Build a local executable without an installer:

```sh
pnpm tauri build --no-bundle
```

Use `--debug` for a development executable. From the repository root, run the
Rust workspace checks and `cargo test -p okytes-core --lib` to regenerate the
TypeScript bindings. Frontend checks are `pnpm format:check`, `pnpm typecheck`, `pnpm lint`, and
`pnpm test`; `pnpm build` produces local web assets.

The app normally stores its database under the platform application-data folder.
`OKYTES_DATA_DIR` and `OKYTES_DOWNLOAD_DIR` accept absolute paths for an isolated
profile. They are process environment settings, not webview filesystem access.

## Windows native integration test

Build the debug app and the deterministic fault-server fixture:

```sh
pnpm tauri build --debug --no-bundle
cargo build -p okytes-test-support --example desktop_fixture
node scripts/native-e2e.mjs
```

The test attaches Playwright to the actual WebView2 instance, uses an isolated
profile, and writes evidence under the ignored `.tools` directory. It enables a
loopback debugging port only in that test process. It does not expose a management
API in normal application runs. Other platforms require their own native tests.
