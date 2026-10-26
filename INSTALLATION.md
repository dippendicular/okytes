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
`OKYTES_DOWNLOAD_DIR` are set to absolute paths. It registers no service. On
launch, the desktop registers its bundled browser host for the released
extension's fixed ID in the current user's browser configuration.

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

Download `Okytes-Chrome-extension.zip` from the same GitHub Release, extract it
to a permanent folder, and load it using **Developer mode → Load unpacked** at
`chrome://extensions`. No extension build tools are required. Follow the release's
`BROWSER-INSTALLATION.md` for browser setup and updates. This ZIP
distribution requires Developer mode; it is not a Chrome Web Store installation.

The desktop refreshes per-user native-messaging registration on every launch;
it does not install the extension itself. See `extensions/README.md` and
`apps/native-host` for the manual development command and browser identifiers.
The daemon runs only when you launch or register it yourself; its service
examples live in `apps/daemon`.

## Quick Download in a local browser

The installed `okytes-daemon` companion serves Quick Download without an extension.
All page assets are inside that executable; it needs no hosted site or CDN. Start
it explicitly with an absolute profile and download folder, for example on Windows:

```powershell
.\okytes-daemon.exe --quick --bind 127.0.0.1:65432 --data-dir D:\OkytesData --downloads D:\Downloads
.\okytes-cli.exe --daemon-url http://127.0.0.1:65432/ --token-file D:\OkytesData\daemon-admin.token quick --open
```

On Linux/macOS use `./okytes-daemon` and `./okytes-cli` with absolute Unix paths.
The CLI prints a one-use code: enter it in the opened local page within two
minutes. Never paste the permanent token into a webpage. Sessions last at most
15 minutes; logout revokes them immediately. Each new or reloaded tab needs a new
code because its independent authorization proof stays only in memory. The queue
and downloaded bytes remain with the daemon. Bookmark the credential-free page.

`--bind` controls the endpoint; existing daemon clients retain the default
`127.0.0.1:17841`. Use `:65432` explicitly for the example above. An occupied port
fails with instructions instead of choosing another interface or port. Quick mode
rejects remote mode and port zero. A phone's loopback address points to the phone.

Close an embedded desktop owner before starting a daemon on its profile. Attach
the desktop and native host using `OKYTES_DAEMON_URL` and
`OKYTES_DAEMON_TOKEN_FILE` as described in [the daemon guide](apps/daemon/README.md).
The desktop/tray **Open Quick Download** action displays an approval code when
attached to a Quick-enabled local daemon with a native admin credential. There is
no automatic owner takeover or anonymous recovery route.

Portable ZIPs and `.run` installations place companions in `bin`. Native bundles
stage the companions through Tauri's external-binary packaging. On macOS inspect
`Okytes.app/Contents/MacOS`; native package paths must be verified for the actual
artifact. For AppImage use the matching portable tools archive or extract the
AppImage to access its daemon; opening the desktop AppImage is not a daemon-start
command. Do not assume an artifact contains these helpers until its release
record confirms that build. Native bundle installation/upgrade qualification
remains separate from source and browser tests.

Background availability is opt-in: `--quick` enables the page on a foreground
daemon and does not install a startup task. Closing a tab or attached desktop
does not stop that separate daemon. Ctrl+C or stopping its supervisor ends
availability; an already open page reports disconnection, while a new navigation
may show the browser's connection error. The page cannot start a stopped process.
Logout normally ends a foreground/per-user process unless an explicitly configured
supervisor has different behavior.

For startup, edit the existing [service templates](apps/daemon/services/README.md)
to include `--quick --bind 127.0.0.1:65432`. Linux uses an explicitly enabled
systemd user unit; disable/stop it before removal. macOS uses a user LaunchAgent;
boot it out before removing its plist. Windows requires the documented external
service wrapper/account configuration, or a user-created logon task using that
user's profile; the console daemon is not directly an SCM service. None is silently
registered by the installer. Stop the owner before upgrading its executable, retain
its profile for recovery, and pair browser tabs again after restart. Do not grant
root/LocalSystem privileges merely to keep a personal queue available.

Only manually reusable HTTP/HTTPS links can be added here, up to ten per batch.
Browser cookies, site headers and expired signed links may need the extension or
a fresh direct link. Quick does not intercept third-party clicks, expose your LAN,
or open/execute downloaded files. Use the desktop for native file actions.
See the [local web threat model](docs/security/local-web-threat-model.md).

## Removing Okytes

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
