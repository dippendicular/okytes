// Windows installed-Chrome test. Uses isolated profiles and temporarily registers
// this host only, restoring all pre-existing registration keys in finally.
import { chromium, expect } from "@playwright/test";
import { spawn, spawnSync } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { resolve, join } from "node:path";
import { fileURLToPath } from "node:url";
const root = resolve(fileURLToPath(new URL("../..", import.meta.url)));
const runtime = join(root, ".tools", "m3-chrome-e2e", String(Date.now()));
mkdirSync(runtime, { recursive: true });
const env = {
  ...process.env,
  OKYTES_DATA_DIR: join(runtime, "data"),
  OKYTES_DOWNLOAD_DIR: join(runtime, "downloads"),
};
const registry = ["Google\\Chrome", "Microsoft\\Edge", "Mozilla"].map(
  (browser) =>
    `HKCU\\Software\\${browser}\\NativeMessagingHosts\\org.okytes.bridge`,
);
const backups = [];
const report = {
  platform: process.platform,
  arch: process.arch,
  checks: [],
  errors: [],
  runtime,
};
let context, app, webview, fixture;
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const mark = (check, evidence) => {
  report.checks.push({ check, evidence });
  console.log(JSON.stringify({ check, evidence }));
};
async function kill(child) {
  if (child && child.exitCode === null) {
    child.kill("SIGKILL");
    await new Promise((resolve) => child.once("exit", resolve));
  }
}
async function jobs(page) {
  return page.evaluate(() =>
    window.__TAURI_INTERNALS__.invoke("list", { offset: 0 }),
  );
}
async function completed(page, id, sha) {
  let job;
  await expect
    .poll(
      async () => {
        job = (await jobs(page)).jobs.find((job) => job.id === id);
        return job?.state;
      },
      { timeout: 60000 },
    )
    .toBe("Completed");
  expect(
    createHash("sha256").update(readFileSync(job.destination)).digest("hex"),
  ).toBe(sha);
}
try {
  context = await chromium.launchPersistentContext(join(runtime, "chrome"), {
    executablePath:
      process.env.OKYTES_CHROME ??
      "C:/Program Files/Google/Chrome/Application/chrome.exe",
    headless: true,
    acceptDownloads: true,
    env,
    ignoreDefaultArgs: ["--disable-extensions"],
    args: ["--enable-unsafe-extension-debugging"],
  });
  report.browser = context.browser().version();
  const cdp = await context.browser().newBrowserCDPSession();
  const { id } = await cdp.send("Extensions.loadUnpacked", {
    path: join(root, "extensions", "dist", "chromium"),
  });
  report.extensionId = id;
  for (const [index, key] of registry.entries()) {
    const file = join(runtime, `registry-${index}.reg`);
    const existing = spawnSync("reg.exe", ["query", key], {
      windowsHide: true,
    });
    if (existing.status === 0) {
      const exported = spawnSync("reg.exe", ["export", key, file, "/y"], {
        windowsHide: true,
      });
      if (exported.status !== 0)
        throw new Error("Could not back up host registration");
      backups.push({ key, file });
    } else backups.push({ key, file: null });
  }
  const installed = spawnSync(
    join(root, "target", "debug", "okytes-native-host.exe"),
    ["install", "--chromium-id", id],
    { windowsHide: true, env, encoding: "utf8" },
  );
  if (installed.status !== 0)
    throw new Error(`Host installation failed: ${installed.stderr}`);
  app = spawn(join(root, "target", "debug", "okytes-desktop.exe"), [], {
    windowsHide: true,
    env: {
      ...env,
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: "--remote-debugging-port=9224",
    },
  });
  app.on("error", (error) => report.errors.push(String(error)));
  for (let attempt = 0; attempt < 100; attempt++) {
    try {
      webview = await chromium.connectOverCDP("http://127.0.0.1:9224");
      break;
    } catch {
      await delay(250);
    }
  }
  if (!webview) throw new Error("Desktop webview unavailable");
  const desktop = webview.contexts()[0].pages()[0];
  const popup = await context.newPage();
  await popup.goto(`chrome-extension://${id}/popup.html`);
  const diagnostic = await popup.evaluate(
    () =>
      new Promise((resolve) => {
        const port = chrome.runtime.connectNative("org.okytes.bridge");
        port.onDisconnect.addListener(() =>
          resolve({
            disconnected: chrome.runtime.lastError?.message ?? "closed",
          }),
        );
        port.onMessage.addListener((message) => {
          resolve(message);
          port.disconnect();
        });
        port.postMessage({
          version: 1,
          id: crypto.randomUUID(),
          operation: { kind: "Ping" },
        });
      }),
  );
  console.log(JSON.stringify({ nativeDiagnostic: diagnostic }));
  await expect(popup.locator("#status")).toHaveText("Desktop connected", {
    timeout: 15000,
  });
  mark(
    "real Chrome extension connects through registered native host",
    report.browser,
  );
  fixture = spawn(
    join(root, "target", "debug", "examples", "desktop_fixture.exe"),
    [],
    { windowsHide: true },
  );
  const info = await new Promise((resolve, reject) => {
    let data = "";
    fixture.stdout.on("data", (chunk) => {
      data += chunk;
      if (data.includes("\n")) resolve(JSON.parse(data.trim().split("\n")[0]));
    });
    fixture.on("error", reject);
  });
  const base = new URL(info.url).origin;
  await popup.locator("#urls").fill(`${base}/browser-first.zip`);
  await popup.locator("#send").click();
  await expect(popup.locator("#status")).toContainText("1 sent to Okytes", {
    timeout: 15000,
  });
  let first = (await jobs(desktop)).jobs.find((job) =>
    job.url.endsWith("/browser-first.zip"),
  );
  expect(first).toBeTruthy();
  await completed(desktop, first.id, info.sha256);
  mark("popup capture completed with SHA-256", {
    id: first.id,
    sha256: info.sha256,
  });
  const options = await context.newPage();
  await options.goto(`chrome-extension://${id}/options.html`);
  await options.locator("#enabled").check();
  await options.getByRole("button", { name: "Save rules" }).click();
  await expect(options.locator("#status")).toHaveText("Rules saved.");
  // Browser download triggered from a normal page, not downloads.download from
  // the extension, which is intentionally excluded from interception.
  const source = await context.newPage();
  await source.goto(`${base}/browser-page`);
  const downloadEvent = source.waitForEvent("download");
  await source.locator("#auto").click();
  const download = await downloadEvent;
  await expect
    .poll(
      async () =>
        (await jobs(desktop)).jobs.some((job) =>
          job.url.endsWith("/browser-auto.zip"),
        ),
      { timeout: 15000 },
    )
    .toBe(true);
  const automatic = (await jobs(desktop)).jobs.find((job) =>
    job.url.endsWith("/browser-auto.zip"),
  );
  await completed(desktop, automatic.id, info.sha256);
  expect(await download.failure()).toBeTruthy();
  mark(
    "downloads API interception and acknowledged cancellation",
    automatic.id,
  );
  // A third transfer remains engine-owned after uninstalling the extension.
  await popup.locator("#urls").fill(`${base}/extension-kill.zip`);
  await popup.locator("#send").click();
  await expect(popup.locator("#status")).toContainText("1 sent to Okytes", {
    timeout: 15000,
  });
  const third = (await jobs(desktop)).jobs.find((job) =>
    job.url.endsWith("/extension-kill.zip"),
  );
  expect(third.state).not.toBe("Completed");
  await cdp.send("Extensions.uninstall", { id });
  await completed(desktop, third.id, info.sha256);
  mark("extension termination leaves active download intact", third.id);
  // Reinstall with persisted non-sensitive rules, then close the actual app.
  const reloaded = await cdp.send("Extensions.loadUnpacked", {
    path: join(root, "extensions", "dist", "chromium"),
  });
  const settings = await context.newPage();
  await settings.goto(`chrome-extension://${reloaded.id}/options.html`);
  await settings.locator("#enabled").check();
  await settings.getByRole("button", { name: "Save rules" }).click();
  await webview.close();
  webview = undefined;
  await kill(app);
  app = undefined;
  const fallbackEvent = source.waitForEvent("download");
  await source.locator("#fallback").click();
  const fallback = await fallbackEvent;
  const file = join(runtime, "fallback.zip");
  await fallback.saveAs(file);
  expect(await fallback.failure()).toBeNull();
  expect(createHash("sha256").update(readFileSync(file)).digest("hex")).toBe(
    info.sha256,
  );
  mark(
    "app-closed browser-native fallback completes with SHA-256",
    info.sha256,
  );
  await expect
    .poll(
      async () => await settings.evaluate(() => chrome.storage.local.get(null)),
    )
    .toEqual({
      rules: {
        version: 1,
        enabled: true,
        allow: [],
        deny: [],
        types: [],
        minBytes: 0,
        maxBytes: 0,
      },
    });
  mark("extension storage contains only non-sensitive rules", true);
} catch (error) {
  report.errors.push(String(error));
  process.exitCode = 1;
  console.error(error);
} finally {
  if (webview) await webview.close().catch(() => {});
  await kill(app);
  await kill(fixture);
  if (context) await context.close().catch(() => {});
  for (const backup of backups) {
    const restored = backup.file
      ? spawnSync("reg.exe", ["import", backup.file], { windowsHide: true })
      : spawnSync("reg.exe", ["delete", backup.key, "/f"], {
          windowsHide: true,
        });
    if (restored.status !== 0) {
      report.errors.push(`Host registration cleanup failed: ${backup.key}`);
      process.exitCode = 1;
    }
  }
  writeFileSync(join(runtime, "report.json"), JSON.stringify(report, null, 2));
  console.log(`Evidence: ${runtime}`);
}
