# Release procedure

Releases are candidates until their recorded verification and signing gates are
reviewed. The workflow creates a **draft**, never a publicly approved release.

## Version and builds

Update the Cargo workspace version, Tauri config, desktop package version and
exact matching `CHANGELOG.md` entry together. Refresh the lockfiles and internal
package requirements where needed. Version tags use `vMAJOR.MINOR.PATCH`.
Run **Release candidate** with an existing tag for a dry run; this uploads
artifacts without publishing a GitHub Release. Pushing that tag triggers the
draft-release path. The repository needs the `release` GitHub environment with
required reviewers and tag restrictions. No release has been approved merely
because this automation exists.

The matrix uses native Windows x64 / ARM64, Ubuntu 22.04 x86_64 / ARM64, and
macOS Apple Silicon / Intel runners. macOS artifacts are per-architecture to
avoid pretending that every native dependency works as a universal build.
Linux's glibc 2.35 floor is checked from the desktop ELF symbol requirements.
Dependencies remain locked. Runner images and platform SDKs change over time;
these builds are reproducible as practical, not promised byte-identical.

Configurations are in the platform subdirectories. Tauri produces NSIS/MSI,
DEB/RPM/AppImage and APP/DMG. `stage.mjs` packages the desktop, CLI, daemon and
native host together in a portable archive. The Windows portable executable is
`bin/okytes-desktop.exe` inside that archive; it needs WebView2. Linux's `.run`
uses that tar payload and validates SHA-256 before extraction. It refuses an
existing destination and executes no application during installation.

## Signing keys and CI secrets

Generate the updater key on an offline machine with
`pnpm exec tauri signer generate -w /private/location/okytes.key`. Keep an
encrypted offline backup. Never commit the private key. Put its public `.pub`
content in the GitHub environment variable `OKYTES_UPDATE_PUBLIC_KEY`; CI embeds
it into the native binary. For an offline-only signing key, leave CI signing
secrets unset. Download the candidate artifacts, review and test them, then sign
the updater payloads with `tauri signer sign --app-version VERSION` on the
offline machine. Run `packaging/sign-release.mjs` there after recording genuine
installer qualification. Bring only the signed metadata, checksum manifest and
public signatures back online and attach them to the draft; never transfer the
private key. Builds can embed the public key without receiving the private key.

For maintainers who explicitly choose automated CI signing, the protected release environment accepts
`TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` as secrets.
Only bundling/signing steps receive them; compilation, tests and ordinary PR
checks must not. Supplying a
private key without a public key is a hard error. With neither, bundles and
checksums are named unsigned and no update feed is generated.

The same key signs the exact update descriptor, binding version, release notes,
platform URLs and payload signatures. `latest.json` wraps those signed bytes.
Only artifacts with a recorded `installation: verified` qualification enter
that feed. The default collector records unverified; no feed is emitted when
none qualify, even if checksum signing is available. Qualification records must
be backed by clean-environment lifecycle evidence, not edited to bypass a gate.
The app checks only after a user action, downloads with a 512 MiB cap, verifies
the payload, and requires another action to start installation. Metadata is
limited to 1 MiB. Stable updates must be newer than the running version and no
older than the durable previously verified release floor. No device identifiers,
cookies, account tokens or download history are attached. GitHub necessarily
sees the connection's IP and ordinary HTTP request details. Profile deletion
resets the extra replay floor; the running app's version still prevents downgrade.

For Windows Authenticode, configure base64 PFX `WINDOWS_CERTIFICATE` and
`WINDOWS_CERTIFICATE_PASSWORD`. The workflow imports it into the ephemeral user's
certificate store and supplies its thumbprint, SHA-256 digest and timestamp
server to Tauri. Hardware-backed signing services need a separately configured
signing command instead of exporting their key. Sign and verify every helper
binary before qualifying the portable archive as signed; the default collector
does not claim that this additional qualification has happened.

For macOS, configure `APPLE_CERTIFICATE` (base64 Developer ID P12),
`APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`,
`APPLE_PASSWORD` (app-specific password), and `APPLE_TEAM_ID`. Tauri invokes
Developer ID signing and notarization. Do not use `--skip-stapling` for a
qualified release. Verify `codesign --verify --deep --strict`,
`spctl --assess --type execute` and `xcrun stapler validate` on the resulting app.
No credentials here means those paths remain unverified. Code signing,
notarization, signed checksums and updater signatures are different checks.

`CHECKSUMS.txt.sig` authenticates `CHECKSUMS.txt` when a key was available.
An absent key produces `CHECKSUMS-unsigned.txt`. SHA-256 covers the collected
artifacts, SBOM, verification records, installation instructions, release notes
and update feed. Preserve the original vendored library licenses and notices;
the Cargo CycloneDX inventory identifies patched path dependencies explicitly.
The frontend license inventory is separate and must be reviewed alongside it.

## Qualification before approving a draft

Use fresh VMs or containers for each artifact. Install, launch, upgrade from the
previous release, and uninstall. Register and remove browser-native hosts and
service files separately. Exercise crash recovery with the installed CLI:

```sh
OKYTES_INSTALLED_CLI=/absolute/installed/bin/okytes-cli \
  cargo test -p okytes-cli --test process --locked \
  forced_process_kill_resumes_identically_and_changed_etag_is_rejected
```

On Windows use the installed `.exe` path. This validates recovery and changed
validators against the real executable; it does not establish GUI or installer
correctness. The workflow exercises the staged executable and conservatively
labels installation unverified. Record exact OS / architecture, installer type,
signing verification, WebView version and test logs before changing a label.
Unsigned or unverified artifacts must remain clearly identified in draft notes;
do not approve them as production-qualified downloads by inference.

Review the checksum signature, SBOM, dependency audit, release notes and every
unverified label before publishing the draft. No tagged dry run, clean-VM
lifecycle result, platform signing or Safari acceptance is implied by local
source tests.
