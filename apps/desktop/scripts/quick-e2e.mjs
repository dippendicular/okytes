// Real browser, native daemon and M1 fault server. All runtime state is private.
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { createServer } from "node:net";
import http from "node:http";
import path from "node:path";
import { fileURLToPath } from "node:url";
import axe from "axe-core";
const root = fileURLToPath(new URL("../../..", import.meta.url));
process.env.PLAYWRIGHT_BROWSERS_PATH ??= path.join(root, ".tools", "browsers");
const { chromium, firefox, expect } = await import("@playwright/test");
const runtime = path.join(root, ".tools", "m9-browser", String(Date.now()));
await mkdir(runtime, { recursive: true });
const suffix = process.platform === "win32" ? ".exe" : "";
const binary =
  process.env.OKYTES_QUICK_DAEMON ??
  path.join(root, "target", "debug", `okytes-daemon${suffix}`);
const cliBinary =
  process.env.OKYTES_QUICK_CLI ??
  path.join(root, "target", "debug", `okytes-cli${suffix}`);
const fixtureBinary =
  process.env.OKYTES_DESKTOP_FIXTURE ??
  path.join(root, "target", "debug", "examples", `desktop_fixture${suffix}`);
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function port() {
  const socket = createServer();
  await new Promise((resolve) => socket.listen(0, "127.0.0.1", resolve));
  const result = socket.address().port;
  await new Promise((resolve) => socket.close(resolve));
  return result;
}
const report = {
  platform: process.platform,
  arch: process.arch,
  browsers: [],
  checks: [],
  failures: [],
};
const secrets = [];
let daemon, browser, fixture, evil;
let cookieSeenByOtherListener = "";
try {
  fixture = spawn(fixtureBinary, [], {
    windowsHide: true,
    env: { ...process.env, OKYTES_FIXTURE_FRAME_MS: "100" },
  });
  const source = await new Promise((resolve, reject) => {
    let pending = "";
    const timer = setTimeout(
      () => reject(new Error("Fault server did not start")),
      20000,
    );
    fixture.once("error", reject);
    fixture.stdout.on("data", (chunk) => {
      pending += chunk;
      if (pending.includes("\n")) {
        clearTimeout(timer);
        resolve(JSON.parse(pending.split("\n")[0]));
      }
    });
  });
  const endpoint = `http://127.0.0.1:${await port()}`;
  const profile = path.join(runtime, "profile");
  let token;
  const native = async (route, method = "GET", body) => {
    const response = await fetch(`${endpoint}/v1/${route}`, {
      method,
      headers: {
        Authorization: `Bearer ${token}`,
        ...(body ? { "Content-Type": "application/json" } : {}),
      },
      body: body ? JSON.stringify(body) : undefined,
      redirect: "error",
    });
    if (!response.ok)
      throw new Error(`Native verification failed (${response.status})`);
    return response.json();
  };
  async function start() {
    daemon = spawn(
      binary,
      ["--quick", "--bind", new URL(endpoint).host, "--data-dir", profile],
      { windowsHide: true, stdio: "ignore" },
    );
    await expect
      .poll(
        async () => {
          try {
            token = (
              await readFile(path.join(profile, "daemon-admin.token"), "utf8")
            ).trim();
            return (await fetch(`${endpoint}/quick`)).status;
          } catch {
            return 0;
          }
        },
        { timeout: 30000 },
      )
      .toBe(200);
    secrets.push(token);
  }
  async function stop() {
    if (daemon && daemon.exitCode === null) {
      daemon.kill();
      await new Promise((resolve) => daemon.once("exit", resolve));
    }
  }
  await start();
  evil = http.createServer((request, response) => {
    cookieSeenByOtherListener = request.headers.cookie ?? "";
    response.end(
      "<!doctype html><html lang='en'><title>Untrusted origin</title><body>Untrusted origin</body></html>",
    );
  });
  await new Promise((resolve) => evil.listen(0, "127.0.0.1", resolve));
  const browsers = process.env.OKYTES_QUICK_BROWSERS?.split(",") ?? [
    "chrome",
    "firefox",
  ];
  for (const name of browsers) {
    browser =
      name === "firefox"
        ? await firefox.launch({ headless: true })
        : await chromium.launch({
            ...(name === "chrome" ? { channel: "chrome" } : {}),
            headless: true,
          });
    report.browsers.push({ name, version: browser.version(), endpoint });
    const context = await browser.newContext({
      viewport: { width: 390, height: 844 },
    });
    const messages = [];
    let page = await context.newPage();
    page.on("console", (message) => messages.push(message.text()));
    async function pair() {
      const { stdout } = await promisify(execFile)(
        cliBinary,
        [
          "--daemon-url",
          `${endpoint}/`,
          "--token-file",
          path.join(profile, "daemon-admin.token"),
          "--json",
          "quick",
        ],
        { windowsHide: true, timeout: 15000 },
      );
      const approval = JSON.parse(stdout);
      secrets.push(approval.code);
      await page.getByLabel("Pairing code").fill(approval.code);
      await page.getByRole("button", { name: "Connect", exact: true }).click();
      await expect(page.getByLabel("Download links")).toBeVisible();
    }
    await page.goto(`${endpoint}/quick`);
    await pair();
    await page.evaluate(axe.source);
    const violations = await page.evaluate(async () =>
      (await window.axe.run()).violations.map((value) => value.id),
    );
    expect(violations).toEqual([]);
    expect(
      await page.evaluate(() => [localStorage.length, sessionStorage.length]),
    ).toEqual([0, 0]);
    await page
      .getByLabel("Download links")
      .fill(`${source.url}?browser=${name}`);
    await page.getByRole("button", { name: "Download", exact: true }).click();
    let job;
    await expect
      .poll(
        async () => {
          const snapshot = await native("jobs");
          job = snapshot.jobs.find((value) =>
            value.url.includes(`browser=${name}`),
          );
          return job?.bytes ?? 0;
        },
        { timeout: 15000 },
      )
      .toBeGreaterThan(0);
    expect(job.state).not.toBe("Completed");
    const before = job.bytes;
    await page.close();
    await expect
      .poll(async () => (await native(`jobs/${job.id}`)).bytes, {
        timeout: 15000,
      })
      .toBeGreaterThan(before);
    await stop();
    await start();
    page = await context.newPage();
    await page.goto(`${endpoint}/quick`);
    await pair();
    await expect
      .poll(async () => (await native(`jobs/${job.id}`)).state, {
        timeout: 60000,
      })
      .toBe("Completed");
    job = await native(`jobs/${job.id}`);
    const hash = createHash("sha256")
      .update(await readFile(job.destination))
      .digest("hex");
    expect(hash).toBe(source.sha256);
    await expect(
      page.getByRole("heading", { name: job.name, exact: true }),
    ).toBeVisible();
    await page.screenshot({
      path: path.join(runtime, `${name}.png`),
      fullPage: true,
    });
    const count = (await native("jobs")).jobs.length;
    const attacker = await context.newPage();
    await attacker.goto(`http://127.0.0.1:${evil.address().port}`);
    expect(cookieSeenByOtherListener).toContain("okytes_quick_");
    const stolenCookieResponse = await fetch(`${endpoint}/quick/session`, {
      headers: { Cookie: cookieSeenByOtherListener, "X-Okytes-Quick": "1" },
    });
    expect(stolenCookieResponse.status).toBe(401);
    const allowed = await attacker.evaluate(async (base) => {
      try {
        const response = await fetch(`${base}/v1/jobs`, {
          method: "POST",
          credentials: "include",
          headers: {
            "Content-Type": "application/json",
            "X-Okytes-Quick": "1",
            "X-Okytes-CSRF": "guessed",
          },
          body: JSON.stringify({
            urls: ["https://example.invalid/attack"],
            destination: null,
            allow_duplicates: false,
          }),
        });
        return response.ok;
      } catch {
        return false;
      }
    }, endpoint);
    expect(allowed).toBe(false);
    expect((await native("jobs")).jobs.length).toBe(count);
    expect(
      messages.some((message) =>
        secrets.some((secret) => message.includes(secret)),
      ),
    ).toBe(false);
    await stop();
    await expect(
      page.getByText(/Service unavailable or session expired/),
    ).toBeVisible({ timeout: 12000 });
    await start();
    report.checks.push({
      browser: name,
      checks: [
        "pairing",
        "phone-width",
        "axe-AA",
        "no-persistent-browser-storage",
        "real-download",
        "tab-close-continues",
        "forced-daemon-restart",
        "SHA256",
        "re-pair-resync",
        "hostile-origin-denied",
        "stopped-service",
      ],
      sha256: hash,
    });
    await context.close();
    await browser.close();
    browser = undefined;
  }
  await stop();
} catch (error) {
  let message = String(error);
  for (const secret of secrets)
    message = message.replaceAll(secret, "<redacted>");
  report.failures.push(message);
  process.exitCode = 1;
} finally {
  await browser?.close().catch(() => {});
  daemon?.kill();
  fixture?.kill();
  evil?.close();
  await writeFile(
    path.join(runtime, "report.json"),
    JSON.stringify(report, null, 2),
  );
  console.log(JSON.stringify(report, null, 2));
}
