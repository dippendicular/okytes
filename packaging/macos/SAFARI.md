# Safari extension packaging

Safari packages a WebExtension inside a signed containing macOS application and
an app-extension target. A Chromium native-messaging manifest and an external
stdio executable are not a Safari integration. Okytes's Chromium and Firefox
integration must not be advertised as Safari-tested.

On macOS with Xcode, build `extensions/dist/chromium`, then run:

```sh
xcrun safari-web-extension-converter extensions/dist/chromium \
  --project-location packaging/macos/generated-safari \
  --app-name 'Okytes Browser Integration' --bundle-identifier org.okytes.safari
```

Newer Xcode versions may expose the renamed
`safari-web-extension-packager`; inspect `xcrun ... --help` for that installed
version instead of assuming flags are interchangeable. Review the converter's
unsupported-permission and manifest warnings. Open the generated Xcode project,
choose a development team, and review both the containing-app and extension
targets before building. Generated output is intentionally ignored; source
review and a verified macOS build are required before checking it in.

The containing app needs a real Safari native-message handler and authenticated
handoff to Okytes. The current Rust stdio host does not provide that handler.
Audit `com.apple.security.app-sandbox`, any explicitly required app-group
entitlement shared by those two targets, network entitlements and provisioning
profiles. Do not add arbitrary filesystem access or transport-security bypasses
to silence a build warning. Enable the extension in Safari settings only after
the containing app is installed.

Safari does not provide Chromium's `downloads` API or the same blocking download
interception behavior ([browser compatibility data](https://github.com/mdn/browser-compat-data/blob/main/webextensions/api/downloads.json)). Automatic download cancellation/ownership transfer must
be omitted until a supported Safari path exists and is tested. Browser rules,
native messaging and credential handoff also differ. Start with explicit
user-selected capture; never claim IDM-style interception based on conversion
alone. Review every shared API call and permission for Safari compatibility.

App Store distribution requires Apple Developer enrollment, App Store signing,
review, sandbox-compatible behavior and a containing app. Direct macOS
distribution has different Developer ID, notarization, Gatekeeper and extension
enablement requirements; check the installed Xcode/SDK and Apple's current
distribution rules before promising a path. Neither route is verified here.
Mobile Safari, mobile packaging and pairing remain outside M8.

References: [Apple Safari extensions](https://developer.apple.com/safari/extensions/),
[Safari WebExtensions](https://developer.apple.com/documentation/safariservices/safari-web-extensions),
[Tauri updater](https://v2.tauri.app/plugin/updater/).
