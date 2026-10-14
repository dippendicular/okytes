import { api, type Tab } from "./compat";
import { defaults, normalizeRules, validUrl, type Rules } from "./rules";
import { handoff, intercept, nativeRequest } from "./capture";
import { ownPage } from "./messages";

// Sensitive state is bounded memory only. Worker suspension deliberately loses
// it; automatic capture then uses available cookies/headers or browser fallback.
const observed = new Map<string, { at: number; headers: [string, string][] }>();
const bypass = new Map<string, number>();
const candidates = new Map<number, string[]>();
async function rules(): Promise<Rules> {
  const stored = await api.storage.local.get("rules");
  try {
    return stored.rules ? normalizeRules(stored.rules) : defaults;
  } catch {
    return defaults;
  }
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

api.downloads.onCreated.addListener((item) => {
  void rules().then((value) =>
    intercept(
      api,
      item,
      value,
      (bypass.get(item.finalUrl || item.url) ?? 0) > Date.now(),
      sessionHeaders,
    ),
  );
});

// Optional webRequest has no blanket granted host access. The browser only
// delivers requests for sites for which the user granted optional permission.
let observing = false;
async function observe(): Promise<void> {
  if (
    observing ||
    !(await api.permissions.contains({ permissions: ["webRequest"] }))
  )
    return;
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
    api.webRequest.onBeforeSendHeaders.addListener(
      listener,
      { urls: ["http://*/*", "https://*/*"] },
      ["requestHeaders"],
    );
    api.webRequest.onBeforeRequest.addListener(
      (request) => {
        if (
          request.tabId < 0 ||
          !validUrl(request.url) ||
          !/\.(m3u8|mpd)(?:[?#]|$)/i.test(request.url)
        )
          return;
        if (candidates.size >= 32 && !candidates.has(request.tabId))
          candidates.delete(candidates.keys().next().value ?? -1);
        const entries = candidates.get(request.tabId) ?? [];
        if (!entries.includes(request.url) && entries.length < 32)
          entries.push(request.url);
        candidates.set(request.tabId, entries);
      },
      { urls: ["http://*/*", "https://*/*"] },
    );
    observing = true;
  } catch {
    /* Optional permission absent: explicit link capture remains available. */
  }
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
): Promise<{ accepted: number; failed: number }> {
  if (!urls.length || urls.length > 100 || urls.some((url) => !validUrl(url)))
    throw new Error("Use 1–100 HTTP/HTTPS links");
  let accepted = 0;
  let failed = 0;
  for (const url of urls) {
    try {
      const response = await handoff(api, {
        url,
        filename: null,
        sha256: null,
        headers: await sessionHeaders(url),
      });
      if (response.ok) accepted++;
      else failed++;
    } catch {
      failed++;
    }
  }
  return { accepted, failed };
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
  if (input.kind === "ping")
    return nativeRequest(api, {
      version: 1,
      id: crypto.randomUUID(),
      operation: { kind: "Ping" },
    });
  if (
    input.kind === "capture" &&
    Array.isArray(input.urls) &&
    input.urls.every((url) => typeof url === "string")
  )
    return captureUrls(input.urls);
  const active = (
    await api.tabs.query({ active: true, currentWindow: true })
  )[0];
  if (input.kind === "collect") return collect(active ?? {});
  if (input.kind === "media") {
    const links = await collect(active ?? {});
    return [
      ...new Set([
        ...links.filter((url) => /\.(m3u8|mpd)(?:[?#]|$)/i.test(url)),
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
    .catch(() =>
      reply({
        error:
          "Request failed. Check the app, site permission and link selection.",
      }),
    );
  return true;
});
async function menus(): Promise<void> {
  await api.contextMenus.removeAll();
  api.contextMenus.create({
    id: "capture-link",
    title: "Download with Okytes",
    contexts: ["link"],
  });
  api.contextMenus.create({
    id: "capture-media",
    title: "Download file with Okytes",
    contexts: ["video", "audio", "image"],
  });
  api.contextMenus.create({
    id: "collect-links",
    title: "Collect links with Okytes",
    contexts: ["page"],
  });
}
api.runtime.onInstalled.addListener(() => {
  void menus();
});
api.contextMenus.onClicked.addListener((info, tab) => {
  if (info.menuItemId === "collect-links") {
    void collect(tab)
      .then(captureUrls)
      .catch(() => {});
    return;
  }
  const url = info.linkUrl || info.srcUrl;
  if (!url || !validUrl(url) || tab.incognito) return;
  void captureUrls([url]).then((result) => {
    if (!result.accepted) return api.downloads.download({ url });
  });
});
api.permissions.onRemoved.addListener(() => {
  observed.clear();
  candidates.clear();
  bypass.clear();
});
