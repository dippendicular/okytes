import { useEffect, useState } from "react";
import {
  api,
  bytes,
  type Job,
  type Settings,
  type StorageProvider,
} from "./api";
import {
  Connections,
  SegmentMap,
  duration,
  rate,
  useTelemetry,
} from "./Details";

type Tab = "status" | "limit" | "completion";
const tabs: { key: Tab; label: string }[] = [
  { key: "status", label: "Download status" },
  { key: "limit", label: "Speed limiter" },
  { key: "completion", label: "Options on completion" },
];
const active = ["Probing", "Downloading", "Verifying"];

function friendly(error: unknown): string {
  return String(error).replace(/^Error:\s*/i, "");
}

// Detail windows share stored settings with the main window, so every edit
// starts from a fresh read and changes only this download's entry.
async function updateSettings(
  change: (settings: Settings) => Settings,
): Promise<Settings> {
  const next = change(await api.settings());
  await api.setSettings(next);
  return next;
}

function useTheme(settings: Settings | undefined) {
  useEffect(() => {
    const media = matchMedia("(prefers-color-scheme: dark)");
    const apply = () => {
      const root = document.documentElement.dataset;
      root.theme =
        !settings || settings.theme === "system"
          ? media.matches
            ? "dark"
            : "light"
          : settings.theme;
      root.accent = settings?.accent ?? "blue";
      root.density = settings?.density ?? "comfortable";
      root.motion = settings?.animations === false ? "off" : "on";
    };
    apply();
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, [settings]);
}

function StatusTab({
  job,
  telemetry,
}: {
  job: Job;
  telemetry: ReturnType<typeof useTelemetry>["telemetry"];
}) {
  const [details, setDetails] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [link, setLink] = useState("");
  const connections = telemetry?.connections ?? [];
  const percent = job.total ? (job.bytes / job.total) * 100 : null;
  const remaining =
    job.total !== null && job.speed > 0
      ? (job.total - job.bytes) / job.speed
      : null;
  const resume =
    telemetry?.range_support === false
      ? "No"
      : job.protocol !== "http" || (job.etag?.startsWith('"') ?? false)
        ? "Yes"
        : "Restarts from zero (no strong ETag)";
  const control = (kind: "Pause" | "Resume" | "Cancel") => {
    setBusy(true);
    setError("");
    api
      .control(job.id, { kind })
      .catch((e: unknown) => setError(friendly(e)))
      .finally(() => setBusy(false));
  };
  return (
    <div className="dw-status">
      <p className="dw-url" title={job.url}>
        {job.url.replace(/^(hls|dash)\+/, "")}
      </p>
      <dl className="dw-facts">
        <dt>Status</dt>
        <dd>
          {job.state === "Downloading"
            ? "Receiving data…"
            : job.state === "Probing"
              ? "Connecting…"
              : job.state}
        </dd>
        <dt>File size</dt>
        <dd>{job.total === null ? "Unknown" : bytes(job.total)}</dd>
        <dt>Downloaded</dt>
        <dd>
          {bytes(job.bytes)}
          {percent !== null && ` (${percent.toFixed(2)}%)`}
        </dd>
        <dt>Transfer rate</dt>
        <dd>{job.state === "Downloading" ? rate(job.speed) : "—"}</dd>
        <dt>Time left</dt>
        <dd>
          {job.state === "Completed"
            ? "Done"
            : remaining === null
              ? "—"
              : duration(remaining)}
        </dd>
        <dt>Resume capability</dt>
        <dd>{resume}</dd>
      </dl>
      <progress
        aria-label={`${job.name} progress`}
        value={
          job.total === null
            ? undefined
            : job.state === "Completed"
              ? 1
              : job.bytes
        }
        max={
          job.total === null
            ? undefined
            : job.state === "Completed"
              ? 1
              : Math.max(1, job.total)
        }
      />
      {error && (
        <p role="alert" className="alert">
          {error}
        </p>
      )}
      <div className="dw-buttons">
        <button onClick={() => setDetails(!details)}>
          {details ? "<< Hide details" : "Show details >>"}
        </button>
        <span />
        <button
          disabled={busy || !["Paused", "Failed"].includes(job.state)}
          onClick={() => control("Resume")}
        >
          Resume
        </button>
        <button
          disabled={busy || ![...active, "Queued"].includes(job.state)}
          onClick={() => control("Pause")}
        >
          Pause
        </button>
        <button
          disabled={busy || ["Completed", "Cancelled"].includes(job.state)}
          onClick={() => control("Cancel")}
        >
          Cancel
        </button>
      </div>
      {job.protocol === "http" && job.state === "Failed" && (
        <form
          className="dw-refresh"
          onSubmit={(e) => {
            e.preventDefault();
            setBusy(true);
            setError("");
            api
              .control(job.id, { kind: "Refresh", value: link })
              .then(() => api.control(job.id, { kind: "Resume" }))
              .then(() => setLink(""))
              .catch((e: unknown) => setError(friendly(e)))
              .finally(() => setBusy(false));
          }}
        >
          <input
            type="url"
            value={link}
            onChange={(e) => setLink(e.target.value)}
            placeholder="Link expired? Paste a fresh link to the same file"
            aria-label="Replacement link"
            required
          />
          <button disabled={busy}>Refresh link and resume</button>
        </form>
      )}
      {details && job.protocol !== "torrent" && (
        <>
          <p className="dw-caption">
            Start positions and download progress by connections
          </p>
          <SegmentMap job={job} connections={connections} />
          <Connections job={job} connections={connections} />
        </>
      )}
    </div>
  );
}

function LimitTab({ job }: { job: Job }) {
  const [settings, setSettings] = useState<Settings>();
  const [mode, setMode] = useState<"default" | "none" | "custom">("default");
  const [kib, setKib] = useState("1024");
  const [status, setStatus] = useState("");
  useEffect(() => {
    let live = true;
    void api.settings().then((value) => {
      if (!live) return;
      setSettings(value);
      const entry = value.traffic.job_limits.find((l) => l.id === job.id);
      if (!entry) setMode("default");
      else if (entry.bps === null) setMode("none");
      else {
        setMode("custom");
        setKib(String(Math.max(1, Math.round(entry.bps / 1024))));
      }
    });
    return () => {
      live = false;
    };
  }, [job.id]);
  const fallback = settings?.traffic.per_job_bps ?? null;
  const save = () => {
    const value = Math.round(Number(kib));
    if (mode === "custom" && (!Number.isFinite(value) || value < 1)) {
      setStatus("Enter a limit of at least 1 KiB/s.");
      return;
    }
    updateSettings((current) => {
      const others = current.traffic.job_limits.filter((l) => l.id !== job.id);
      const job_limits =
        mode === "default"
          ? others
          : [
              ...others,
              { id: job.id, bps: mode === "none" ? null : value * 1024 },
            ];
      if (job_limits.length > 128)
        throw new Error(
          "At most 128 downloads can have their own limit. Remove one first.",
        );
      return { ...current, traffic: { ...current.traffic, job_limits } };
    })
      .then((next) => {
        setSettings(next);
        setStatus("Saved. The new limit applies immediately.");
      })
      .catch((e: unknown) => setStatus(friendly(e)));
  };
  return (
    <div className="dw-form">
      <p className="muted">
        Limits this download only. The global limit in Settings still caps the
        total across all downloads.
      </p>
      <label className="checkbox">
        <input
          type="radio"
          checked={mode === "default"}
          onChange={() => setMode("default")}
        />
        Use the default per-download limit (
        {fallback === null ? "unlimited" : rate(fallback)})
      </label>
      <label className="checkbox">
        <input
          type="radio"
          checked={mode === "none"}
          onChange={() => setMode("none")}
        />
        No limit for this download
      </label>
      <label className="checkbox">
        <input
          type="radio"
          checked={mode === "custom"}
          onChange={() => setMode("custom")}
        />
        Limit to
        <input
          className="dw-number"
          type="number"
          min={1}
          value={kib}
          disabled={mode !== "custom"}
          onChange={(e) => setKib(e.target.value)}
        />
        KiB/s
      </label>
      <button className="primary" disabled={!settings} onClick={save}>
        Apply limit
      </button>
      {status && <p role="status">{status}</p>}
    </div>
  );
}

function CompletionTab({ job }: { job: Job }) {
  const [openFile, setOpenFile] = useState(false);
  const [openFolder, setOpenFolder] = useState(false);
  const [notify, setNotify] = useState(true);
  const [upload, setUpload] = useState("");
  const [providers, setProviders] = useState<StorageProvider[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [status, setStatus] = useState("");
  useEffect(() => {
    let live = true;
    void api.settings().then((value) => {
      if (!live) return;
      const entry = value.completion?.find((c) => c.id === job.id);
      setOpenFile(entry?.open_file ?? false);
      setOpenFolder(entry?.open_folder ?? false);
      setNotify(entry?.notify ?? value.notifications);
      setUpload(entry?.upload ?? "");
      void api
        .storageStatus()
        .then((list) => live && setProviders(list))
        .catch(() => {});
      setLoaded(true);
    });
    return () => {
      live = false;
    };
  }, [job.id]);
  const done = ["Completed", "Cancelled"].includes(job.state);
  const save = () => {
    updateSettings((current) => {
      const others = (current.completion ?? []).filter((c) => c.id !== job.id);
      const completion = [
        ...others,
        {
          id: job.id,
          open_file: openFile,
          open_folder: openFolder,
          notify,
          ...(upload ? { upload } : {}),
        },
      ];
      if (completion.length > 128)
        throw new Error("Too many downloads have pending completion options.");
      return { ...current, completion };
    })
      .then(() =>
        setStatus("Saved. Okytes runs these once when the download completes."),
      )
      .catch((e: unknown) => setStatus(friendly(e)));
  };
  return (
    <div className="dw-form">
      {done && (
        <p className="muted">
          This download has finished; completion options no longer apply.
        </p>
      )}
      <label className="checkbox">
        <input
          type="checkbox"
          checked={openFile}
          disabled={done}
          onChange={(e) => setOpenFile(e.target.checked)}
        />
        Open the file
      </label>
      <label className="checkbox">
        <input
          type="checkbox"
          checked={openFolder}
          disabled={done}
          onChange={(e) => setOpenFolder(e.target.checked)}
        />
        Show the file in its folder
      </label>
      <label className="checkbox">
        <input
          type="checkbox"
          checked={notify}
          disabled={done}
          onChange={(e) => setNotify(e.target.checked)}
        />
        Show a notification
      </label>
      <label>
        Upload to cloud storage
        <select
          value={upload}
          disabled={done}
          onChange={(e) => setUpload(e.target.value)}
        >
          <option value="">Don't upload</option>
          {providers.map((p) => (
            <option key={p.provider} value={p.provider} disabled={!p.connected}>
              {p.name}
              {p.connected ? "" : " (connect in Settings)"}
            </option>
          ))}
        </select>
      </label>
      <p className="muted">
        Options run in the Okytes app even if this window is closed. Opening a
        file starts its default application, so only enable it for files you
        trust.
      </p>
      <button className="primary" disabled={!loaded || done} onClick={save}>
        Save options
      </button>
      {status && <p role="status">{status}</p>}
    </div>
  );
}

export function DetailsWindow({ id }: { id: string }) {
  const [tab, setTab] = useState<Tab>("status");
  const [settings, setSettings] = useState<Settings>();
  const [listed, setListed] = useState<Job>();
  const [error, setError] = useState("");
  const { telemetry, unavailable } = useTelemetry(id);
  useTheme(settings);
  useEffect(() => {
    void api
      .settings()
      .then(setSettings)
      .catch(() => {});
  }, []);
  // Without embedded telemetry (daemon mode) fall back to the durable record.
  useEffect(() => {
    if (!unavailable) return;
    let live = true;
    const poll = () =>
      void api
        .get(id)
        .then((job) => live && setListed(job))
        .catch((e: unknown) => live && setError(friendly(e)));
    poll();
    const timer = setInterval(poll, 500);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [id, unavailable]);
  const job = telemetry?.job ?? listed;
  useEffect(() => {
    if (!job) return;
    const percent = job.total
      ? Math.floor((job.bytes / job.total) * 100)
      : null;
    document.title = `${percent === null ? "" : `${percent}% `}${job.name}`;
  }, [job]);
  if (!job)
    return (
      <main className="details-window">
        <p className="muted">{error || "Loading download…"}</p>
      </main>
    );
  return (
    <main className="details-window">
      <nav className="dw-tabs" role="tablist">
        {tabs.map((t) => (
          <button
            key={t.key}
            role="tab"
            aria-selected={tab === t.key}
            className={tab === t.key ? "selected" : ""}
            onClick={() => setTab(t.key)}
          >
            {t.label}
          </button>
        ))}
      </nav>
      <section role="tabpanel">
        {tab === "status" && <StatusTab job={job} telemetry={telemetry} />}
        {tab === "limit" && <LimitTab job={job} />}
        {tab === "completion" && <CompletionTab job={job} />}
      </section>
    </main>
  );
}
