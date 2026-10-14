import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { useVirtualizer } from "@tanstack/react-virtual";
import { TorrentForm, TorrentDiagnostics } from "./Torrents";
import { TrafficSettings, defaultTraffic } from "./TrafficSettings";
import {
  api,
  bytes,
  category,
  parseUrls,
  type Action,
  type Job,
  type Settings,
} from "./api";

const active = ["Probing", "Downloading", "Verifying", "Seeding"];
const views = [
  "All downloads",
  "Active",
  "Queued",
  "Completed",
  "Failed",
] as const;

function Modal({
  title,
  children,
  open,
  onOpenChange,
  error,
}: {
  title: string;
  children: React.ReactNode;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  error?: string;
}) {
  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="overlay" />
        <Dialog.Content className="dialog">
          <div className="dialog-title">
            <Dialog.Title>{title}</Dialog.Title>
            <Dialog.Close aria-label="Close dialog">×</Dialog.Close>
          </div>
          <Dialog.Description className="sr-only">
            {title} options
          </Dialog.Description>
          {error && (
            <p role="alert" className="alert">
              {error}
            </p>
          )}
          {children}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

export function App() {
  const [jobs, setJobs] = useState<Job[]>([]);
  const [connected, setConnected] = useState(false);
  const [historyOffset, setHistoryOffset] = useState(0);
  const [hasMore, setHasMore] = useState(false);
  const [view, setView] = useState<string>("All downloads");
  const [search, setSearch] = useState("");
  const [filter, setFilter] = useState("All types");
  const [error, setError] = useState("");
  const [addOpen, setAddOpen] = useState(false);
  const [torrentOpen, setTorrentOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [helpOpen, setHelpOpen] = useState(false);
  const [settings, setSettings] = useState<Settings>();
  const [urls, setUrls] = useState("");
  const [destination, setDestination] = useState("");
  const [duplicates, setDuplicates] = useState(false);
  const [busy, setBusy] = useState(false);
  const [selected, setSelected] = useState<string>();
  const [rename, setRename] = useState("");
  const [samples, setSamples] = useState<number[]>([]);
  const scroll = useRef<HTMLDivElement>(null);
  const searchInput = useRef<HTMLInputElement>(null);
  const refreshLock = useRef(false);

  const refresh = useCallback(async () => {
    if (refreshLock.current) return;
    refreshLock.current = true;
    try {
      const result: Job[] = [];
      let offset: number | null = historyOffset;
      // The UI retains at most 2048 rows; virtualized rendering stays independent of history size.
      while (offset !== null && result.length < 2048) {
        const page = await api.list(offset);
        result.push(...page.jobs);
        offset = page.next;
      }
      setJobs(result);
      setConnected(true);
      setHasMore(offset !== null);
      const speed = result.reduce((sum, job) => sum + job.speed, 0);
      setSamples((old) => [...old.slice(-59), speed]);
    } catch (e) {
      setError(String(e));
      setConnected(false);
    } finally {
      refreshLock.current = false;
    }
  }, [historyOffset]);

  useEffect(() => {
    let live = true;
    void api
      .settings()
      .then((value) => {
        if (live) {
          setSettings(value);
          setDestination(value.destination);
        }
      })
      .catch((e) => setError(String(e)));
    void refresh();
    const interval = setInterval(() => {
      void refresh();
    }, 500);
    return () => {
      live = false;
      clearInterval(interval);
    };
  }, [refresh]);

  useEffect(() => {
    const media = matchMedia("(prefers-color-scheme: dark)");
    const apply = () => {
      document.documentElement.dataset.theme =
        settings?.theme === "system" || !settings
          ? media.matches
            ? "dark"
            : "light"
          : settings.theme;
    };
    apply();
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, [settings]);

  useEffect(() => {
    const key = (event: KeyboardEvent) => {
      if (event.ctrlKey || event.metaKey) {
        if (event.key.toLowerCase() === "n") {
          event.preventDefault();
          setAddOpen(true);
        }
        if (event.key.toLowerCase() === "f") {
          event.preventDefault();
          searchInput.current?.focus();
        }
      }
      if (event.key === "F1") {
        event.preventDefault();
        setHelpOpen(true);
      }
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, []);

  const filtered = useMemo(
    () =>
      jobs.filter((job) => {
        if (view === "Active" && !active.includes(job.state)) return false;
        if (view === "Queued" && !["Queued", "Paused"].includes(job.state))
          return false;
        if (view === "Completed" && job.state !== "Completed") return false;
        if (view === "Failed" && !["Failed", "Cancelled"].includes(job.state))
          return false;
        return (
          (filter === "All types" || category(job.name) === filter) &&
          `${job.name} ${job.url} ${job.destination}`
            .toLowerCase()
            .includes(search.toLowerCase())
        );
      }),
    [jobs, view, search, filter],
  );
  const virtual = useVirtualizer({
    count: filtered.length,
    getScrollElement: () => scroll.current,
    estimateSize: () => 84,
    overscan: 8,
  });
  const selectedJob = jobs.find((job) => job.id === selected);

  async function act(job: Job, action: Action) {
    setBusy(true);
    setError("");
    try {
      await api.control(job.id, action);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }
  async function add() {
    setBusy(true);
    setError("");
    try {
      await api.add({
        urls: parseUrls(urls),
        destination: destination || null,
        allow_duplicates: duplicates,
      });
      setAddOpen(false);
      setUrls("");
      setDuplicates(false);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }
  async function clipboard() {
    try {
      const text = await navigator.clipboard.readText();
      setUrls(text);
      setAddOpen(true);
    } catch {
      setError(
        "Clipboard access was denied. Paste URLs into the Add downloads dialog.",
      );
    }
  }
  async function importFile(file?: File) {
    if (!file) return;
    if (file.size > 819200) {
      setError(
        "Import is too large. Use a text file containing at most 100 URLs.",
      );
      return;
    }
    try {
      const text = await file.text();
      parseUrls(text);
      setUrls(text);
      setAddOpen(true);
    } catch (e) {
      setError(String(e));
    }
  }
  const totalSpeed = jobs.reduce((sum, job) => sum + job.speed, 0);
  const peak = Math.max(1, ...samples);

  return (
    <div
      className="app"
      onDragOver={(e) => {
        e.preventDefault();
      }}
      onDrop={(e) => {
        e.preventDefault();
        if (e.dataTransfer.files.length) {
          void importFile(e.dataTransfer.files[0]);
        } else {
          setUrls(
            e.dataTransfer
              .getData("text/uri-list")
              .split("\n")
              .filter((line) => !line.startsWith("#"))
              .join("\n") || e.dataTransfer.getData("text/plain"),
          );
          setAddOpen(true);
        }
      }}
    >
      <aside className="sidebar">
        <div className="brand">
          <div>
            <img
              className="brand-logo"
              src="/brand/okytes_logo_transperant.png"
              alt="Okytes"
            />
            <small>DOWNLOAD MANAGER</small>
          </div>
        </div>
        <button className="primary add" onClick={() => setAddOpen(true)}>
          ＋ Add downloads
        </button>
        <button onClick={() => setTorrentOpen(true)}>Add torrent</button>
        <nav aria-label="Download views">
          {views.map((name) => (
            <button
              key={name}
              aria-current={view === name ? "page" : undefined}
              className={view === name ? "nav selected" : "nav"}
              onClick={() => setView(name)}
            >
              {name}
              <span>
                {
                  jobs.filter(
                    (job) =>
                      name === "All downloads" ||
                      (name === "Active"
                        ? active.includes(job.state)
                        : name === "Queued"
                          ? ["Queued", "Paused"].includes(job.state)
                          : name === "Failed"
                            ? ["Failed", "Cancelled"].includes(job.state)
                            : job.state === name),
                  ).length
                }
              </span>
            </button>
          ))}
        </nav>
        <div className="sidebar-bottom">
          <button
            onClick={() => {
              void clipboard();
            }}
          >
            Paste from clipboard
          </button>
          <label className="file-button">
            Import URL file
            <input
              type="file"
              accept=".txt,.urls,text/plain"
              onChange={(e) => {
                void importFile(e.target.files?.[0]);
                e.target.value = "";
              }}
            />
          </label>
          <button onClick={() => setSettingsOpen(true)}>Settings</button>
          <button onClick={() => setHelpOpen(true)}>Keyboard shortcuts</button>
        </div>
      </aside>
      <main>
        <header>
          <div>
            <p className="eyebrow">YOUR DOWNLOADS</p>
            <h1>{view}</h1>
            <p className="muted">
              {filtered.length} downloads ·{" "}
              {jobs.filter((j) => active.includes(j.state)).length} running
            </p>
          </div>
          <div className="speed-summary">
            <strong>{bytes(totalSpeed)}/s</strong>
            <span>Current transfer rate</span>
          </div>
        </header>
        {error && (
          <div role="alert" className="alert">
            {error}
            <button aria-label="Dismiss error" onClick={() => setError("")}>
              ×
            </button>
          </div>
        )}
        <section className="toolbar" aria-label="Search and filters">
          <input
            ref={searchInput}
            type="search"
            aria-label="Search downloads"
            placeholder="Search filenames, URLs or folders…"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
          />
          <select
            aria-label="File category"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          >
            {[
              "All types",
              "Video",
              "Audio",
              "Image",
              "Archive",
              "Document",
              "Other",
            ].map((value) => (
              <option key={value}>{value}</option>
            ))}
          </select>
          <button
            disabled={
              busy ||
              !jobs.some(
                (j) => active.includes(j.state) || j.state === "Queued",
              )
            }
            onClick={() => {
              void (async () => {
                setBusy(true);
                try {
                  await api.pauseAll();
                  await refresh();
                } catch (e) {
                  setError(String(e));
                } finally {
                  setBusy(false);
                }
              })();
            }}
          >
            Pause all
          </button>
        </section>
        <div className="list-heading">
          <span>FILE / SOURCE</span>
          <span>PROGRESS / TRANSFER</span>
          <span>CONTROLS</span>
        </div>
        <div
          ref={scroll}
          className="job-list"
          role="list"
          aria-label="Downloads"
          tabIndex={0}
        >
          <div style={{ height: virtual.getTotalSize(), position: "relative" }}>
            {virtual.getVirtualItems().map((item) => {
              const job = filtered[item.index];
              return (
                <div
                  role="listitem"
                  aria-posinset={item.index + 1}
                  aria-setsize={filtered.length}
                  key={job.id}
                  className="job-row"
                  style={{
                    position: "absolute",
                    top: 0,
                    left: 0,
                    width: "100%",
                    height: item.size,
                    transform: `translateY(${item.start}px)`,
                  }}
                >
                  <div className="file-cell">
                    <span
                      className={`file-icon ${category(job.name).toLowerCase()}`}
                    >
                      {category(job.name).slice(0, 3).toUpperCase()}
                    </span>
                    <div className="file-meta">
                      <button
                        className="filename"
                        onClick={() => {
                          setSelected(job.id);
                          setRename(job.name);
                        }}
                      >
                        {job.name}
                      </button>
                      <small title={job.url}>{job.url}</small>
                    </div>
                  </div>
                  <div className="progress-cell">
                    <div className="progress-label">
                      <span className={`status ${job.state.toLowerCase()}`}>
                        {job.state}
                      </span>
                      <span>
                        {bytes(job.bytes)} /{" "}
                        {job.total === null ? "unknown" : bytes(job.total)}
                      </span>
                    </div>
                    <progress
                      aria-label={`${job.name} progress`}
                      value={
                        job.total === null
                          ? undefined
                          : job.total === 0 && job.state === "Completed"
                            ? 1
                            : job.bytes
                      }
                      max={
                        job.total === null ? undefined : Math.max(1, job.total)
                      }
                      aria-valuetext={
                        job.total === null
                          ? `${bytes(job.bytes)} downloaded; total size unknown`
                          : undefined
                      }
                    />
                    <small>
                      {job.speed
                        ? `${bytes(job.speed)}/s · ${job.total ? `${Math.ceil((job.total - job.bytes) / job.speed)}s remaining` : "size unknown"}`
                        : category(job.name)}
                      {job.priority ? ` · Priority ${job.priority}` : ""}
                    </small>
                  </div>
                  <div className="row-controls">
                    {(active.includes(job.state) || job.state === "Queued") && (
                      <button
                        disabled={busy}
                        aria-label={`${job.state === "Seeding" ? "Stop sharing" : "Pause"} ${job.name}`}
                        onClick={() => {
                          void act(job, { kind: "Pause" });
                        }}
                      >
                        {job.state === "Seeding" ? "Stop sharing" : "Pause"}
                      </button>
                    )}
                    {["Paused", "Failed"].includes(job.state) && (
                      <button
                        disabled={busy}
                        aria-label={`Resume ${job.name}`}
                        onClick={() => {
                          void act(job, { kind: "Resume" });
                        }}
                      >
                        Resume
                      </button>
                    )}
                    <button
                      aria-label={`Details for ${job.name}`}
                      onClick={() => {
                        setSelected(job.id);
                        setRename(job.name);
                      }}
                    >
                      Details
                    </button>
                  </div>
                </div>
              );
            })}
          </div>
          {!filtered.length && (
            <div className="empty" role="listitem">
              <span>↓</span>
              <h2>
                {search || filter !== "All types"
                  ? "No matching downloads"
                  : "Your downloads belong here"}
              </h2>
              <p>
                Paste a link, import a URL file, or drop links onto this window.
              </p>
              <button onClick={() => setAddOpen(true)}>Add a download</button>
            </div>
          )}
        </div>
        <footer>
          <div>
            <span className="dot" />{" "}
            {connected
              ? "Engine connected · durable checkpoints"
              : "Connecting to engine…"}
          </div>
          <span>
            {settings?.concurrency ?? "—"} simultaneous downloads · History
            entries {historyOffset + 1}–{historyOffset + jobs.length}
          </span>
          <div className="history-pages">
            <button
              disabled={!historyOffset || busy}
              onClick={() =>
                setHistoryOffset(Math.max(0, historyOffset - 2048))
              }
            >
              Previous history page
            </button>
            <button
              disabled={!hasMore || busy}
              onClick={() => setHistoryOffset(historyOffset + 2048)}
            >
              Next history page
            </button>
          </div>
        </footer>
        <section className="graph" aria-label="Transfer speed history">
          <div>
            <strong>Transfer activity</strong>
            <span>Last 30 seconds · {bytes(totalSpeed)}/s</span>
          </div>
          <svg
            viewBox="0 0 600 60"
            preserveAspectRatio="none"
            role="img"
            aria-label="Measured transfer rate over the last 30 seconds"
          >
            <polyline
              points={samples
                .map(
                  (value, index) =>
                    `${(index * 600) / 59},${58 - (value / peak) * 54}`,
                )
                .join(" ")}
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
            />
          </svg>
        </section>
      </main>
      <Modal
        error={error}
        title="Add downloads"
        open={addOpen}
        onOpenChange={setAddOpen}
      >
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void add();
          }}
        >
          <label>
            URLs
            <textarea
              autoFocus
              placeholder="https://example.com/file.zip\nOne URL per line, up to 100"
              value={urls}
              onChange={(e) => setUrls(e.target.value)}
              required
            />
          </label>
          <label>
            Save to
            <input
              value={destination}
              onChange={(e) => setDestination(e.target.value)}
              required
            />
          </label>
          <label className="checkbox">
            <input
              type="checkbox"
              checked={duplicates}
              onChange={(e) => setDuplicates(e.target.checked)}
            />
            Add duplicates, even if these URLs are already in history
          </label>
          <p className="muted">
            Existing files are preserved. Filename conflicts receive a numbered
            suffix.
          </p>
          <button className="primary" disabled={busy}>
            Start downloads
          </button>
        </form>
      </Modal>
      <Modal
        title="Add torrent"
        open={torrentOpen}
        onOpenChange={setTorrentOpen}
      >
        <TorrentForm
          destination={destination}
          done={() => {
            setTorrentOpen(false);
            void refresh();
          }}
        />
      </Modal>
      <Modal
        error={error}
        title="Download details"
        open={!!selectedJob}
        onOpenChange={(open) => {
          if (!open) setSelected(undefined);
        }}
      >
        {selectedJob && (
          <div className="details">
            <h3>{selectedJob.name}</h3>
            {selectedJob.protocol === "torrent" && (
              <TorrentDiagnostics id={selectedJob.id} />
            )}
            <dl>
              <dt>Source</dt>
              <dd>{selectedJob.url}</dd>
              <dt>Destination</dt>
              <dd>{selectedJob.destination}</dd>
              {selectedJob.protocol !== "torrent" && (
                <>
                  <dt>Strong ETag</dt>
                  <dd>
                    {selectedJob.etag ??
                      "Unavailable — safe resume may restart from zero"}
                  </dd>
                  <dt>HTTP response</dt>
                  <dd>
                    {selectedJob.response_status ?? "Not sampled this session"}
                  </dd>
                  <dt>Time to headers</dt>
                  <dd>
                    {selectedJob.response_latency_ms === null
                      ? "Not sampled this session"
                      : `${selectedJob.response_latency_ms.toFixed(0)} ms (includes admission and redirects)`}
                  </dd>
                  <dt>Retries</dt>
                  <dd>{selectedJob.retries}</dd>
                  <dt>Workers / segments</dt>
                  <dd>
                    {selectedJob.workers} active / {selectedJob.segments.length}{" "}
                    total
                  </dd>
                </>
              )}
            </dl>
            {selectedJob.error && (
              <p role="alert" className="alert">
                {selectedJob.error}
              </p>
            )}
            <div className="segment-map" aria-label="Durable segment ownership">
              {selectedJob.segments.map((segment) => (
                <div key={segment.start}>
                  <span>
                    {bytes(segment.start)}–{bytes(segment.end)} ·{" "}
                    {segment.state}
                  </span>
                  <progress
                    aria-label={`Segment at byte ${segment.start}`}
                    value={segment.bytes}
                    max={segment.end - segment.start + 1}
                  />
                </div>
              ))}
            </div>
            {selectedJob.protocol !== "torrent" &&
              ["Paused", "Queued", "Failed"].includes(selectedJob.state) && (
                <form
                  onSubmit={(e) => {
                    e.preventDefault();
                    void act(selectedJob, { kind: "Rename", value: rename });
                  }}
                >
                  <label>
                    Filename
                    <input
                      value={rename}
                      onChange={(e) => setRename(e.target.value)}
                      maxLength={200}
                    />
                  </label>
                  <button disabled={busy}>Rename</button>
                </form>
              )}
            {selectedJob.state === "Queued" && (
              <label>
                Queue priority
                <select
                  value={selectedJob.priority}
                  disabled={busy}
                  onChange={(e) => {
                    void act(selectedJob, {
                      kind: "Priority",
                      value: Number(e.target.value),
                    });
                  }}
                >
                  {[-100, -10, 0, 10, 100].map((n) => (
                    <option key={n} value={n}>
                      {n}
                    </option>
                  ))}
                </select>
              </label>
            )}
            <div className="actions">
              {selectedJob.protocol !== "torrent" && (
                <button
                  disabled={busy}
                  onClick={() => {
                    void act(selectedJob, { kind: "Restart" });
                  }}
                >
                  Download again
                </button>
              )}
              {!["Completed", "Cancelled"].includes(selectedJob.state) && (
                <button
                  disabled={busy}
                  onClick={() => {
                    void act(selectedJob, { kind: "Cancel" });
                  }}
                >
                  Cancel download
                </button>
              )}
              <button
                disabled={busy}
                onClick={() => {
                  void act(selectedJob, { kind: "Remove" });
                  setSelected(undefined);
                }}
              >
                Remove from history
              </button>
            </div>
            <p className="muted">
              Removing history preserves completed files. Download again creates
              a new entry and preserves the existing file.
            </p>
          </div>
        )}
      </Modal>
      <Modal
        error={error}
        title="Settings"
        open={settingsOpen}
        onOpenChange={(open) => {
          setSettingsOpen(open);
          if (!open)
            void api
              .settings()
              .then(setSettings)
              .catch((e) => setError(String(e)));
        }}
      >
        {settings && (
          <form
            onSubmit={(e) => {
              e.preventDefault();
              setBusy(true);
              void api
                .setSettings(settings)
                .then(() => {
                  setDestination(settings.destination);
                  setSettingsOpen(false);
                })
                .catch((e) => setError(String(e)))
                .finally(() => setBusy(false));
            }}
          >
            <label>
              Default download folder
              <input
                value={settings.destination}
                onChange={(e) =>
                  setSettings({ ...settings, destination: e.target.value })
                }
                required
              />
            </label>
            <label>
              Simultaneous downloads
              <input
                type="number"
                min={1}
                max={32}
                value={settings.concurrency}
                onChange={(e) =>
                  setSettings({
                    ...settings,
                    concurrency: Number(e.target.value),
                  })
                }
              />
            </label>
            <label>
              Appearance
              <select
                value={settings.theme}
                onChange={(e) =>
                  setSettings({ ...settings, theme: e.target.value })
                }
              >
                {["system", "light", "dark"].map((value) => (
                  <option key={value}>{value}</option>
                ))}
              </select>
            </label>
            <TrafficSettings
              policy={settings.traffic ?? defaultTraffic}
              onChange={(traffic) => setSettings({ ...settings, traffic })}
            />
            <label className="checkbox">
              <input
                type="checkbox"
                checked={settings.notifications}
                onChange={(e) =>
                  setSettings({ ...settings, notifications: e.target.checked })
                }
              />
              Desktop notifications on completion and failure
            </label>
            <button className="primary" disabled={busy}>
              Save settings
            </button>
          </form>
        )}
      </Modal>
      <Modal
        error={error}
        title="Keyboard shortcuts"
        open={helpOpen}
        onOpenChange={setHelpOpen}
      >
        <dl>
          <dt>Ctrl / ⌘ + N</dt>
          <dd>Add downloads</dd>
          <dt>Ctrl / ⌘ + F</dt>
          <dd>Search</dd>
          <dt>F1</dt>
          <dd>Open this guide</dd>
          <dt>Escape</dt>
          <dd>Close dialog</dd>
          <dt>Tab / Shift + Tab</dt>
          <dd>Move between controls</dd>
        </dl>
      </Modal>
    </div>
  );
}
