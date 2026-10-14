import { test } from "node:test";
import assert from "node:assert/strict";
import { createOkytes } from "../dist/index.js";
const token = "a".repeat(36) + "." + "b".repeat(64);
test("rejects empty tokens, userinfo and plaintext remote access", () => {
  for (const options of [
    { baseUrl: "http://example.com/", token },
    { baseUrl: "https://user:pass@example.com/", token },
    { baseUrl: "http://127.0.0.1/", token: "" },
  ])
    assert.throws(() => createOkytes(options));
});
test("uses scoped bearer header without following redirects", async () => {
  let request;
  let options;
  const client = createOkytes({
    baseUrl: "http://127.0.0.1:17841/",
    token,
    fetch: async (input, init) => {
      request = input;
      options = init;
      return new Response(
        JSON.stringify({ revision: "1", jobs: [], next: null }),
        { headers: { "Content-Type": "application/json" } },
      );
    },
  });
  const { data } = await client.api.GET("/v1/jobs");
  assert.equal(data.revision, "1");
  assert.equal(request.headers.get("authorization"), `Bearer ${token}`);
  assert.equal(options.redirect, "error");
});
