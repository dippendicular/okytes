import { describe, it, expect, vi, afterEach } from "vitest";
import type { Browser, NativePort, DownloadItem } from "./compat";
import { intercept, nativeRequest } from "./capture";
import { defaults } from "./rules";
function fixture(mode: "ok" | "reject" | "disconnect" | "wrong-id") {
  let message: (value: unknown) => unknown = () => {};
  let disconnect: () => unknown = () => {};
  const port: NativePort = {
    onMessage: {
      addListener: (fn) => {
        message = fn;
      },
    },
    onDisconnect: {
      addListener: (fn) => {
        disconnect = fn;
      },
    },
    disconnect: vi.fn(),
    postMessage: vi.fn((request) => {
      queueMicrotask(() => {
        if (mode === "disconnect") disconnect();
        else
          message({
            version: 1,
            id: mode === "wrong-id" ? "wrong" : request.id,
            ok: mode === "ok",
            job_id: mode === "ok" ? crypto.randomUUID() : null,
            error: mode === "reject" ? "app_unavailable" : null,
          });
      });
    }),
  };
  const cancel = vi.fn().mockResolvedValue(undefined);
  const api = {
    runtime: { connectNative: () => port },
    downloads: { cancel },
  } as unknown as Browser;
  return { api, cancel, port };
}
const item: DownloadItem = {
  id: 42,
  url: "https://example.com/a.zip",
  filename: "a.zip",
  fileSize: 2048,
  state: "in_progress",
};
afterEach(() => vi.useRealTimers());
describe("durable handoff and browser fallback", () => {
  it("cancels only after a matching successful desktop acknowledgement", async () => {
    const f = fixture("ok");
    expect(
      await intercept(
        f.api,
        item,
        { ...defaults, enabled: true },
        false,
        async () => [["cookie", "session=test"]],
      ),
    ).toBe(true);
    expect(f.cancel).toHaveBeenCalledOnce();
    expect(f.cancel).toHaveBeenCalledWith(42);
    expect(f.port.postMessage).toHaveBeenCalledWith(
      expect.objectContaining({
        operation: expect.objectContaining({
          value: expect.objectContaining({
            headers: [["cookie", "session=test"]],
          }),
        }),
      }),
    );
  });
  it.each(["reject", "disconnect", "wrong-id"] as const)(
    "leaves the browser download running on %s",
    async (mode) => {
      const f = fixture(mode);
      expect(
        await intercept(
          f.api,
          item,
          { ...defaults, enabled: true },
          false,
          async () => [],
        ),
      ).toBe(false);
      expect(f.cancel).not.toHaveBeenCalled();
    },
  );
  it("never captures bypass, incognito, or extension-created downloads", async () => {
    const f = fixture("ok");
    for (const [download, bypass] of [
      [item, true],
      [{ ...item, incognito: true }, false],
      [{ ...item, byExtensionId: "another-extension" }, false],
    ] as const)
      expect(
        await intercept(
          f.api,
          download,
          { ...defaults, enabled: true },
          bypass,
          async () => [],
        ),
      ).toBe(false);
    expect(f.port.postMessage).not.toHaveBeenCalled();
  });
  it("bounds a silent native host with a timeout", async () => {
    vi.useFakeTimers();
    const f = fixture("ok");
    f.port.postMessage = () => {};
    const promise = nativeRequest(f.api, {
      version: 1,
      id: crypto.randomUUID(),
      operation: { kind: "Ping" },
    });
    const assertion = expect(promise).rejects.toThrow(
      "browser download continues",
    );
    await vi.advanceTimersByTimeAsync(9000);
    await assertion;
    expect(f.port.disconnect).toHaveBeenCalledOnce();
  });
});
