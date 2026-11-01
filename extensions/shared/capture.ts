import type { Browser, DownloadItem } from "./compat";
import type { Capture } from "./generated/Capture";
import type { Request } from "./generated/Request";
import type { Response } from "./generated/Response";
import { shouldCapture, type Rules, validUrl } from "./rules";

export const HOST = "org.okytes.bridge";
// Stable codes from the extension, native host and desktop, mapped to actions
// the user can take. Messages never include URLs, cookies or headers.
const reasons: Record<string, string> = {
  host_missing:
    "Okytes browser host is not installed. Launch the Okytes desktop app once to register it.",
  host_exited:
    "The Okytes browser host closed the connection. Open Okytes and try again; if this persists, this extension's ID may not be registered (see the extension README).",
  timeout:
    "Okytes did not answer in time; the browser download continues.",
  app_unavailable: "Okytes is not running. Open the Okytes desktop app.",
  capture_rejected:
    "Okytes could not create this download. Check the destination folder in Okytes.",
  rate_limited: "Too many requests. Wait a minute and try again.",
  invalid_request: "Okytes rejected the request from this extension version.",
  invalid_url: "Only HTTP and HTTPS links without embedded credentials are supported.",
  invalid_headers: "This site's session data cannot be handed to Okytes.",
  invalid_filename: "The suggested filename is not valid.",
};
export function describe(code: string | null | undefined): string {
  return (code && reasons[code]) || "Okytes could not accept the download.";
}
export class BridgeError extends Error {
  constructor(readonly code: string) {
    super(describe(code));
  }
}
// Explicit media capture preserves the original HTTP origin for session handoff.
export function mediaSource(url: string): string {
  if (!validUrl(url)) throw new Error("Use an HTTP/HTTPS manifest URL");
  const path = new URL(url).pathname.toLowerCase();
  if (path.endsWith(".m3u8")) return `hls+${url}`;
  if (path.endsWith(".mpd")) return `dash+${url}`;
  throw new Error("Choose a .m3u8 or .mpd manifest");
}
// Each request has a bounded lifetime; the MV3 worker can reconnect on the next
// user/browser event. The extension never receives the desktop handshake token.
export function nativeRequest(
  api: Browser,
  request: Request,
): Promise<Response> {
  return new Promise((resolve, reject) => {
    let port: ReturnType<Browser["runtime"]["connectNative"]>;
    try {
      port = api.runtime.connectNative(HOST);
    } catch {
      reject(new BridgeError("host_missing"));
      return;
    }
    let settled = false;
    const finish = (response?: Response, code = "host_exited") => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      port.disconnect();
      if (response) resolve(response);
      else reject(new BridgeError(code));
    };
    const timer = setTimeout(() => finish(undefined, "timeout"), 9000);
    port.onDisconnect.addListener(() => {
      // Chromium and Firefox report a missing or disallowed host only here.
      const detail = api.runtime.lastError?.message ?? "";
      finish(
        undefined,
        /not found|no such native|not registered|forbidden|access to the specified/i.test(
          detail,
        )
          ? "host_missing"
          : "host_exited",
      );
    });
    port.onMessage.addListener((message) => {
      if (!message || typeof message !== "object") return;
      const response = message as Response;
      if (
        response.version !== 1 ||
        response.id !== request.id ||
        typeof response.ok !== "boolean"
      ) {
        finish();
        return;
      }
      finish(response);
    });
    try {
      port.postMessage(request);
    } catch {
      finish();
    }
  });
}

// The desktop uses the request UUID as the job UUID, so repeating the same
// request after an ambiguous failure cannot create a second job. Retrying
// prevents a slow-but-successful handoff from also continuing in the browser.
export async function handoff(
  api: Browser,
  capture: Capture,
): Promise<Response> {
  if (!validUrl(capture.url.replace(/^(hls|dash)\+/, "")))
    throw new BridgeError("invalid_url");
  const request: Request = {
    version: 1,
    id: crypto.randomUUID(),
    operation: { kind: "Capture", value: capture },
  };
  try {
    return await nativeRequest(api, request);
  } catch (error) {
    if (!(error instanceof BridgeError) || error.code === "host_missing")
      throw error;
    return nativeRequest(api, request);
  }
}

// Do not pause first: a killed extension/worker must leave the original browser
// transfer able to finish. Cancel only after the app's durable acknowledgement.
export async function intercept(
  api: Browser,
  item: DownloadItem,
  rules: Rules,
  bypass: boolean,
  headers: (url: string) => Promise<[string, string][]>,
  report?: (error: string | null) => void,
): Promise<boolean> {
  const url = item.finalUrl || item.url;
  if (
    item.byExtensionId ||
    item.incognito ||
    item.state !== "in_progress" ||
    !shouldCapture(rules, {
      url,
      filename: item.filename,
      size: item.totalBytes ?? item.fileSize,
      bypass,
    })
  )
    return false;
  try {
    const response = await handoff(api, {
      url,
      filename: null,
      sha256: null,
      headers: await headers(url),
    });
    if (!response.ok || !response.job_id) {
      report?.(response.error ?? "capture_rejected");
      return false;
    }
    report?.(null);
    await api.downloads.cancel(item.id).catch(() => {
      // Already finished in the browser; Okytes still owns its own copy.
    });
    return true;
  } catch (error) {
    report?.(error instanceof BridgeError ? error.code : "host_exited");
    return false;
  }
}
