import type { components } from "./schema.js";

export type TokenProvider = string | (() => string | Promise<string>);
export interface ClientOptions {
  endpoint: string;
  token: TokenProvider;
  fetch?: typeof globalThis.fetch;
  timeoutMs?: number;
  maxRetries?: number;
}

type ApiError = components["schemas"]["ApiError"];
type Snapshot = components["schemas"]["Snapshot"];
type Job = components["schemas"]["Job"];
type Action = components["schemas"]["Action"];
type Capabilities = components["schemas"]["Capabilities"];
type Update = components["schemas"]["Update"];

export class OkytesError extends Error {
  constructor(
    public readonly code: string,
    public readonly category: string,
    public readonly retryable: boolean,
    message: string,
    public readonly status?: number,
  ) {
    super(message);
    this.name = "OkytesError";
  }
}

export class OkytesClient {
  private readonly endpoint: URL;
  private readonly transport: typeof fetch;
  private readonly timeoutMs: number;
  private readonly maxRetries: number;

  constructor(private readonly options: ClientOptions) {
    this.endpoint = new URL(options.endpoint);
    const local = ["localhost", "127.0.0.1", "[::1]"].includes(this.endpoint.hostname);
    if (
      !["http:", "https:"].includes(this.endpoint.protocol) ||
      (this.endpoint.protocol === "http:" && !local) ||
      this.endpoint.username ||
      this.endpoint.password ||
      this.endpoint.pathname !== "/" ||
      this.endpoint.search ||
      this.endpoint.hash
    ) throw new OkytesError("invalid_endpoint", "validation", false, "Use loopback HTTP or verified HTTPS.");
    this.transport = options.fetch ?? globalThis.fetch;
    this.timeoutMs = options.timeoutMs ?? 15_000;
    this.maxRetries = options.maxRetries ?? 3;
  }

  private async token(): Promise<string> {
    const value = typeof this.options.token === "function" ? await this.options.token() : this.options.token;
    if (!/^[a-f0-9-]{36}\.[a-f0-9]{64}$/.test(value))
      throw new OkytesError("invalid_token", "authentication", false, "The token provider returned an invalid credential.");
    return value;
  }

  async request<T>(path: string, init: RequestInit = {}, idempotencyKey?: string): Promise<T> {
    for (let attempt = 0;; attempt += 1) {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), this.timeoutMs);
    try {
      const response = await this.transport(new URL(path, this.endpoint), {
        ...init,
        redirect: "error",
        signal: init.signal ?? controller.signal,
        headers: {
          Authorization: `Bearer ${await this.token()}`,
          ...(init.body ? { "Content-Type": "application/json" } : {}),
          ...init.headers,
          ...(idempotencyKey ? { "Idempotency-Key": idempotencyKey } : {}),
        },
      });
      if (!response.ok) {
        const body = (await response.json().catch(() => null)) as ApiError | null;
        const error = new OkytesError(
          body?.error ?? "request_failed",
          body?.category ?? "internal",
          body?.retryable ?? response.status >= 500,
          body?.message ?? `Okytes request failed (${response.status}).`,
          response.status,
        );
        if (!error.retryable || attempt >= this.maxRetries || init.signal?.aborted) throw error;
        await new Promise((resolve) => setTimeout(resolve, Math.min(250 * 2 ** attempt, 10_000) * (0.5 + Math.random())));
        continue;
      }
      if (response.status === 204 || response.headers.get("content-length") === "0") return undefined as T;
      return (await response.json()) as T;
    } finally {
      clearTimeout(timeout);
    }
    }
  }

  capabilities = () => this.request<Capabilities>("v1/capabilities");
  jobs = (offset = 0) => this.request<Snapshot>(`v1/jobs?offset=${offset}`);
  job = (id: string) => this.request<Job>(`v1/jobs/${encodeURIComponent(id)}`);
  add = (input: components["schemas"]["AddInput"]) =>
    this.request<string[]>("v1/jobs", { method: "POST", body: JSON.stringify(input) }, crypto.randomUUID());
  download = (input: components["schemas"]["Submit"], idempotencyKey = crypto.randomUUID()) =>
    this.request<string>("v1/submit", { method: "POST", body: JSON.stringify(input) }, idempotencyKey);
  inspectMedia = (url: string) => this.request<components["schemas"]["MediaChoice"][]>("v1/media/inspect", { method: "POST", body: JSON.stringify(url) });
  inspectTorrent = (input: components["schemas"]["TorrentInput"]) => this.request<components["schemas"]["TorrentMetadata"]>("v1/torrents/inspect", { method: "POST", body: JSON.stringify(input) });
  addTorrent = (input: components["schemas"]["TorrentInput"], idempotencyKey = crypto.randomUUID()) => this.request<string>("v1/torrents", { method: "POST", body: JSON.stringify(input) }, idempotencyKey);
  torrent = (id: string) => this.request<components["schemas"]["TorrentSnapshot"]>(`v1/torrents/${encodeURIComponent(id)}`);
  authorizeFile = (id: string, credentials: components["schemas"]["FileCredentials"]) => this.request<void>(`v1/jobs/${encodeURIComponent(id)}/credentials`, { method: "POST", body: JSON.stringify(credentials) });
  sendToPeer = (input: components["schemas"]["PeerSend"]) => this.request<string>("v1/peers/send", { method: "POST", body: JSON.stringify(input) }, crypto.randomUUID());
  control = (id: string, action: Action) =>
    this.request<void>(`v1/jobs/${encodeURIComponent(id)}/control`, { method: "POST", body: JSON.stringify(action) });

  async *events(signal?: AbortSignal): AsyncGenerator<Update> {
    let delay = 250;
    while (!signal?.aborted) {
      try {
        const response = await this.transport(new URL("v1/events", this.endpoint), {
          headers: { Authorization: `Bearer ${await this.token()}` },
          redirect: "error",
          signal,
        });
        if (!response.ok || !response.body) throw new Error("stream unavailable");
        const reader = response.body.getReader();
        const decoder = new TextDecoder();
        let pending = "";
        try {
          for (;;) {
            const part = await reader.read();
            if (part.done) break;
            pending += decoder.decode(part.value, { stream: true });
            if (pending.length > 131_072) throw new Error("event frame exceeds limit");
            let end: number;
            while ((end = pending.indexOf("\n\n")) >= 0) {
              const frame = pending.slice(0, end);
              pending = pending.slice(end + 2);
              const data = frame.split("\n").filter((line) => line.startsWith("data:")).map((line) => line.slice(5).trimStart()).join("\n");
              if (data) yield JSON.parse(data) as Update;
            }
          }
        } finally {
          await reader.cancel().catch(() => undefined);
          reader.releaseLock();
        }
        delay = 250;
      } catch (error) {
        if (signal?.aborted) return;
        yield { kind: "Resync" } as Update;
        await new Promise((resolve) => setTimeout(resolve, delay + Math.random() * delay));
        delay = Math.min(delay * 2, 10_000);
      }
    }
  }
}
