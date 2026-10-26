# Install the Okytes Chrome extension

Download `Okytes-Chrome-extension.zip` and the Okytes desktop package for your
operating system from the same GitHub Release. This ZIP contains built extension
files; Node.js, pnpm and Rust are not needed.

1. Extract the ZIP into a permanent folder. Keep it there while the extension is installed.
2. Open `chrome://extensions` in Chrome and enable **Developer mode**.
3. Click **Load unpacked** and select the extracted folder containing `manifest.json`.
4. Launch Okytes. The desktop app registers its bundled native host for the
   extension's fixed ID automatically.

Keep the desktop app running when sending downloads. The browser and desktop
must use the same user profile. Okytes refreshes registration on every launch,
so an upgrade or a moved portable directory repairs the recorded host path.

The ZIP is an unpacked distribution, not a Chrome Web Store installation.
It requires Developer mode and does not automatically update through Chrome.
To update, extract the new release over the existing extension folder and click
**Reload** on its extension card.
Managed browsers may prohibit unpacked extensions.
