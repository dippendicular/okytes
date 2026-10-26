# Install the Okytes Chrome extension

Download `Okytes-Chrome-extension.zip` and the Okytes desktop package for your
operating system from the same GitHub Release. This ZIP contains built extension
files; Node.js, pnpm and Rust are not needed.

1. Extract the ZIP into a permanent folder. Keep it there while the extension is installed.
2. Open `chrome://extensions` in Chrome and enable **Developer mode**.
3. Click **Load unpacked** and select the extracted folder containing `manifest.json`.
4. Copy the extension's ID shown on its card.
5. Register the native host shipped with the desktop package, using that ID.

For the Windows portable package, open PowerShell in its extracted folder:

```powershell
.\bin\okytes-native-host.exe install --chromium-id YOUR_EXTENSION_ID
```

For Linux portable packages or the macOS tools archive, open a terminal in the
extracted folder:

```sh
./bin/okytes-native-host install --chromium-id YOUR_EXTENSION_ID
```

For a native desktop installer, locate its installed `okytes-native-host`
companion and run the same `install --chromium-id YOUR_EXTENSION_ID` command.
Keep that executable in place; moving it requires registering it again.
Restart Okytes after registration and keep the desktop app running when sending
downloads. The browser and desktop must use the same user profile.

The ZIP is an unpacked distribution, not a Chrome Web Store installation.
It requires Developer mode and does not automatically update through Chrome.
To update, extract the new release over the existing extension folder and click
**Reload** on its extension card. Check the ID and repeat registration if it changed.
Managed browsers may prohibit unpacked extensions.
