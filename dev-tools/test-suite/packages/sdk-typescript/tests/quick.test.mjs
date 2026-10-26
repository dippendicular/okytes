import { test } from "node:test";
import assert from "node:assert/strict";
import { createQuick } from "../../../../../dev-tools/sdk/packages/sdk-typescript/dist/index.js";

test("Quick browser requests keep proof in memory and never use a native bearer or URL credential", async () => {
  const calls = [];
  const client = createQuick(async (path, init) => {
    calls.push({ path, ...init });
    return Response.json(path === "/quick/session" ? { csrf: "memory-proof" } : []);
  });
  await client.session("one-use-code");
  await client.add({ urls: ["https://example.invalid/file"], destination: null, allow_duplicates: false });
  assert.equal(calls[1].headers["X-Okytes-CSRF"], "memory-proof");
  for (const call of calls) {
    assert.equal(call.credentials, "same-origin");
    assert.equal(call.redirect, "error");
    assert.equal(call.headers.Authorization, undefined);
    assert.ok(!call.path.includes("one-use-code") && !call.path.includes("memory-proof"));
  }
  await client.logout();
  await client.session();
  assert.equal(calls.at(-1).headers["X-Okytes-CSRF"], undefined);
});

test("expired sessions clear the in-memory proof and return a sanitized error", async () => {
  let paired = false;
  const client = createQuick(async () => {
    if (!paired) { paired = true; return Response.json({ csrf: "old-proof" }); }
    return Response.json({ sensitive: "MUST_NOT_APPEAR" }, { status: 401 });
  });
  await client.session("code");
  await assert.rejects(client.list(), error => error.message.includes("expired") && !error.message.includes("MUST_NOT_APPEAR"));
});
