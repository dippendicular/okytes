# Okytes branding

`okytes_icon_transperant.png` is the app and extension icon.
`okytes_logo_transperant.png` is the full logo used in the desktop sidebar,
browser extension, and project README. Keep their transparency and proportions.
The full logo has dark lettering; display it on a light background.

From the repository root, regenerate the derived assets with:

```sh
pnpm --dir apps/desktop icons
```

This uses the installed Tauri CLI to create Windows ICO, macOS ICNS, desktop PNG,
tray RGBA, favicon, and browser extension icons. It also copies the original
logo into the desktop and extension assets. Commit the generated files with any
change to the originals so regular builds do not need to regenerate them.
