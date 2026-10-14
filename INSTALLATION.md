# Installing Okytes

Download the artifact for your operating system and architecture. Verify its
SHA-256 against the release checksum manifest before opening it. A checksum
detects damage; authenticate the signed manifest against the project's public
release key to establish who produced it. Unsigned artifacts do not provide
publisher authentication. Do not bypass an OS security warning without reviewing
the release and its signing status.

## Windows

The NSIS `.exe` installs for the current user; the `.msi` installs per-machine
through Windows Installer and requires administrator rights.
Both require Microsoft Edge WebView2 Runtime, which the bundler can download
after showing its installer. For offline deployment, install WebView2 separately.
Windows 10/11 x64 and Windows 11 ARM64 are distinct targets. Use the correct build.

The portable archive contains the desktop executable, CLI, daemon and native
host in `bin/`. Extract the entire directory and launch `bin/okytes-desktop.exe`.
It still requires WebView2. â€œPortableâ€ describes deployment, not private data:
the desktop uses its normal per-user profile unless `OKYTES_DATA_DIR` and
`OKYTES_DOWNLOAD_DIR` are set to absolute paths. It registers no service or
browser host automatically.

## Linux

Release builds use Ubuntu 22.04 on x86_64 and ARM64, with a declared glibc 2.35
floor. This floor must also be checked against the built ELF files before
publishing; a newer compiler dependency can raise it. Linux packages need GTK 3,
WebKitGTK 4.1, Ayatana AppIndicator for the tray, fonts, and a working graphical
session with X11 or Wayland and a compatible graphics driver. `.run` and
AppImage do not remove these architecture, glibc or graphics constraints.
The `.run` wrapper also needs a POSIX shell, GNU tar/coreutils, awk and grep.
AppImage may also require FUSE 2; `--appimage-extract-and-run` is an alternative
where supported. A headless server should use the CLI or daemon instead.

Install `.deb` with `sudo apt install ./Okytes_*.deb`, or `.rpm` with
`sudo dnf install ./Okytes-*.rpm`; the package manager resolves dependencies.
Make AppImage executable with `chmod +x Okytes*.AppImage` and open it.

For `.run`, first execute `sh Okytes.run --check`; then
`sh Okytes.run`. Its payload checksum is checked again before extraction.
The default destination is `$HOME/.local/opt/okytes`. Use
`--prefix /absolute/new/directory` to choose another location. Existing
directories are refused. Install an upgrade in a new directory, close the old
app, test the new one, then remove the old installation. This installer does not
request root, alter shell startup files, register a daemon, or execute the app.

## macOS

Choose the Apple Silicon (`aarch64`) or Intel (`x86_64`) DMG, requiring macOS 13
or newer. Open the DMG and drag Okytes into Applications. Native WebKit is used.
The APP archive contains the same application bundle; command-line companions
are distributed separately in the tools archive.
Developer ID signing and Apple notarization are separate from update signatures;
release notes state which checks actually ran. Unsigned builds are for testing
and may be blocked by Gatekeeper. No universal binary is promised.

## Browser integration and background service

Native messaging is an explicit registration step. See `extensions/README.md`
and `apps/native-host` for the platform-specific registration command and
browser identifiers. Installing the desktop does not install an extension.
The daemon runs only when you launch or register it yourself; its service
examples live in `apps/daemon`.

## Uninstall

Close Okytes and stop any separately registered daemon first. Use Windows
Settings / installed apps for NSIS or MSI, the package manager for DEB/RPM,
delete the application from Applications for macOS, or remove the extracted
portable/AppImage/`.run` directory. Keep downloads unless you deliberately want
to delete them.

Remove any native-messaging registration you created. On Windows delete only
the `org.okytes.bridge` host key under `HKCU\Software\Google\Chrome\NativeMessagingHosts`,
`HKCU\Software\Microsoft\Edge\NativeMessagingHosts`, and
`HKCU\Software\Mozilla\NativeMessagingHosts`, after checking the registered
manifest belongs to this installation. On Linux remove the corresponding
`org.okytes.bridge.json` from `~/.config/google-chrome/NativeMessagingHosts`,
`~/.config/chromium/NativeMessagingHosts` and `~/.mozilla/native-messaging-hosts`.
On macOS those manifest directories are under `~/Library/Application Support/`
in `Google/Chrome`, `Microsoft Edge` and `Mozilla`. Check the exact host name in
the installed manifest before deleting anything. Remove manually created systemd user
units, launchd plist files, or Windows service-supervisor registrations; reload
the service manager afterwards. Never remove unrelated services or host files.
Profile removal is optional: inspect the configured `OKYTES_DATA_DIR` or Tauri
application-data directory (`org.okytes.desktop`) before deleting it. Profiles
contain queue/history, checkpoints and browser bridge authorization; partial
downloads can no longer resume after their checkpoints are deleted.
