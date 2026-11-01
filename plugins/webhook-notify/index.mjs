// Official example plugin. Listens to daemon events with a token limited to
// events:read and jobs:read, and posts a short message per finished download.
// Messages contain the file name and outcome only, never the download URL.
import { fileURLToPath } from "node:url";
import { Plugin, loadManifest } from "@okytes/sdk/plugin";

const target = process.env.OKYTES_WEBHOOK_URL;
const format = (process.env.OKYTES_WEBHOOK_FORMAT ?? "json").toLowerCase();
if (!target) {
  console.error("Set OKYTES_WEBHOOK_URL (and optionally OKYTES_WEBHOOK_FORMAT=ntfy|discord|slack|json).");
  process.exit(2);
}
const url = new URL(target);
const loopback = ["127.0.0.1", "localhost", "[::1]"].includes(url.hostname);
if (url.protocol !== "https:" && !(url.protocol === "http:" && loopback)) {
  console.error("Use an https:// webhook (plain http is allowed only on this computer).");
  process.exit(2);
}
if (!["ntfy", "discord", "slack", "json"].includes(format)) {
  console.error(`Unknown OKYTES_WEBHOOK_FORMAT ${format}`);
  process.exit(2);
}

const manifest = await loadManifest(fileURLToPath(new URL(".", import.meta.url)));
const plugin = await Plugin.fromEnvironment(manifest);
plugin.require("events:read");
plugin.require("jobs:read");

// Engine errors may quote the link, including signed query tokens.
const redact = (text) => text.replace(/[a-z][a-z0-9+.-]*:\/\/\S+/gi, "[link]");

async function post(text, failed) {
  const body =
    format === "ntfy"
      ? text
      : JSON.stringify(
          format === "discord" ? { content: text } : format === "slack" ? { text } : { text, failed },
        );
  const headers =
    format === "ntfy"
      ? { Title: failed ? "Download failed" : "Download complete", Tags: failed ? "warning" : "white_check_mark" }
      : { "Content-Type": "application/json" };
  const response = await fetch(url, { method: "POST", headers, body, redirect: "error", signal: AbortSignal.timeout(10_000) });
  if (!response.ok) throw new Error(`webhook returned ${response.status}`);
}

// Completed jobs are announced once per process; a Resync never replays them.
const announced = new Set();
console.error(`okytes webhook plugin: listening (${format})`);
for await (const update of plugin.client.events()) {
  if (update.kind !== "Changed") continue;
  const event = update.value;
  const done = event.JobCompleted ?? event.JobFailed;
  if (!done || announced.has(done.job_id)) continue;
  announced.add(done.job_id);
  if (announced.size > 10_000) announced.clear();
  try {
    const job = await plugin.client.job(done.job_id);
    const failed = Boolean(event.JobFailed);
    await post(failed ? `❌ ${job.name} failed: ${redact(job.error ?? "unknown error")}` : `✅ ${job.name} finished`, failed);
  } catch (error) {
    console.error(`okytes webhook plugin: ${error.message}`);
  }
}
