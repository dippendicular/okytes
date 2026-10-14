# Contributing to Okytes

Start with an issue describing the problem, expected behavior and a reproducible
example. Small bug fixes can go directly to a pull request. Discuss protocol,
persistence and public API changes before committing to a large implementation.

## Build from source

Install the pinned Rust toolchain, Node.js 24, pnpm 11.11, Git and tar. On Linux,
install WebKitGTK 4.1 development files, GTK 3, Ayatana AppIndicator, librsvg and
patchelf. Windows needs the MSVC build tools and WebView2; macOS needs Xcode
command-line tools. Run `node scripts/prepare-vendor.mjs` first: it restores
checksum-pinned, reviewed torrent dependencies into the ignored vendor folder.

Run `cargo build --workspace --locked`. For the desktop, run
`pnpm --dir apps/desktop install --frozen-lockfile` and
`pnpm --dir apps/desktop tauri dev`. Use an isolated `OKYTES_DATA_DIR` when
testing changes to persisted state. Never put credentials, private signing keys
or personal download URLs in fixtures, logs or pull requests.

## Before submitting

Run `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets --locked -- -D warnings`,
`cargo test --workspace --locked`, and `cargo deny check`. In `apps/desktop`,
run format checking, typechecking, lint, tests and build with pnpm. Run the
extension and SDK checks when their contracts change. Include a regression test
for a bug fix and explain the operating systems on which you tested it.

Keep durable checkpoints behind completed file synchronization. Resume must
reject changed remote identities. Keep buffers, queues and connection counts
bounded. Downloads are never executed automatically. New dependencies need a
compatible license and a reason to exist.

Packaging and release procedures are in `packaging/RELEASING.md`; installation
and removal are in `packaging/INSTALLATION.md`. Building successfully does not
establish that an installer or another platform works. State unverified cases
plainly in your pull request.

Contributions are licensed under Apache-2.0 OR MIT, matching the project.

## Architecture changes

Describe the problem, alternatives, compatibility impact and validation plan in
the pull request. Maintainers record accepted architecture decisions with the
implementation. Breaking API and profile changes need a migration plan.
