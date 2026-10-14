import { invoke } from "@tauri-apps/api/core";
import type { Action } from "../../../crates/core/bindings/Action";
import type { AddInput } from "../../../crates/core/bindings/AddInput";
import type { Job } from "../../../crates/core/bindings/Job";
import type { Settings } from "../../../crates/core/bindings/Settings";
import type { Snapshot } from "../../../crates/core/bindings/Snapshot";
export type { Action, AddInput, Job, Settings, Snapshot };
export const api = {
  inspectMedia: (url: string) =>
    invoke<import("../../../crates/core/bindings/MediaChoice").MediaChoice[]>(
      "inspect_media",
      { url },
    ),
  submit: (input: {
    url: string;
    destination: string | null;
    filename: string | null;
    sha256: string | null;
    conflict: string;
    credentials:
      | import("../../../crates/core/bindings/FileCredentials").FileCredentials
      | null;
  }) => invoke<string>("submit", { input }),
  authorizeFile: (
    id: string,
    credentials: import("../../../crates/core/bindings/FileCredentials").FileCredentials,
  ) => invoke<void>("authorize_file", { id, credentials }),
  inspectTorrent: (
    input: import("../../../crates/torrent-types/bindings/Input").Input,
  ) =>
    invoke<import("../../../crates/torrent-types/bindings/Metadata").Metadata>(
      "inspect_torrent",
      { input },
    ),
  addTorrent: (
    input: import("../../../crates/torrent-types/bindings/Input").Input,
  ) => invoke<string>("add_torrent", { input }),
  torrent: (id: string) =>
    invoke<import("../../../crates/torrent-types/bindings/Snapshot").Snapshot>(
      "torrent",
      { id },
    ),
  pauseAll: () => invoke<void>("pause_all"),
  list: (offset = 0) => invoke<Snapshot>("list", { offset }),
  get: (id: string) => invoke<Job>("get", { id }),
  add: (input: AddInput) => invoke<string[]>("add", { input }),
  control: (id: string, action: Action) =>
    invoke<void>("control", { id, action }),
  settings: () => invoke<Settings>("settings"),
  setSettings: (settings: Settings) =>
    invoke<void>("set_settings", { settings }),
};

export function category(name: string): string {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  if (["mp4", "mkv", "webm", "mov"].includes(ext)) return "Video";
  if (["mp3", "flac", "wav", "ogg"].includes(ext)) return "Audio";
  if (["png", "jpg", "jpeg", "gif", "webp", "svg"].includes(ext))
    return "Image";
  if (["zip", "rar", "7z", "gz", "tar"].includes(ext)) return "Archive";
  if (["pdf", "docx", "txt", "epub", "csv"].includes(ext)) return "Document";
  return "Other";
}
export function bytes(value: number): string {
  if (!Number.isFinite(value) || value < 0) return "—";
  const unit = Math.min(4, Math.floor(Math.log2(Math.max(1, value)) / 10));
  return `${(value / 1024 ** unit).toFixed(unit ? 1 : 0)} ${["B", "KiB", "MiB", "GiB", "TiB"][unit]}`;
}
export function parseUrls(text: string): string[] {
  if (text.length > 819200)
    throw new Error("Import is too large. Use at most 100 URLs.");
  const urls = text.split(/\s+/).filter(Boolean);
  if (urls.length > 100) throw new Error("Use at most 100 URLs per import.");
  return urls;
}
