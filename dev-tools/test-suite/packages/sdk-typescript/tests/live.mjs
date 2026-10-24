// Invoked by a Rust harness hosting the production API and a real fault server.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createOkytes } from "../../../../../packages/sdk-typescript/dist/index.js";
const token = readFileSync(process.env.OKYTES_SDK_TOKEN_FILE, "utf8");
const { api, events } = createOkytes({
  baseUrl: process.env.OKYTES_SDK_URL,
  token,
});
const created = await api.POST("/v1/submit", {
  body: {
    url: process.env.OKYTES_SDK_SOURCE,
    destination: null,
    filename: "sdk.bin",
    sha256: process.env.OKYTES_SDK_SHA,
    conflict: "fail",
  },
});
assert.ok(created.data, JSON.stringify(created.error));
const id = created.data;
const stopped = await api.POST("/v1/jobs/{id}/control", {
  params: { path: { id } },
  body: { kind: "Pause" },
});
assert.equal(stopped.response.status, 200);
const paused = await api.GET("/v1/jobs/{id}", { params: { path: { id } } });
assert.equal(paused.data.state, "Paused");
const abort = new AbortController();
const stream = events(abort.signal);
const first = await stream.next();
assert.equal(first.value.kind, "Resync");
await stream.return();
abort.abort();
await api.POST("/v1/jobs/{id}/control", {
  params: { path: { id } },
  body: { kind: "Resume" },
});
const end = Date.now() + 30000;
let job;
do {
  const result = await api.GET("/v1/jobs/{id}", { params: { path: { id } } });
  job = result.data;
  assert.notEqual(job.state, "Failed");
  if (job.state === "Completed") break;
  await new Promise((resolve) => setTimeout(resolve, 100));
} while (Date.now() < end);
assert.equal(job.state, "Completed");
console.log(JSON.stringify({ id, destination: job.destination }));
