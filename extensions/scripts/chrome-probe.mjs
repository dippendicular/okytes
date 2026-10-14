// Inspect extension loading in an isolated installed-Chrome profile. No user
// profile or registry changes. The unsafe debugging flag applies only here.
import { chromium } from "@playwright/test";
import { resolve } from "node:path";
import { mkdir } from "node:fs/promises";
const root = resolve("../.tools/m3-chrome-probe");
await mkdir(root, { recursive: true });
const context = await chromium.launchPersistentContext(root, {
  executablePath:
    process.env.OKYTES_CHROME ??
    "C:/Program Files/Google/Chrome/Application/chrome.exe",
  headless: true,
  ignoreDefaultArgs: ["--disable-extensions"],
  args: ["--enable-unsafe-extension-debugging"],
});
try {
  console.log(`Browser: ${context.browser().version()}`);
  const cdp = await context.browser().newBrowserCDPSession();
  const loaded = await cdp.send("Extensions.loadUnpacked", {
    path: resolve("dist/chromium"),
  });
  console.log(JSON.stringify(loaded));
  const page = await context.newPage();
  await page.goto(`chrome-extension://${loaded.id}/popup.html`);
  await page.waitForTimeout(10000);
  console.log(await page.locator("#status").textContent());
} finally {
  await context.close();
}
