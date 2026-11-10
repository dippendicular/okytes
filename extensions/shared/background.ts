import { api, type DownloadItem, type Tab } from "./compat";
import { defaults, normalizeRules, validUrl, type Rules } from "./rules";
import {
  BridgeError,
  describe,
  handoff,
  handoffTorrent,
  handoffTorrentUrl,
  intercept,
  nativeRequest,
  mediaSource,
} from "./capture";
import { ownPage } from "./messages";

// Sensitive state is bounded memory only. Worker suspension deliberately loses
// it; automatic capture then uses available cookies/headers or browser fallback.
const observed = new Map<string, { at: number; headers: [string, string][] }>();
const bypass = new Map<string, number>();
const candidates = new Map<number, string[]>();
const announcedMedia = new Map<string, number>();
// Duplicate suppression: URLs being handed off now, and URLs Okytes accepted
// in the last few seconds through any path (context menu, popup, automatic).
const inflight = new Set<string>();
const accepted = new Map<string, number>();
// Chromium assigns filenames after onCreated; type rules wait for that event.
const awaitingName = new Map<number, DownloadItem>();
const menus = api.contextMenus ?? api.menus;

function remember(url: string): void {
  if (accepted.size >= 64) accepted.delete(accepted.keys().next().value ?? "");
  accepted.set(url, Date.now() + 10000);
}
// Diagnostics name the host only; full URLs can carry signed tokens.
function host(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return "invalid URL";
  }
}
let badgeTimer: ReturnType<typeof setTimeout> | undefined;
function feedback(error: string | null, detail = ""): void {
  const message = error ? describe(error) : "Sent to Okytes";
  if (error) console.warn(`[okytes] ${error}${detail ? ` (${detail})` : ""}`);
  const action = api.action;
  if (!action) return;
  clearTimeout(badgeTimer);
  void Promise.all([
    action.setBadgeText({ text: error ? "!" : "✓" }),
    action.setBadgeBackgroundColor({ color: error ? "#c62828" : "#2e7d32" }),
    action.setTitle({ title: `Okytes — ${message}` }),
  ]).catch(() => {});
  badgeTimer = setTimeout(() => {
    void action.setBadgeText({ text: "" }).catch(() => {});
    void action.setTitle({ title: "Okytes" }).catch(() => {});
  }, 8000);
}
async function rules(): Promise<Rules> {
  const stored = await api.storage.local.get("rules");
  try {
    return stored.rules ? normalizeRules(stored.rules) : defaults;
  } catch {
    return defaults;
  }
}
// Earlier releases persisted `enabled: false` as their initial value. Migrate
// that one-time default, while preserving any later user choice.
async function initializeDefaults(): Promise<void> {
  const stored = await api.storage.local.get("rules");
  const marker = await api.storage.local.get("autoCaptureDefaultsVersion");
  if (marker.autoCaptureDefaultsVersion === 1) return;
  const value = stored.rules ? normalizeRules(stored.rules) : defaults;
  await api.storage.local.set({
    rules: { ...value, enabled: true },
    autoCaptureDefaultsVersion: 1,
  });
}
function originPattern(url: string): string {
  return `${new URL(url).origin}/*`;
}
async function sessionHeaders(url: string): Promise<[string, string][]> {
  const origin = originPattern(url);
  const result: [string, string][] = [];
  if (!(await api.permissions.contains({ origins: [origin] }))) return result;
  const request = observed.get(url);
  if (request && Date.now() - request.at < 30000)
    result.push(...request.headers);
  if (await api.permissions.contains({ permissions: ["cookies"] })) {
    const cookies = await api.cookies.getAll({ url });
    const value = cookies.map((c) => `${c.name}=${c.value}`).join("; ");
    if (
      value &&
      value.length <= 16384 &&
      !result.some(([name]) => name === "cookie")
    )
      result.push(["cookie", value]);
  }
  return result;
}

async function automatic(item: DownloadItem): Promise<void> {
  const url = item.finalUrl || item.url;
  if (item.byExtensionId || inflight.has(url)) return;
  if ((accepted.get(url) ?? 0) > Date.now()) {
    // Okytes already owns this URL from an explicit capture moments ago.
    await api.downloads.cancel(item.id).catch(() => {});
    return;
  }
  const value = await rules();
  if (!value.enabled) return;
  // `.torrent` is metainfo, not an ordinary payload. The desktop fetches it
  // with the current origin session and imports it into the native queue.
  if (/\.torrent(?:[?#]|$)/i.test(url)) {
    inflight.add(url);
    try {
      const response = await handoffTorrentUrl(api, url, await sessionHeaders(url));
      if (!response.ok || !response.job_id) {
        feedback(response.error ?? "capture_rejected", "torrent");
        return;
      }
      remember(url);
      feedback(null, "torrent");
      await api.downloads.cancel(item.id).catch(() => {});
    } catch (error) {
      feedback(error instanceof BridgeError ? error.code : "host_exited", "torrent");
    } finally {
      inflight.delete(url);
    }
    return;
  }
  if (!item.filename && value.types.length) {
    if (awaitingName.size >= 32)
      awaitingName.delete(awaitingName.keys().next().value ?? -1);
    awaitingName.set(item.id, item);
    setTimeout(() => awaitingName.delete(item.id), 15000);
    return;
  }
  inflight.add(url);
  try {
    const captured = await intercept(
      api,
      item,
      value,
      (bypass.get(url) ?? 0) > Date.now(),
      sessionHeaders,
      (error) => feedback(error, host(url)),
    );
    if (captured) remember(url);
  } finally {
    inflight.delete(url);
  }
}
function isMediaRequest(url: string): boolean {
  try {
    const value = new URL(url);
    // Resolver adapters, rather than ephemeral player requests, own page URL
    // resolution, track selection and expiry refresh. The browser only offers
    // an explicit HLS/DASH manifest to the built-in media transport.
    return /\.(m3u8|mpd)(?:$|\.)/i.test(value.pathname);
  } catch {
    return false;
  }
}
function announceMedia(url: string): void {
  if ((announcedMedia.get(url) ?? 0) > Date.now()) return;
  announcedMedia.set(url, Date.now() + 30000);
  const action = api.action;
  if (action)
    void Promise.all([
      action.setBadgeText({ text: "▶" }),
      action.setBadgeBackgroundColor({ color: "#1565c0" }),
      action.setTitle({ title: "Okytes — media detected; click to review" }),
    ]).catch(() => {});
  setTimeout(() => announcedMedia.delete(url), 30000);
}
api.downloads.onCreated.addListener((item) => {
  void automatic(item).catch(() => feedback("host_exited"));
});
api.downloads.onChanged.addListener((delta) => {
  const item = awaitingName.get(delta.id);
  if (!item) return;
  if (delta.state?.current && delta.state.current !== "in_progress") {
    awaitingName.delete(delta.id);
    return;
  }
  const filename = delta.filename?.current;
  if (!filename) return;
  awaitingName.delete(delta.id);
  void automatic({ ...item, filename }).catch(() => feedback("host_exited"));
});

let observing = false;
async function observe(): Promise<void> {
  if (observing) return;
  try {
    const listener = (request: {
      url: string;
      tabId: number;
      requestHeaders?: { name: string; value?: string }[];
    }) => {
      if (!validUrl(request.url)) return;
      const headers: [string, string][] = (request.requestHeaders ?? [])
        .filter(
          (h) =>
            ["authorization", "referer", "user-agent", "accept"].includes(
              h.name.toLowerCase(),
            ) &&
            h.value &&
            h.value.length <= 16384 &&
            /^[\x20-\x7e]*$/.test(h.value),
        )
        .slice(0, 16)
        .map((h) => [h.name.toLowerCase(), h.value ?? ""]);
      if (observed.size >= 64)
        observed.delete(observed.keys().next().value ?? "");
      const record = { at: Date.now(), headers };
      observed.set(request.url, record);
      setTimeout(() => {
        if (observed.get(request.url) === record) observed.delete(request.url);
      }, 30000);
    };
    try {
      // Chromium hides Referer and Cookie unless extraHeaders is requested.
      api.webRequest.onBeforeSendHeaders.addListener(
        listener,
        { urls: ["http://*/*", "https://*/*"] },
        ["requestHeaders", "extraHeaders"],
      );
    } catch {
      // Firefox rejects the Chromium-only option and exposes these headers anyway.
      api.webRequest.onBeforeSendHeaders.addListener(
        listener,
        { urls: ["http://*/*", "https://*/*"] },
        ["requestHeaders"],
      );
    }
    api.webRequest.onBeforeRequest.addListener(
      (request) => {
        if (
          request.tabId < 0 ||
          !validUrl(request.url) ||
          !isMediaRequest(request.url)
        )
          return;
        if (candidates.size >= 32 && !candidates.has(request.tabId))
          candidates.delete(candidates.keys().next().value ?? -1);
        const entries = candidates.get(request.tabId) ?? [];
        if (!entries.includes(request.url) && entries.length < 32)
          entries.push(request.url);
        candidates.set(request.tabId, entries);
        announceMedia(request.url);
      },
      { urls: ["http://*/*", "https://*/*"] },
    );
    observing = true;
  } catch { /* Explicit link capture remains available if observation fails. */ }
}
void observe();
api.permissions.onAdded.addListener(() => {
  void observe();
});

async function collect(tab: Tab): Promise<string[]> {
  if (tab.id === undefined || tab.incognito)
    throw new Error("Choose a normal HTTP/HTTPS tab");
  const result = await api.scripting.executeScript({
    target: { tabId: tab.id },
    func: () =>
      Array.from(document.querySelectorAll<HTMLAnchorElement>("a[href]"))
        .slice(0, 1000)
        .map((a) => a.href),
  });
  return [
    ...new Set(result.flatMap((value) => value.result).filter(validUrl)),
  ].slice(0, 100);
}
async function captureUrls(
  urls: string[],
  media = false,
): Promise<{ accepted: number; failed: number; errors: string[] }> {
  if (!urls.length || urls.length > 100 || urls.some((url) => !validUrl(url)))
    throw new Error("Use 1–100 HTTP/HTTPS links");
  let ok = 0;
  const codes = new Set<string>();
  for (const url of urls) {
    try {
      const response = await handoff(api, {
        // Manifest URLs use the media pipeline; signed direct media endpoints
        // (including YouTube's videoplayback requests) are regular HTTP jobs.
        url:
          media && /\.(m3u8|mpd)(?:[?#]|$)/i.test(url)
            ? mediaSource(url)
            : url,
        filename: null,
        sha256: null,
        headers: await sessionHeaders(url),
      });
      if (response.ok) {
        ok++;
        remember(url);
      } else codes.add(response.error ?? "capture_rejected");
    } catch (error) {
      const code = error instanceof BridgeError ? error.code : "invalid_url";
      codes.add(code);
      // Without a host every remaining link fails the same way.
      if (code === "host_missing") break;
    }
  }
  const errors = [...codes];
  feedback(ok ? null : (errors[0] ?? "capture_rejected"), `${urls.length} link(s)`);
  return { accepted: ok, failed: urls.length - ok, errors: errors.map(describe) };
}
async function captureTorrent(magnet: string): Promise<boolean> {
  try {
    const response = await handoffTorrent(api, magnet);
    if (!response.ok || !response.job_id) {
      feedback(response.error ?? "capture_rejected", "torrent");
      return false;
    }
    feedback(null, "torrent");
    return true;
  } catch (error) {
    feedback(error instanceof BridgeError ? error.code : "host_exited", "torrent");
    return false;
  }
}
async function captureTorrentUrl(source: string): Promise<boolean> {
  try {
    const response = await handoffTorrentUrl(api, source, await sessionHeaders(source));
    if (!response.ok || !response.job_id) {
      feedback(response.error ?? "capture_rejected", "torrent");
      return false;
    }
    feedback(null, "torrent");
    return true;
  } catch (error) {
    feedback(error instanceof BridgeError ? error.code : "host_exited", "torrent");
    return false;
  }
}
async function message(value: unknown, tab?: Tab): Promise<unknown> {
  if (!value || typeof value !== "object") throw new Error("Invalid command");
  const input = value as Record<string, unknown>;
  if (
    input.kind === "bypass" &&
    tab?.id !== undefined &&
    typeof input.url === "string" &&
    validUrl(input.url)
  ) {
    if (bypass.size >= 64) bypass.delete(bypass.keys().next().value ?? "");
    bypass.set(input.url, Date.now() + 5000);
    return { ok: true };
  }
  // Content scripts may only mark a bypass for their own tab. Queue management
  // commands must originate from the extension's own pages, not a page script.
  if (tab) throw new Error("Command requires the extension popup");
  if (input.kind === "ping") {
    const response = await nativeRequest(api, {
      version: 1,
      id: crypto.randomUUID(),
      operation: { kind: "Ping" },
    });
    return response.ok
      ? response
      : { ...response, error: describe(response.error) };
  }
  if (
    (input.kind === "capture" || input.kind === "capture_media") &&
    Array.isArray(input.urls) &&
    input.urls.every((url) => typeof url === "string")
  )
    return captureUrls(input.urls, input.kind === "capture_media");
  const active = (
    await api.tabs.query({ active: true, currentWindow: true })
  )[0];
  if (input.kind === "collect") return collect(active ?? {});
  if (input.kind === "media") {
    const links = await collect(active ?? {});
    return [
      ...new Set([
        ...links.filter(isMediaRequest),
        ...(candidates.get(active?.id ?? -1) ?? []),
      ]),
    ].slice(0, 32);
  }
  throw new Error("Unknown command");
}
api.runtime.onMessage.addListener((value, sender, reply) => {
  if (sender.id !== api.runtime.id) return false;
  const privileged = ownPage(sender, api.runtime.id, api.runtime.getURL(""));
  void message(value, privileged ? undefined : sender.tab)
    .then(reply)
    .catch((error: unknown) =>
      reply({
        // Our own errors carry actionable text without URLs or credentials.
        error:
          error instanceof Error && error.message
            ? error.message
            : "Request failed. Check the app, site permission and link selection.",
      }),
    );
  return true;
});
async function register(): Promise<void> {
  await menus.removeAll();
  menus.create({
    id: "capture-link",
    title: "Download with Okytes",
    contexts: ["link"],
  });
  menus.create({
    id: "capture-media",
    title: "Download file with Okytes",
    contexts: ["video", "audio", "image"],
  });
  menus.create({
    id: "collect-links",
    title: "Collect links with Okytes",
    contexts: ["page"],
  });
}
// Registered on install and on every browser start: Firefox event pages do not
// reliably keep menus across restarts, and re-creating them is idempotent.
api.runtime.onInstalled.addListener(() => {
  void initializeDefaults().catch(() => {});
  void register();
});
api.runtime.onStartup.addListener(() => {
  void initializeDefaults().catch(() => {});
  void register();
});
void initializeDefaults().catch(() => {});
menus.onClicked.addListener((info, tab) => {
  if (info.menuItemId === "collect-links") {
    void collect(tab ?? {})
      .then((urls) => captureUrls(urls))
      .catch((error: unknown) =>
        feedback(error instanceof BridgeError ? error.code : "invalid_url"),
      );
    return;
  }
  const url = info.linkUrl || info.srcUrl;
  if (!url) {
    feedback("invalid_url");
    return;
  }
  if (url.toLowerCase().startsWith("magnet:")) {
    void captureTorrent(url);
    return;
  }
  if (!validUrl(url)) {
    feedback("invalid_url");
    return;
  }
  // Private windows stay in the browser, matching automatic capture.
  if (tab?.incognito) return;
  if (/\.torrent(?:[?#]|$)/i.test(url)) {
    void captureTorrentUrl(url).then((accepted) => {
      if (!accepted) return api.downloads.download({ url });
    });
    return;
  }
  void captureUrls([url]).then((result) => {
    // Explicit fallback: the user asked for this file, so the browser fetches
    // it and the toolbar badge explains why Okytes did not.
    if (!result.accepted) return api.downloads.download({ url });
  });
});
api.permissions.onRemoved.addListener(() => {
  observed.clear();
  candidates.clear();
  bypass.clear();
});
