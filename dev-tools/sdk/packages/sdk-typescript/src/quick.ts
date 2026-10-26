import type { components } from "./schema.js";

type Snapshot = components["schemas"]["Snapshot"];
type AddInput = components["schemas"]["AddInput"];
type Action = components["schemas"]["Action"];
export type QuickPeerRequest = {
  id: string;
  peer_fingerprint: string;
  direction: "inbound" | "outbound";
  state: string;
  revision: number;
  bytes: number;
  total: number;
  files_completed: number;
  failure: string | null;
  destination: string | null;
  manifest: { entries: Array<{ kind: string; path: string; size?: number }> };
};

/** Same-origin browser client. Session credentials stay in HttpOnly cookies;
 * the anti-CSRF value lives only in this client closure. Never persist it. */
export function createQuick(transport: typeof fetch = globalThis.fetch) {
  let csrf = "";
  async function request<T>(
    path: string,
    method = "GET",
    body?: unknown,
  ): Promise<T> {
    const response = await transport(path, {
      method,
      credentials: "same-origin",
      redirect: "error",
      cache: "no-store",
      headers: {
        "X-Okytes-Quick": "1",
        ...(csrf ? { "X-Okytes-CSRF": csrf } : {}),
        ...(body === undefined ? {} : { "Content-Type": "application/json" }),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (!response.ok) {
      if (response.status === 401) csrf = "";
      throw new Error(
        response.status === 401
          ? "Your session has expired. Pair this browser again."
          : response.status === 409
            ? "This URL is already in your queue. Confirm duplicates to add another copy."
            : `The service could not complete this request (${response.status}).`,
      );
    }
    return response.json() as Promise<T>;
  }
  async function session(code?: string) {
    const value = await request<{ csrf: string }>(
      "/quick/session",
      code ? "POST" : "GET",
      code ? { code } : undefined,
    );
    csrf = value.csrf;
  }
  return {
    session,
    async logout() {
      await request("/quick/logout", "POST", {});
      csrf = "";
    },
    list: (offset = 0) => request<Snapshot>(`/v1/jobs?offset=${offset}`),
    add: (input: AddInput) => request<string[]>("/v1/jobs", "POST", input),
    control: (id: string, action: Action) =>
      request<void>(
        `/v1/jobs/${encodeURIComponent(id)}/control`,
        "POST",
        action,
      ),
    peerRequests: () => request<QuickPeerRequest[]>("/v1/peers/requests"),
    peerDecide: (
      id: string,
      expected_revision: number,
      action: "accept" | "reject" | "pause" | "retry" | "cancel",
    ) =>
      request<QuickPeerRequest>(
        `/v1/peers/requests/${encodeURIComponent(id)}/decision`,
        "POST",
        { expected_revision, action, destination: null },
      ),
    async events(changed: () => void, signal: AbortSignal) {
      const response = await transport("/v1/events", {
        credentials: "same-origin",
        redirect: "error",
        cache: "no-store",
        signal,
        headers: { "X-Okytes-Quick": "1", "X-Okytes-CSRF": csrf },
      });
      if (!response.ok || !response.body)
        throw new Error("Event connection unavailable.");
      const reader = response.body.getReader();
      const decoder = new TextDecoder();
      let pending = "";
      try {
        for (;;) {
          const part = await reader.read();
          if (part.done) return;
          pending += decoder.decode(part.value, { stream: true });
          if (pending.length > 131072)
            throw new Error("Event frame exceeds limit.");
          let end;
          while ((end = pending.indexOf("\n\n")) >= 0) {
            const frame = pending.slice(0, end);
            pending = pending.slice(end + 2);
            if (frame.split("\n").some((line) => line.startsWith("data:")))
              changed();
          }
        }
      } finally {
        await reader.cancel().catch(() => {});
        reader.releaseLock();
      }
    },
  };
}
