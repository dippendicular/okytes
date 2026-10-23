import { useCallback, useEffect, useRef, useState } from "react";
import {
  createQuick,
  type QuickPeerRequest,
} from "../../../../packages/sdk-typescript/src/quick";
import { bytes } from "../api";
import type { components } from "../../../../packages/sdk-typescript/src/schema";

type Snapshot = components["schemas"]["Snapshot"];
type Action = components["schemas"]["Action"];
const client = createQuick();

export function Quick() {
  const [state, setState] = useState<"loading" | "pair" | "ready">("loading");
  const [code, setCode] = useState("");
  const [urls, setUrls] = useState("");
  const [destination, setDestination] = useState("");
  const [duplicates, setDuplicates] = useState(false);
  const [snapshot, setSnapshot] = useState<Snapshot>();
  const [peerRequests, setPeerRequests] = useState<QuickPeerRequest[]>([]);
  const [offset, setOffset] = useState(0);
  const [error, setError] = useState("");
  const [connected, setConnected] = useState(true);
  const [busy, setBusy] = useState(false);
  const refreshing = useRef(false);
  const refresh = useCallback(async () => {
    if (refreshing.current) return;
    refreshing.current = true;
    try {
      const [downloads, peers] = await Promise.all([
        client.list(offset),
        client.peerRequests(),
      ]);
      setSnapshot(downloads);
      setPeerRequests(peers);
      setConnected(true);
    } catch (cause) {
      setConnected(false);
      if (
        cause instanceof Error &&
        cause.message.includes("session has expired")
      ) {
        setState("pair");
        setError(cause.message);
      }
    } finally {
      refreshing.current = false;
    }
  }, [offset]);

  useEffect(() => {
    let live = true;
    void client
      .session()
      .then(() => {
        if (live) setState("ready");
      })
      .catch(() => {
        if (live) setState("pair");
      });
    return () => {
      live = false;
    };
  }, []);
  useEffect(() => {
    if (state !== "ready") return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const schedule = () => {
      if (!timer)
        timer = setTimeout(() => {
          timer = undefined;
          void refresh();
        }, 200);
    };
    void refresh();
    const interval = setInterval(() => {
      void refresh();
    }, 5000);
    void (async () => {
      while (!controller.signal.aborted) {
        try {
          await client.events(schedule, controller.signal);
        } catch {
          if (!controller.signal.aborted) setConnected(false);
        }
        if (controller.signal.aborted) break;
        await new Promise<void>((resolve) => {
          const stop = () => {
            clearTimeout(retry);
            resolve();
          };
          const retry = setTimeout(() => {
            controller.signal.removeEventListener("abort", stop);
            resolve();
          }, 3000);
          controller.signal.addEventListener("abort", stop, { once: true });
        });
        if (!controller.signal.aborted) schedule();
      }
    })();
    return () => {
      controller.abort();
      clearInterval(interval);
      if (timer) clearTimeout(timer);
    };
  }, [state, refresh]);

  async function run(operation: () => Promise<unknown>) {
    setBusy(true);
    setError("");
    try {
      await operation();
      await refresh();
    } catch (cause) {
      setError(
        cause instanceof Error
          ? cause.message
          : "The service could not complete this request.",
      );
    } finally {
      setBusy(false);
    }
  }
  async function control(id: string, action: Action) {
    await run(() => client.control(id, action));
  }

  return (
    <main>
      <header>
        <img src="/quick/logo.png" alt="Okytes" width="240" />
        <h1>Quick Download</h1>
        <p>Your local download queue, in your browser.</p>
      </header>
      {error && (
        <p role="alert" className="alert">
          {error}
        </p>
      )}
      {state === "loading" && (
        <p role="status">Connecting to your local service…</p>
      )}
      {state === "pair" && (
        <section aria-label="Authorize this browser">
          <h2>Connect this browser</h2>
          <p>
            Pair each new or reloaded tab. Your queue stays with the daemon;
            this page keeps its authorization proof only in memory.
          </p>
          <p>
            Request a one-use pairing code from the trusted Okytes CLI. Enter it
            here within two minutes. Keep your permanent daemon token private.
          </p>
          <form
            onSubmit={(event) => {
              event.preventDefault();
              void run(async () => {
                await client.session(code.trim());
                setCode("");
                setState("ready");
              });
            }}
          >
            <label htmlFor="code">Pairing code</label>
            <input
              id="code"
              autoComplete="off"
              spellCheck={false}
              maxLength={64}
              required
              value={code}
              onChange={(event) => setCode(event.target.value)}
            />
            <button disabled={busy}>Connect</button>
          </form>
          <section aria-label="LAN Share transfers">
            <h2>LAN Share</h2>
            {peerRequests.length === 0 ? (
              <p>No peer transfers yet.</p>
            ) : (
              peerRequests.map((request) => (
                <article key={request.id}>
                  <strong>
                    {request.direction === "inbound" ? "Incoming" : "Outgoing"}{" "}
                    · {request.state}
                  </strong>
                  <p>
                    {request.files_completed} files · {bytes(request.bytes)} of{" "}
                    {bytes(request.total)}
                  </p>
                  <progress
                    value={request.bytes}
                    max={Math.max(1, request.total)}
                  />
                  {request.failure && <p className="alert">{request.failure}</p>}
                  <div className="actions">
                    {request.direction === "inbound" &&
                      request.state === "awaiting_approval" && (
                        <>
                          <button
                            disabled={busy}
                            onClick={() =>
                              void run(() =>
                                client.peerDecide(
                                  request.id,
                                  request.revision,
                                  "accept",
                                ),
                              )
                            }
                          >
                            Accept to default folder
                          </button>
                          <button
                            disabled={busy}
                            onClick={() =>
                              void run(() =>
                                client.peerDecide(
                                  request.id,
                                  request.revision,
                                  "reject",
                                ),
                              )
                            }
                          >
                            Reject
                          </button>
                        </>
                      )}
                    {["negotiating", "transferring"].includes(request.state) && (
                      <button
                        disabled={busy}
                        onClick={() =>
                          void run(() =>
                            client.peerDecide(request.id, request.revision, "pause"),
                          )
                        }
                      >
                        Pause
                      </button>
                    )}
                    {["paused", "interrupted", "failed"].includes(request.state) && (
                      <button
                        disabled={busy}
                        onClick={() =>
                          void run(() =>
                            client.peerDecide(request.id, request.revision, "retry"),
                          )
                        }
                      >
                        Retry
                      </button>
                    )}
                  </div>
                </article>
              ))
            )}
          </section>
        </section>
      )}
      {state === "ready" && (
        <>
          {!connected && (
            <p role="status" className="alert">
              Service unavailable or session expired. Check that the daemon is
              running. Reconnect this browser if your session has expired. This
              page cannot start a stopped native service.
            </p>
          )}
          <form
            onSubmit={(event) => {
              event.preventDefault();
              const values = urls.split(/\s+/).filter(Boolean);
              if (!values.length || values.length > 10) {
                setError("Paste between one and ten direct HTTP/HTTPS URLs.");
                return;
              }
              try {
                for (const value of values) {
                  const url = new URL(value);
                  if (
                    !["http:", "https:"].includes(url.protocol) ||
                    url.username ||
                    url.password ||
                    value.length > 8192
                  )
                    throw new Error();
                }
              } catch {
                setError(
                  "Use direct HTTP/HTTPS links without embedded login details.",
                );
                return;
              }
              void run(async () => {
                await client.add({
                  urls: values,
                  destination: destination || null,
                  allow_duplicates: duplicates,
                });
                setUrls("");
                setDuplicates(false);
              });
            }}
            onDragOver={(event) => event.preventDefault()}
            onDrop={(event) => {
              event.preventDefault();
              const text =
                event.dataTransfer.getData("text/uri-list") ||
                event.dataTransfer.getData("text/plain");
              setUrls(
                text
                  .split(/\r?\n/)
                  .filter((line) => !line.startsWith("#"))
                  .join("\n")
                  .slice(0, 81930),
              );
            }}
          >
            <label htmlFor="urls">Download links</label>
            <textarea
              id="urls"
              rows={3}
              maxLength={81930}
              required
              placeholder="Paste or drop a direct download link"
              value={urls}
              onChange={(event) => setUrls(event.target.value)}
            />
            <button
              type="button"
              disabled={busy}
              onClick={() => {
                void navigator.clipboard
                  ?.readText()
                  .then((text) => setUrls(text.slice(0, 81930)))
                  .catch(() =>
                    setError(
                      "Clipboard access is unavailable. Paste your link into the field.",
                    ),
                  );
                if (!navigator.clipboard)
                  setError(
                    "Clipboard access is unavailable. Paste your link into the field.",
                  );
              }}
            >
              Paste from clipboard
            </button>
            <label htmlFor="destination">
              Destination within your configured download folder (optional)
            </label>
            <input
              id="destination"
              value={destination}
              maxLength={4096}
              onChange={(event) => setDestination(event.target.value)}
            />
            <label className="check">
              <input
                type="checkbox"
                checked={duplicates}
                onChange={(event) => setDuplicates(event.target.checked)}
              />
              Allow another copy of URLs already in my queue
            </label>
            <button className="primary" disabled={busy}>
              Download
            </button>
          </form>
          <section aria-label="Download queue">
            <h2>Your downloads</h2>
            {!snapshot && <p role="status">Loading your queue…</p>}
            {snapshot?.jobs.length === 0 && (
              <p>No downloads yet. Paste a link to get started.</p>
            )}
            <ul>
              {snapshot?.jobs.map((job) => (
                <li key={job.id}>
                  <h3>{job.name}</h3>
                  <p>
                    {job.state} · {bytes(job.bytes)}
                    {job.total == null
                      ? " downloaded"
                      : ` / ${bytes(job.total)}`}
                    {job.speed > 0 &&
                      ` · ${bytes(job.speed)}/s${job.total ? ` · ${Math.max(0, Math.ceil((job.total - job.bytes) / job.speed))}s remaining` : ""}`}
                    {job.retries > 0 && ` · ${job.retries} retries`}
                  </p>
                  <progress
                    aria-label={`${job.name} progress`}
                    value={job.total == null ? undefined : job.bytes}
                    max={job.total == null ? undefined : Math.max(1, job.total)}
                  />
                  <div className="controls">
                    {[
                      "Queued",
                      "Probing",
                      "Downloading",
                      "Verifying",
                      "Seeding",
                    ].includes(job.state) && (
                      <button
                        disabled={busy}
                        onClick={() => {
                          void control(job.id, { kind: "Pause" });
                        }}
                      >
                        Pause {job.name}
                      </button>
                    )}
                    {["Paused", "Failed"].includes(job.state) && (
                      <button
                        disabled={busy}
                        onClick={() => {
                          void control(job.id, { kind: "Resume" });
                        }}
                      >
                        Resume {job.name}
                      </button>
                    )}
                    {!["Completed", "Cancelled"].includes(job.state) && (
                      <button
                        disabled={busy}
                        onClick={() => {
                          void control(job.id, { kind: "Cancel" });
                        }}
                      >
                        Cancel {job.name}
                      </button>
                    )}
                    <label>
                      Priority for {job.name}
                      <select
                        value={job.priority}
                        disabled={busy}
                        onChange={(event) => {
                          void control(job.id, {
                            kind: "Priority",
                            value: Number(event.target.value),
                          });
                        }}
                      >
                        <option value={job.priority}>{job.priority}</option>
                        {[-100, 0, 100]
                          .filter((value) => value !== job.priority)
                          .map((value) => (
                            <option key={value} value={value}>
                              {value}
                            </option>
                          ))}
                      </select>
                    </label>
                  </div>
                </li>
              ))}
            </ul>
            <button
              disabled={busy || offset === 0}
              onClick={() => setOffset(0)}
            >
              First page
            </button>
            <button
              disabled={busy || snapshot?.next == null}
              onClick={() => {
                if (snapshot?.next != null) setOffset(snapshot.next);
              }}
            >
              More downloads
            </button>
          </section>
          <button
            disabled={busy}
            onClick={() => {
              void run(async () => {
                await client.logout();
                setSnapshot(undefined);
                setState("pair");
              });
            }}
          >
            Disconnect this browser
          </button>
        </>
      )}
      <footer>
        <p>
          Bookmark this page for manual downloads. Transfers continue while the
          native daemon runs, even after you close this tab. Open completed
          files and detailed diagnostics in the desktop app.
        </p>
        <p>
          This page does not intercept website clicks. Links that need browser
          cookies, headers or expired site tokens may require the extension or a
          fresh direct URL. The loopback address on a phone points to that
          phone, not your computer.
        </p>
      </footer>
    </main>
  );
}
