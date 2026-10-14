// Real Windows desktop UI against a separate daemon; isolated profiles only.
import { chromium, expect } from "@playwright/test";
import { spawn } from "node:child_process";
import { mkdirSync, readFileSync, existsSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { createServer } from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";
const root = path.resolve(fileURLToPath(new URL("../../..", import.meta.url)));
const runtime = path.join(root, ".tools", "m4-native-e2e", String(Date.now()));
mkdirSync(runtime, { recursive: true });
const report = {
  runtime,
  platform: process.platform,
  arch: process.arch,
  checks: [],
  errors: [],
};
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
let daemon, app, fixture, browser;
const executable = (name) => path.join(root, "target", "debug", `${name}.exe`);
async function kill(child) {
  if (child && child.exitCode === null) {
    child.kill("SIGKILL");
    await new Promise((resolve) => child.once("exit", resolve));
  }
}
try {
  const port = await new Promise((resolve) => {
    const server = createServer();
    server.listen(0, "127.0.0.1", () => {
      const port = server.address().port;
      server.close(() => resolve(port));
    });
  });
  const url = `http://127.0.0.1:${port}/`;
  const profile = path.join(runtime, "daemon");
  const tokenFile = path.join(profile, "daemon-admin.token");
  daemon = spawn(
    executable("okytes-daemon"),
    ["--bind", `127.0.0.1:${port}`, "--data-dir", profile],
    { windowsHide: true },
  );
  for (let attempt = 0; attempt < 100; attempt++) {
    if (existsSync(tokenFile)) {
      const response = await fetch(new URL("v1/jobs", url), {
        headers: { Authorization: `Bearer ${readFileSync(tokenFile, "utf8")}` },
      }).catch(() => undefined);
      if (response?.ok) break;
    }
    if (daemon.exitCode !== null) throw new Error("Daemon exited");
    await delay(100);
  }
  app = spawn(executable("okytes-desktop"), [], {
    windowsHide: true,
    env: {
      ...process.env,
      OKYTES_DATA_DIR: path.join(runtime, "desktop"),
      OKYTES_DAEMON_URL: url,
      OKYTES_DAEMON_TOKEN_FILE: tokenFile,
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: "--remote-debugging-port=9225",
    },
  });
  for (let attempt = 0; attempt < 100; attempt++) {
    try {
      browser = await chromium.connectOverCDP("http://127.0.0.1:9225");
      break;
    } catch {
      await delay(150);
    }
  }
  if (!browser)
    throw new Error("Native desktop webview did not become available");
  const page = browser.contexts()[0].pages()[0];
  await expect(
    page.getByRole("heading", { name: "All downloads", exact: true }),
  ).toBeVisible();
  fixture = spawn(
    path.join(root, "target", "debug", "examples", "desktop_fixture.exe"),
    [],
    { windowsHide: true },
  );
  const info = await new Promise((resolve, reject) => {
    let text = "";
    const timer = setTimeout(
      () => reject(new Error("Fixture startup timeout")),
      10000,
    );
    fixture.on("error", reject);
    fixture.stdout.on("data", (chunk) => {
      text += chunk;
      if (text.includes("\n")) {
        clearTimeout(timer);
        resolve(JSON.parse(text.trim().split("\n")[0]));
      }
    });
  });
  await page.getByRole("button", { name: /Add downloads/ }).click();
  await page.getByRole("textbox", { name: "URLs", exact: true }).fill(info.url);
  await page.getByRole("button", { name: "Start downloads" }).click();
  const headers = {
    Authorization: `Bearer ${readFileSync(tokenFile, "utf8")}`,
  };
  let job;
  await expect
    .poll(
      async () => {
        const response = await fetch(new URL("v1/jobs", url), { headers });
        const snapshot = await response.json();
        job = snapshot.jobs[0];
        return job?.bytes ?? 0;
      },
      { timeout: 20000 },
    )
    .toBeGreaterThan(0);
  report.checks.push("UI add reaches real daemon with durable progress");
  const response = await fetch(new URL(`v1/jobs/${job.id}/control`, url), {
    method: "POST",
    headers: { ...headers, "Content-Type": "application/json" },
    body: JSON.stringify({ kind: "Pause" }),
  });
  expect(response.ok).toBe(true);
  await expect(page.getByRole("button", { name: /^Resume / })).toBeVisible({
    timeout: 10000,
  });
  await page.getByRole("button", { name: /^Resume / }).click();
  report.checks.push("unchanged UI resumes a daemon-owned job");
  await browser.close();
  browser = undefined;
  await kill(app);
  app = undefined;
  await expect
    .poll(
      async () => {
        const response = await fetch(new URL(`v1/jobs/${job.id}`, url), {
          headers,
        });
        job = await response.json();
        return job.state;
      },
      { timeout: 60000 },
    )
    .toBe("Completed");
  expect(
    createHash("sha256").update(readFileSync(job.destination)).digest("hex"),
  ).toBe(info.sha256);
  report.checks.push(
    "desktop close leaves daemon running; output SHA-256 matches",
  );
} catch (error) {
  report.errors.push(String(error));
  console.error(error);
  process.exitCode = 1;
} finally {
  if (browser) await browser.close().catch(() => {});
  await kill(app);
  await kill(fixture);
  await kill(daemon);
  writeFileSync(
    path.join(runtime, "report.json"),
    JSON.stringify(report, null, 2),
  );
  console.log(`Evidence: ${runtime}`);
}
