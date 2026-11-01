import { useEffect, useState } from "react";
import { api } from "./api";
import type { MediaChoice } from "../../../crates/core/bindings/MediaChoice";

export function FileTransferForm({
  destination,
  done,
  id,
  protocol,
}: {
  destination: string;
  done: () => void;
  id?: string;
  protocol?: string;
}) {
  const [url, setUrl] = useState("");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [fingerprint, setFingerprint] = useState("");
  const [confirmed, setConfirmed] = useState(false);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const sftp = protocol === "sftp" || url.startsWith("sftp:");
  return (
    <form
      className="settings"
      onSubmit={(e) => {
        e.preventDefault();
        setBusy(true);
        setError("");
        const credentials = {
          username,
          password,
          confirmed_host_key: confirmed && fingerprint ? fingerprint : null,
        };
        const request = id
          ? api
              .authorizeFile(id, credentials)
              .then(() => api.control(id, { kind: "Resume" }))
          : api.submit({
              url,
              destination: destination || null,
              filename: null,
              sha256: null,
              conflict: "rename",
              credentials: username ? credentials : null,
            });
        void request
          .then(() => {
            setPassword("");
            done();
          })
          .catch((e: unknown) => setError(String(e)))
          .finally(() => setBusy(false));
      }}
    >
      {!id && (
        <label>
          File URL
          <input
            value={url}
            onChange={(e) => {
              setUrl(e.target.value);
              setFingerprint("");
              setConfirmed(false);
            }}
            placeholder="sftp://server/path/file.zip"
            required
          />
        </label>
      )}
      <label>
        Username
        <input
          autoComplete="off"
          value={username}
          onChange={(e) => setUsername(e.target.value)}
          required={!!id}
        />
      </label>
      <label>
        Password
        <input
          type="password"
          autoComplete="off"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
      </label>
      <p>
        Credentials stay in memory. Supply them again to resume after restarting
        Okytes. FTP without a username uses anonymous login.
      </p>
      {sftp && (
        <>
          <label>
            SFTP host fingerprint
            <input
              value={fingerprint}
              onChange={(e) => {
                setFingerprint(e.target.value);
                setConfirmed(false);
              }}
              placeholder="SHA256:…"
            />
          </label>
          <label>
            <input
              type="checkbox"
              checked={confirmed}
              disabled={!fingerprint}
              onChange={(e) => setConfirmed(e.target.checked)}
            />{" "}
            I verified this fingerprint with the server administrator and trust
            it.
          </label>
          <p>
            An unknown host fails with its fingerprint in the download details.
            Changed keys are always refused.
          </p>
        </>
      )}
      {error && <p role="alert">{error}</p>}
      <button className="primary" disabled={busy}>
        {busy
          ? "Submitting…"
          : id
            ? "Authorize and resume"
            : "Add file transfer"}
      </button>
    </form>
  );
}
export function MediaForm({
  destination,
  done,
}: {
  destination: string;
  done: () => void;
}) {
  const [url, setUrl] = useState("");
  const [format, setFormat] = useState("hls");
  const [choices, setChoices] = useState<MediaChoice[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [merge, setMerge] = useState(true);
  const [ffmpeg, setFfmpeg] = useState<string | null>();
  useEffect(() => {
    let live = true;
    void api
      .ffmpegStatus()
      .then((value) => live && setFfmpeg(value))
      .catch(() => live && setFfmpeg(null));
    return () => {
      live = false;
    };
  }, []);
  const picked = choices.filter((c) => selected.has(c.id));
  const mergeable =
    picked.some((c) => c.kind === "video") && picked.length >= 2 && !!ffmpeg;
  return (
    <form
      className="settings"
      onSubmit={(e) => {
        e.preventDefault();
        setBusy(true);
        setError("");
        void (async () => {
          const ids: string[] = [];
          // Video first: the merged file takes its folder and name.
          const ordered = [...picked].sort(
            (a, b) => Number(b.kind === "video") - Number(a.kind === "video"),
          );
          for (const choice of ordered) {
            ids.push(
              await api.submit({
                url: choice.url,
                destination: destination || null,
                filename: null,
                sha256: null,
                conflict: "rename",
                credentials: null,
              }),
            );
          }
          if (merge && mergeable) await api.planMerge(ids);
          done();
        })()
          .catch((e: unknown) => setError(String(e)))
          .finally(() => setBusy(false));
      }}
    >
      <label>
        Manifest URL
        <input
          value={url}
          onChange={(e) => {
            setUrl(e.target.value);
            setChoices([]);
            setSelected(new Set());
          }}
          placeholder="https://server/video.m3u8"
          required
        />
      </label>
      <label>
        Manifest format
        <select
          value={format}
          onChange={(e) => {
            setFormat(e.target.value);
            setChoices([]);
            setSelected(new Set());
          }}
        >
          <option value="hls">HLS</option>
          <option value="dash">MPEG-DASH</option>
        </select>
      </label>
      <button
        type="button"
        disabled={busy || !url}
        onClick={() => {
          setBusy(true);
          setError("");
          void api
            .inspectMedia(
              url.startsWith(`${format}+`) ? url : `${format}+${url}`,
            )
            .then((value) => {
              setChoices(value);
              setSelected(new Set());
            })
            .catch((e: unknown) => setError(String(e)))
            .finally(() => setBusy(false));
        }}
      >
        Inspect tracks
      </button>
      {choices.map((choice) => (
        <label key={choice.id}>
          <input
            type="checkbox"
            checked={selected.has(choice.id)}
            onChange={(e) =>
              setSelected((old) => {
                const next = new Set(old);
                if (e.target.checked) next.add(choice.id);
                else next.delete(choice.id);
                return next;
              })
            }
          />{" "}
          {choice.kind}: {choice.label}
        </label>
      ))}
      <label>
        <input
          type="checkbox"
          checked={merge && mergeable}
          disabled={!mergeable}
          onChange={(e) => setMerge(e.target.checked)}
        />{" "}
        Merge the selected video, audio and subtitles into one MKV when all
        finish
      </label>
      <p className="muted">
        {ffmpeg
          ? `Merging uses ${ffmpeg} and copies streams without re-encoding. The separate tracks are kept.`
          : ffmpeg === null
            ? "Merging needs ffmpeg. Install it and make sure it is on PATH, or set OKYTES_FFMPEG to its full path."
            : "Checking for ffmpeg…"}
      </p>
      <p>
        Each selected track downloads separately. Supported VOD streams assemble
        as MPEG-TS, fragmented MP4, or WebVTT. Live streams, DRM and layouts
        requiring remuxing are refused.
      </p>
      {error && <p role="alert">{error}</p>}
      <button className="primary" disabled={busy || selected.size === 0}>
        {busy ? "Submitting…" : "Download selected tracks"}
      </button>
    </form>
  );
}
