import createClient from "openapi-fetch";
import type { paths, components } from "./schema.js";
export type { paths, components } from "./schema.js";
export { createQuick } from "./quick.js";
export { OkytesClient, OkytesError } from "./node.js";
export type { ClientOptions, TokenProvider } from "./node.js";
export interface Options {
  baseUrl: string;
  token: string;
  fetch?: typeof globalThis.fetch;
}

/** Create a scoped client. Use a control token for routine job management. */
export function createOkytes(options: Options) {
  const url = new URL(options.baseUrl);
  const local = ["127.0.0.1", "[::1]", "localhost"].includes(url.hostname);
  if (
    !["http:", "https:"].includes(url.protocol) ||
    (url.protocol === "http:" && !local) ||
    url.username ||
    url.password ||
    url.pathname !== "/" ||
    url.search ||
    url.hash ||
    !/^[a-f0-9-]{36}\.[a-f0-9]{64}$/.test(options.token)
  )
    throw new Error("Invalid daemon connection");
  const transport = options.fetch ?? globalThis.fetch;
  const safeFetch: typeof fetch = async (input, init) =>
    transport(input, { ...init, redirect: "error" });
  const api = createClient<paths>({
    baseUrl: options.baseUrl,
    headers: { Authorization: `Bearer ${options.token}` },
    fetch: safeFetch,
  });
  /** On Resync replace local state with GET /v1/jobs. Reconnect after a closed
   * stream; cancellation releases its reader and underlying request. */
  async function* events(
    signal?: AbortSignal,
  ): AsyncGenerator<components["schemas"]["Update"]> {
    const response = await safeFetch(new URL("v1/events", url), {
      headers: { Authorization: `Bearer ${options.token}` },
      signal,
    });
    if (!response.ok || !response.body)
      throw new Error(`Daemon stream failed (${response.status})`);
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let pending = "";
    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) return;
        pending += decoder.decode(value, { stream: true });
        if (pending.length > 131072)
          throw new Error("Daemon stream frame exceeds limit");
        let end;
        while ((end = pending.indexOf("\n\n")) >= 0) {
          const frame = pending.slice(0, end);
          pending = pending.slice(end + 2);
          const data = frame
            .split("\n")
            .filter((line) => line.startsWith("data:"))
            .map((line) => line.slice(5).trimStart())
            .join("\n");
          if (data) yield JSON.parse(data) as components["schemas"]["Update"];
        }
      }
    } finally {
      await reader.cancel().catch(() => {});
      reader.releaseLock();
    }
  }
  return { api, events };
}
