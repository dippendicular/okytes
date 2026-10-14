// Exercises the actual Windows Tauri WebView and in-process engine. No bridge mocks.
import { chromium, expect } from "@playwright/test";
import { spawn } from "node:child_process";
import { readFileSync, mkdirSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
import path from "node:path";

const root = path.resolve(fileURLToPath(new URL("../../..", import.meta.url)));
const runtime = path.join(root, ".tools", "m2-native-e2e", String(Date.now()));
mkdirSync(runtime, { recursive: true });
const executable = path.join(root, "target", "debug", "okytes-desktop.exe");
const fixtureExecutable = path.join(
  root,
  "target",
  "debug",
  "examples",
  "desktop_fixture.exe",
);
const fixture = spawn(fixtureExecutable, [], { windowsHide: true });
let app, browser;
const report = {
  platform: process.platform,
  arch: process.arch,
  checks: [],
  errors: [],
  runtime,
};
function mark(check, evidence) {
  report.checks.push({ check, evidence });
  console.log(JSON.stringify({ check, evidence }));
}
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function start() {
  app = spawn(executable, [], {
    windowsHide: true,
    env: {
      ...process.env,
      OKYTES_DATA_DIR: path.join(runtime, "data"),
      OKYTES_DOWNLOAD_DIR: path.join(runtime, "downloads"),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: "--remote-debugging-port=9223",
    },
  });
  app.on("error", (error) => report.errors.push(String(error)));
  let connected;
  for (let attempt = 0; attempt < 80; attempt++) {
    try {
      connected = await chromium.connectOverCDP("http://127.0.0.1:9223");
      break;
    } catch {
      await delay(250);
    }
  }
  if (!connected)
    throw new Error("Windows WebView CDP endpoint did not become available");
  browser = connected;
  const page = browser.contexts()[0].pages()[0];
  page.on("pageerror", (error) => report.errors.push(String(error)));
  await expect(
    page.getByRole("heading", { name: "All downloads", exact: true }),
  ).toBeVisible({ timeout: 30000 });
  return page;
}
async function stop() {
  if (browser) {
    await browser.close().catch(() => {});
    browser = undefined;
  }
  if (app) {
    app.kill("SIGKILL");
    await new Promise((resolve) => {
      if (app.exitCode !== null) resolve();
      else app.once("exit", resolve);
    });
    app = undefined;
  }
}
async function add(page, urls) {
  await page.getByRole("button", { name: /Add downloads/ }).click();
  await page
    .getByRole("textbox", { name: "URLs", exact: true })
    .fill(urls.join("\n"));
  await page.getByRole("button", { name: "Start downloads" }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0, { timeout: 30000 });
}
async function details(page, name) {
  const row = page
    .getByRole("listitem")
    .filter({ has: page.getByRole("button", { name, exact: true }) });
  await row
    .getByRole("button", { name: `Details for ${name}`, exact: true })
    .click();
  return page.getByRole("dialog");
}
try {
  const fixtureInfo = await new Promise((resolve, reject) => {
    let text = "";
    fixture.stdout.on("data", (chunk) => {
      text += chunk;
      const line = text.split("\n")[0];
      try {
        resolve(JSON.parse(line));
      } catch {}
    });
    fixture.on("error", reject);
    fixture.on("exit", (code) => reject(new Error(`fixture exited ${code}`)));
  });
  const source = new URL(fixtureInfo.url);
  let page = await start();
  mark("native startup", await page.title());
  await add(page, [
    new URL("/first.zip", source).href,
    new URL("/second.pdf", source).href,
    new URL("/third.bin", source).href,
  ]);
  const first = page.getByRole("listitem").filter({
    has: page.getByRole("button", { name: "first.zip", exact: true }),
  });
  await expect(first.getByText("Downloading", { exact: true })).toBeVisible({
    timeout: 30000,
  });
  await page.waitForFunction(
    () => [...document.querySelectorAll("progress")].some((p) => p.value > 0),
    { timeout: 30000 },
  );
  await first.getByRole("button", { name: "Pause first.zip" }).click();
  await expect(first.getByText("Paused", { exact: true })).toBeVisible({
    timeout: 30000,
  });
  mark(
    "live progress and pause",
    "UI reported nonzero durable bytes and reached Paused",
  );
  await first.getByRole("button", { name: "Resume first.zip" }).click();
  await expect(first.getByText("Downloading", { exact: true })).toBeVisible({
    timeout: 30000,
  });
  const durableBeforeKill = Number(
    await first
      .getByRole("progressbar", { name: "first.zip progress", exact: true })
      .getAttribute("value"),
  );
  expect(durableBeforeKill).toBeGreaterThan(0);
  expect(durableBeforeKill).toBeLessThan(8 * 1024 * 1024);
  mark("durable bytes before force kill", durableBeforeKill);
  await stop();
  page = await start();
  await expect(
    page
      .getByRole("listitem")
      .filter({
        has: page.getByRole("button", { name: "first.zip", exact: true }),
      })
      .getByText("Completed", { exact: true }),
  ).toBeVisible({ timeout: 60000 });
  for (const name of ["first.zip", "second.pdf", "third.bin"]) {
    await expect(
      page
        .getByRole("listitem")
        .filter({ has: page.getByRole("button", { name, exact: true }) })
        .getByText("Completed", { exact: true }),
    ).toBeVisible({ timeout: 60000 });
    const hash = createHash("sha256")
      .update(readFileSync(path.join(runtime, "downloads", name)))
      .digest("hex");
    expect(hash).toBe(fixtureInfo.sha256);
  }
  mark(
    "force quit and recovery",
    `three SHA-256 verified files: ${fixtureInfo.sha256}`,
  );
  await page.evaluate(
    readFileSync(
      path.join(
        root,
        "apps",
        "desktop",
        "node_modules",
        "axe-core",
        "axe.min.js",
      ),
      "utf8",
    ),
  );
  const accessibility = await page.evaluate(async () => {
    const result = await window.axe.run();
    return result.violations.map((v) => ({
      id: v.id,
      nodes: v.nodes.map((n) => n.target),
    }));
  });
  expect(accessibility).toEqual([]);
  mark("native accessibility including contrast", "axe: zero violations");
  let dialog = await details(page, "first.zip");
  await expect(
    dialog.getByText("HTTP response", { exact: true }),
  ).toBeVisible();
  await dialog.getByRole("button", { name: "Download again" }).click();
  await dialog.getByRole("button", { name: "Close dialog" }).click();
  mark(
    "diagnostics and restart",
    "real diagnostics displayed and a fresh job added",
  );
  await page.getByRole("button", { name: "Pause all", exact: true }).click();
  for (let batch = 0; batch < 5; batch++) {
    await add(
      page,
      Array.from(
        { length: 100 },
        (_, index) => new URL(`/queue-${batch * 100 + index}.zip`, source).href,
      ),
    );
    await page.getByRole("button", { name: "Pause all", exact: true }).click();
  }
  await expect(page.locator("header .muted")).toContainText("504 downloads", {
    timeout: 30000,
  });
  const rendered = await page.getByRole("listitem").count();
  expect(rendered).toBeLessThan(40);
  const started = Date.now();
  await page
    .getByRole("searchbox", { name: "Search downloads" })
    .fill("queue-499");
  await expect(
    page.getByRole("button", { name: "queue-499.zip", exact: true }),
  ).toBeVisible({ timeout: 30000 });
  mark("500-job list", {
    total: 504,
    rendered,
    searchMilliseconds: Date.now() - started,
  });
  dialog = await details(page, "queue-499.zip");
  await expect(dialog.getByText(/0 active \/ 0 total/)).toBeVisible();
  await dialog.getByLabel("Filename", { exact: true }).fill("renamed-499.zip");
  await dialog.getByRole("button", { name: "Rename", exact: true }).click();
  await expect(
    dialog.getByRole("heading", { name: "renamed-499.zip", exact: true }),
  ).toBeVisible();
  await dialog
    .getByRole("button", { name: "Cancel download", exact: true })
    .click();
  await expect(
    dialog.getByRole("button", { name: "Cancel download", exact: true }),
  ).toHaveCount(0);
  await dialog
    .getByRole("button", { name: "Remove from history", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.getByRole("searchbox", { name: "Search downloads" }).fill("");
  await expect(page.locator("header .muted")).toContainText("503 downloads");
  mark(
    "rename, cancel and remove",
    "paused job renamed, cancelled, then removed through the UI",
  );
  await page.getByRole("button", { name: /Add downloads/ }).click();
  await page
    .getByRole("textbox", { name: "URLs", exact: true })
    .fill(new URL("/first.zip", source).href);
  await page
    .getByRole("button", { name: "Start downloads", exact: true })
    .click();
  await expect(page.getByRole("dialog").getByRole("alert")).toContainText(
    "duplicate URL",
  );
  await page
    .getByLabel("Add duplicates, even if these URLs are already in history")
    .check();
  await page
    .getByRole("button", { name: "Start downloads", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.getByRole("button", { name: "Pause all", exact: true }).click();
  mark(
    "explicit duplicate choice",
    "duplicate rejected until user selected Add duplicates",
  );
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  dialog = page.getByRole("dialog");
  await dialog
    .getByRole("combobox", { name: /^Appearance/ })
    .selectOption("dark");
  await dialog.getByLabel("Simultaneous downloads", { exact: true }).fill("2");
  await dialog
    .getByLabel("Desktop notifications on completion and failure")
    .uncheck();
  await dialog
    .getByRole("button", { name: "Save settings", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  const darkAccessibility = await page.evaluate(async () => {
    const result = await window.axe.run();
    return result.violations.map((v) => ({
      id: v.id,
      nodes: v.nodes.map((n) => n.target),
    }));
  });
  expect(darkAccessibility).toEqual([]);
  mark(
    "dark accessibility including contrast",
    "axe: zero violations with 504 real jobs",
  );
  await page.keyboard.press("Control+f");
  await expect(
    page.getByRole("searchbox", { name: "Search downloads" }),
  ).toBeFocused();
  await page.keyboard.press("F1");
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  mark(
    "keyboard shortcuts",
    "Ctrl+F focuses search, F1 opens shortcut guide, Escape closes it",
  );
  await stop();
  page = await start();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  dialog = page.getByRole("dialog");
  await expect(
    dialog.getByLabel("Simultaneous downloads", { exact: true }),
  ).toHaveValue("2");
  await expect(
    dialog.getByLabel("Desktop notifications on completion and failure"),
  ).not.toBeChecked();
  await dialog
    .getByRole("button", { name: "Close dialog", exact: true })
    .click();
  mark(
    "persisted settings",
    "dark theme, concurrency 2 and notifications off survived a process restart",
  );
  await page.getByLabel("Import URL file").setInputFiles({
    name: "urls.txt",
    mimeType: "text/plain",
    buffer: Buffer.from(new URL("/imported.zip", source).href),
  });
  await expect(
    page.getByRole("textbox", { name: "URLs", exact: true }),
  ).toHaveValue(new URL("/imported.zip", source).href);
  await page
    .getByRole("button", { name: "Start downloads", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.getByRole("button", { name: "Pause all", exact: true }).click();
  const transfer = await page.evaluateHandle((url) => {
    const data = new DataTransfer();
    data.setData("text/uri-list", url);
    return data;
  }, new URL("/dropped.zip", source).href);
  await page.locator(".app").dispatchEvent("drop", { dataTransfer: transfer });
  await expect(
    page.getByRole("textbox", { name: "URLs", exact: true }),
  ).toHaveValue(new URL("/dropped.zip", source).href);
  await page
    .getByRole("button", { name: "Start downloads", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.getByRole("button", { name: "Pause all", exact: true }).click();
  await expect(page.locator("header .muted")).toContainText("506 downloads");
  mark(
    "file import and URL drop",
    "real browser file read and HTML5 drop both added downloads through the engine",
  );
  await page.getByRole("searchbox", { name: "Search downloads" }).fill("");
  await page.screenshot({ path: path.join(runtime, "dashboard.png") });
  mark("screenshot", path.join(runtime, "dashboard.png"));
  writeFileSync(
    path.join(runtime, "report.json"),
    JSON.stringify(report, null, 2),
  );
} catch (error) {
  report.errors.push(String(error));
  if (browser) {
    const page = browser.contexts()[0]?.pages()[0];
    if (page)
      await page
        .screenshot({ path: path.join(runtime, "failure.png") })
        .catch(() => {});
  }
  writeFileSync(
    path.join(runtime, "report.json"),
    JSON.stringify(report, null, 2),
  );
  console.error(error);
  process.exitCode = 1;
} finally {
  await stop();
  fixture.kill("SIGKILL");
}
