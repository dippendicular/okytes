import { invoke } from "@tauri-apps/api/core";
import type { Action } from "../../../crates/core/bindings/Action";
import type { AddInput } from "../../../crates/core/bindings/AddInput";
import type { Job } from "../../../crates/core/bindings/Job";
import type { Settings } from "../../../crates/core/bindings/Settings";
import type { Snapshot } from "../../../crates/core/bindings/Snapshot";
export type { Action, AddInput, Job, Settings, Snapshot };
export type PeerDevice = {
  peer_id: string | null;
  fullname: string;
  name: string;
  os: string;
  protocol: number;
  fingerprint: string;
  addresses: string[];
  port: number;
  trusted: boolean;
};
export type PeerRequest = {
  id: string;
  peer_fingerprint: string;
  direction: "inbound" | "outbound";
  manifest: {
    entries: Array<
      | { kind: "file"; path: string; size: number; sha256: string }
      | { kind: "directory"; path: string }
    >;
  };
  state: string;
  revision: number;
  bytes: number;
  total: number;
  files_completed: number;
  failure: string | null;
  destination: string | null;
};
export type PeerTrust = {
  id: string;
  fingerprint: string;
  revision: number;
  revoked: boolean;
};
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
  clipboard: () => invoke<string>("read_clipboard"),
  peerIdentity: () =>
    invoke<{ fingerprint: string; certificate: string }>("peer_identity"),
  peerDiscover: () => invoke<PeerDevice[]>("peer_discover"),
  peerDiscoverySetting: () => invoke<{ enabled: boolean }>("peer_discovery_setting"),
  setPeerDiscovery: (enabled: boolean) =>
    invoke<{ enabled: boolean }>("set_peer_discovery", { enabled }),
  peerRequests: () => invoke<PeerRequest[]>("peer_requests"),
  peerTrust: (input: {
    id: string;
    certificate: string;
    confirmed_fingerprint: string;
    expected_revision: number | null;
  }) => invoke("peer_trust", { input }),
  peerTrusts: () => invoke<PeerTrust[]>("peer_trusts"),
  peerRevoke: (id: string, revision: number) =>
    invoke<PeerTrust>("peer_revoke", { id, revision }),
  openPeerDestination: (path: string) => invoke<void>("open_peer_destination", { path }),
  peerSend: (input: { peer_id: string; address: string; paths: string[] }) =>
    invoke<string>("peer_send", { input }),
  peerDecide: (
    id: string,
    expected_revision: number,
    action: "accept" | "reject" | "pause" | "retry" | "cancel",
    destination: string | null = null,
  ) =>
    invoke<PeerRequest>("peer_decide", {
      id,
      input: { expected_revision, action, destination },
    }),
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
