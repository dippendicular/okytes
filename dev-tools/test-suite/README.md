# Okytes test suite

This directory contains repository-wide test infrastructure. Tests owned by an application or Rust crate remain beside that component under its normal `tests/` directory.

## Layout

- `test-support/` is the reusable Rust fault server and desktop fixture binary.
- `packages/sdk-typescript/tests/` checks the published Node SDK and its live daemon contract.
- `packaging/tests/` checks release staging, checksums, notices and portable artifacts.
- `packaging/windows/test-install.ps1` performs a real silent NSIS install and uninstall in a clean Windows environment.

## Prerequisites

Install the pinned Rust toolchain, Node.js 24, pnpm 11.11, and Python 3.10 or newer. Run `node dev-tools/scripts/prepare-vendor.mjs` after a fresh clone. Platform packaging tests additionally need the tools listed in `INSTALLATION.md` and the release workflow.

## Rust suites

Run all crate and application tests from the repository root:

```powershell
cargo test --workspace
```

Run one owner-specific suite while developing:

```powershell
cargo test -p okytes-daemon --test api
cargo test -p okytes-transport-http --test protocol
cargo test -p okytes-persistence --test recovery
```

The shared fixture can be built independently:

```powershell
cargo build -p okytes-test-support --example desktop_fixture
```

## TypeScript SDK

The package command builds against tests stored here:

```powershell
pnpm --dir packages/sdk-typescript install --frozen-lockfile
pnpm --dir packages/sdk-typescript generate
pnpm --dir packages/sdk-typescript typecheck
pnpm --dir packages/sdk-typescript build
pnpm --dir packages/sdk-typescript test
```

The live SDK scenario is launched by `apps/daemon/tests/sdk.rs`; run it with `cargo test -p okytes-daemon --test sdk` after building the TypeScript package.

## Python SDK

Build distributable artifacts and inspect them without publishing:

```powershell
python -m pip install --upgrade build twine
python -m build packages/sdk-python
python -m twine check packages/sdk-python/dist/*
```

## Desktop, extension and packaging

Desktop and extension tests stay with their owning packages:

```powershell
pnpm --dir apps/desktop test
pnpm --dir extensions test
node --test dev-tools/test-suite/packaging/tests/release.test.mjs
```

The Windows installer lifecycle changes the current user's install registration and therefore belongs in a clean VM:

```powershell
./dev-tools/test-suite/packaging/windows/test-install.ps1 -Installer ./path/to/Okytes-setup.exe -Destination "$PWD/.tools/nsis-installed"
```

## Full acceptance sequence

CI uses the same owner locations. Before a release, run formatting, clippy, workspace tests, dependency policy, frontend checks, SDK builds, package inspection and packaging tests. Never publish from a dirty tree or by bypassing a failing contract-generation step.

