import type { Browser, DownloadItem } from "./compat";
import type { Capture } from "./generated/Capture";
import type { Request } from "./generated/Request";
import type { Response } from "./generated/Response";
import { shouldCapture, type Rules, validUrl } from "./rules";

export const HOST = "org.okytes.bridge";
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
      reject(new Error("Okytes is unavailable"));
      return;
    }
    let settled = false;
    const finish = (response?: Response) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      port.disconnect();
      if (response) resolve(response);
      else
        reject(
          new Error("Okytes is unavailable; the browser download continues"),
        );
    };
    const timer = setTimeout(() => finish(), 9000);
    port.onDisconnect.addListener(() => {
      void api.runtime.lastError;
      finish();
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

export async function handoff(
  api: Browser,
  capture: Capture,
): Promise<Response> {
  if (!validUrl(capture.url.replace(/^(hls|dash)\+/, "")))
    throw new Error("Use an HTTP or HTTPS link without embedded credentials");
  return nativeRequest(api, {
    version: 1,
    id: crypto.randomUUID(),
    operation: { kind: "Capture", value: capture },
  });
}

// Do not pause first: a killed extension/worker must leave the original browser
// transfer able to finish. Cancel only after the app's durable acknowledgement.
export async function intercept(
  api: Browser,
  item: DownloadItem,
  rules: Rules,
  bypass: boolean,
  headers: (url: string) => Promise<[string, string][]>,
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
    if (!response.ok || !response.job_id) return false;
    await api.downloads.cancel(item.id);
    return true;
  } catch {
    return false;
  }
}
