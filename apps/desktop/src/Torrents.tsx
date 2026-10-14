import { useEffect, useState } from "react";
import type { Input } from "../../../crates/torrent-types/bindings/Input";
import type { Options } from "../../../crates/torrent-types/bindings/Options";
import type { Metadata } from "../../../crates/torrent-types/bindings/Metadata";
import type { Snapshot } from "../../../crates/torrent-types/bindings/Snapshot";
import { api, bytes } from "./api";

export function TorrentForm({
  destination,
  done,
}: {
  destination: string;
  done: () => void;
}) {
  const [magnet, setMagnet] = useState("");
  const [metainfo, setMetainfo] = useState<string | null>(null);
  const [metadata, setMetadata] = useState<Metadata>();
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [options, setOptions] = useState<Options>({
    files: null,
    download_bps: null,
    upload_bps: null,
    seed_ratio: null,
    seed_seconds: 0,
    peers: 16,
    initial_peers: [],
    privacy: {
      dht: false,
      pex: false,
      local_discovery: false,
      bootstrap_nodes: null,
      listen: "127.0.0.1:0",
      bind_device: null,
      peer_proxy: null,
      encryption: "plaintext",
    },
  });
  const input = (): Input => ({
    magnet: magnet || null,
    metainfo,
    destination: destination || null,
    options,
  });
  async function preview() {
    setBusy(true);
    setError("");
    try {
      setMetadata(await api.inspectTorrent(input()));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        setBusy(true);
        setError("");
        void api
          .addTorrent(input())
          .then(done)
          .catch((e: unknown) => setError(String(e)))
          .finally(() => setBusy(false));
      }}
    >
      <p>
        Peers can see your IP address and the torrent’s info hash. Discovery is
        off by default. Uploading may occur while downloading; seeding stops at
        completion unless you choose otherwise.
      </p>
      {error && <p role="alert">{error}</p>}
      <label>
        Magnet link
        <input
          value={magnet}
          onChange={(e) => {
            setMagnet(e.target.value);
            setMetainfo(null);
            setMetadata(undefined);
          }}
        />
      </label>
      <label>
        Torrent file
        <input
          type="file"
          accept=".torrent"
          onChange={(e) => {
            const file = e.target.files?.[0];
            if (!file) return;
            if (file.size > 512 * 1024) {
              setError("Torrent files must be at most 512 KiB.");
              return;
            }
            void file.arrayBuffer().then((buffer) => {
              let text = "";
              for (const byte of new Uint8Array(buffer))
                text += String.fromCharCode(byte);
              setMetainfo(btoa(text));
              setMagnet("");
              setMetadata(undefined);
            });
          }}
        />
      </label>
      <fieldset>
        <legend>Discovery</legend>
        {(["dht", "pex", "local_discovery"] as const).map((key) => (
          <label key={key}>
            <input
              type="checkbox"
              checked={options.privacy[key]}
              onChange={(e) =>
                setOptions({
                  ...options,
                  privacy: { ...options.privacy, [key]: e.target.checked },
                })
              }
            />
            {key === "dht"
              ? "DHT (public network)"
              : key === "pex"
                ? "Peer exchange"
                : "Local peer discovery"}
          </label>
        ))}
      </fieldset>
      <label>
        Listen address
        <input
          value={options.privacy.listen}
          onChange={(e) =>
            setOptions({
              ...options,
              privacy: { ...options.privacy, listen: e.target.value },
            })
          }
        />
      </label>
      <label>
        SOCKS5 peer proxy
        <input
          placeholder="socks5://host:port"
          value={options.privacy.peer_proxy ?? ""}
          onChange={(e) =>
            setOptions({
              ...options,
              privacy: {
                ...options.privacy,
                peer_proxy: e.target.value || null,
              },
            })
          }
        />
      </label>
      <small>
        The proxy covers outgoing peer TCP connections. Trackers, DHT and local
        discovery use their own connections. Peer traffic is plaintext.
      </small>
      {(
        ["download_bps", "upload_bps", "seed_ratio", "seed_seconds"] as const
      ).map((key) => (
        <label key={key}>
          {
            {
              download_bps: "Download limit (bytes/s)",
              upload_bps: "Upload limit (bytes/s)",
              seed_ratio: "Maximum upload ratio",
              seed_seconds: "Seed seconds after completion",
            }[key]
          }
          <input
            type="number"
            min={key.endsWith("bps") ? 1 : 0}
            step={key === "seed_ratio" ? "0.01" : "1"}
            value={options[key] ?? ""}
            placeholder="Unlimited"
            onChange={(e) =>
              setOptions({
                ...options,
                [key]: e.target.value === "" ? null : Number(e.target.value),
              })
            }
          />
        </label>
      ))}
      <small>
        Zero seed seconds stops sharing at completion. An empty time or ratio
        adds no restriction. Ratio accounting reserves upload bytes before
        sending and includes uploads during the download.
      </small>
      <button type="button" disabled={busy} onClick={() => void preview()}>
        Inspect files
      </button>
      {metadata && (
        <fieldset>
          <legend>
            {metadata.name} · {metadata.pieces} pieces
          </legend>
          {metadata.files.map((file) => (
            <label key={file.index}>
              <input
                type="checkbox"
                checked={
                  options.files === null || options.files.includes(file.index)
                }
                onChange={(e) => {
                  const selected =
                    options.files ?? metadata.files.map((f) => f.index);
                  setOptions({
                    ...options,
                    files: e.target.checked
                      ? [...selected, file.index]
                      : selected.filter((i) => i !== file.index),
                  });
                }}
              />
              {file.path} · {bytes(file.bytes)}
            </label>
          ))}
        </fieldset>
      )}
      <button
        type="submit"
        disabled={busy || (!magnet && !metainfo) || options.files?.length === 0}
      >
        Add torrent
      </button>
    </form>
  );
}

export function TorrentDiagnostics({ id }: { id: string }) {
  const [snapshot, setSnapshot] = useState<Snapshot>();
  const [error, setError] = useState("");
  useEffect(() => {
    let live = true;
    const update = () => {
      void api
        .torrent(id)
        .then((value) => {
          if (live) {
            setSnapshot(value);
            setError("");
          }
        })
        .catch((e: unknown) => {
          if (live) setError(String(e));
        });
    };
    update();
    const timer = setInterval(update, 1000);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [id]);
  if (error) return <p role="alert">{error}</p>;
  if (!snapshot) return <p>Reading torrent diagnostics…</p>;
  const step = Math.max(1, Math.ceil(snapshot.verified.length / 128));
  return (
    <section aria-label="Torrent diagnostics">
      <p>
        {snapshot.activity} · Recorded uploads: {bytes(snapshot.uploaded_bytes)}
      </p>
      {snapshot.activity === "seeding" && (
        <button
          onClick={() => {
            void api
              .control(id, { kind: "Pause" })
              .catch((e: unknown) => setError(String(e)));
          }}
        >
          Stop sharing
        </button>
      )}
      <p>Info hash: {snapshot.metadata?.info_hash ?? "Resolving metadata"}</p>
      <p>Trackers: {snapshot.tracker_status}</p>
      <p>{snapshot.metadata?.trackers.join(" · ")}</p>
      <div className="piece-map" aria-label="Verified and durable pieces">
        {Array.from(
          { length: Math.ceil(snapshot.verified.length / step) },
          (_, group) => {
            const pieces = snapshot.verified.slice(
              group * step,
              (group + 1) * step,
            );
            return (
              <progress
                key={group}
                title={`Pieces ${group * step}–${group * step + pieces.length - 1}`}
                value={pieces.filter(Boolean).length}
                max={pieces.length}
              />
            );
          },
        )}
      </div>
      <details>
        <summary>Peer statistics</summary>
        <pre>{snapshot.peers}</pre>
      </details>
    </section>
  );
}
