// Chromium's Promise APIs and Firefox's browser APIs share this small boundary.
// No browser-specific behavior belongs in rules or capture policy.
export interface Event<T extends unknown[]> {
  addListener(listener: (...args: T) => unknown, ...options: unknown[]): void;
}
export interface DownloadItem {
  id: number;
  url: string;
  finalUrl?: string;
  filename: string;
  fileSize: number;
  totalBytes?: number;
  byExtensionId?: string;
  incognito?: boolean;
  state: string;
}
export interface Tab {
  id?: number;
  url?: string;
  incognito?: boolean;
}
export interface NativePort {
  postMessage(message: unknown): void;
  disconnect(): void;
  onMessage: Event<[unknown]>;
  onDisconnect: Event<[]>;
}
export interface Browser {
  runtime: {
    id: string;
    lastError?: { message?: string };
    getURL(path: string): string;
    connectNative(name: string): NativePort;
    sendMessage(message: unknown): Promise<unknown>;
    onMessage: Event<
      [
        unknown,
        { id?: string; url?: string; tab?: Tab },
        (response: unknown) => void,
      ]
    >;
    onInstalled: Event<[]>;
    openOptionsPage(): Promise<void>;
  };
  downloads: {
    onCreated: Event<[DownloadItem]>;
    cancel(id: number): Promise<void>;
    download(input: { url: string }): Promise<number>;
  };
  tabs: {
    query(input: { active: boolean; currentWindow: boolean }): Promise<Tab[]>;
  };
  storage: {
    local: {
      get(key: string): Promise<Record<string, unknown>>;
      set(value: Record<string, unknown>): Promise<void>;
    };
  };
  permissions: {
    request(input: {
      permissions?: string[];
      origins?: string[];
    }): Promise<boolean>;
    contains(input: {
      permissions?: string[];
      origins?: string[];
    }): Promise<boolean>;
    onRemoved: Event<[unknown]>;
    onAdded: Event<[unknown]>;
  };
  scripting: {
    executeScript<T>(input: {
      target: { tabId: number };
      func: () => T;
    }): Promise<{ result: T }[]>;
    registerContentScripts(
      input: { id: string; matches: string[]; js: string[]; runAt: string }[],
    ): Promise<void>;
    unregisterContentScripts(input: { ids: string[] }): Promise<void>;
  };
  cookies: {
    getAll(input: { url: string }): Promise<{ name: string; value: string }[]>;
  };
  webRequest: {
    onBeforeSendHeaders: Event<
      [
        {
          url: string;
          tabId: number;
          requestHeaders?: { name: string; value?: string }[];
        },
      ]
    >;
    onBeforeRequest: Event<[{ url: string; tabId: number; type: string }]>;
  };
  contextMenus: {
    create(input: { id: string; title: string; contexts: string[] }): void;
    removeAll(): Promise<void>;
    onClicked: Event<
      [{ menuItemId: string | number; linkUrl?: string; srcUrl?: string }, Tab]
    >;
  };
}
declare const browser: Browser | undefined;
declare const chrome: Browser;
const native = typeof browser !== "undefined" ? browser : chrome;
// Firefox exposes contextMenus as an alias for menus; manifests alone differ.
export const api: Browser = native;
