import { useEffect, useMemo, useRef, useState } from "react";
import {
  api,
  bytes,
  type Action,
  type Connection,
  type Job,
  type Telemetry,
} from "./api";

// Live diagnostics poll at a fixed, modest rate; the engine samples connection
// throughput itself, so nothing here depends on per-chunk events.
const POLL_MS = 400;
type MapState =
  "done" | "active" | "stalled" | "retrying" | "failed" | "pending";
const mapStates: { key: MapState; label: string }[] = [
  { key: "done", label: "Completed" },
  { key: "active", label: "Downloading" },
  { key: "stalled", label: "Stalled" },
  { key: "retrying", label: "Retrying" },
  { key: "failed", label: "Failed" },
  { key: "pending", label: "Pending" },
];
type Range = { start: number; end: number; state: MapState };

export function duration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "—";
  const s = Math.ceil(seconds);
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
}
export const rate = (value: number | null | undefined) =>
  value === null || value === undefined ? "—" : `${bytes(value)}/s`;

export function useTelemetry(id: string) {
  const [telemetry, setTelemetry] = useState<Telemetry>();
  const [unavailable, setUnavailable] = useState(false);
  useEffect(() => {
    let live = true;
    let inflight = false;
    const poll = () => {
      if (inflight || document.hidden) return;
      inflight = true;
      api
        .telemetry(id)
        .then((value) => {
          if (live) setTelemetry(value);
        })
        .catch((error: unknown) => {
          // A daemon-backed desktop has no embedded engine to sample; durable
          // job data from the main list remains authoritative. Other failures
          // are transient and the next poll retries.
          if (live && /telemetry is unavailable/i.test(String(error)))
            setUnavailable(true);
        })
        .finally(() => {
          inflight = false;
        });
    };
    poll();
    const timer = setInterval(poll, POLL_MS);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [id]);
  return {
    telemetry: telemetry?.job.id === id ? telemetry : undefined,
    unavailable,
  };
}

function linkState(state: string): MapState {
  if (state === "stalled" || state === "retrying") return state;
  return "active";
}

// Persisted segments describe byte ownership; live connections only colour the
// remaining part of the segment they currently serve.
function ranges(job: Job, connections: Connection[]): Range[] {
  const total = job.total ?? 0;
  if (job.state === "Completed")
    return [{ start: 0, end: total, state: "done" }];
  const failed = job.state === "Failed" || job.state === "Cancelled";
  if (!job.segments.length) {
    const tail: MapState = connections.length
      ? linkState(connections[0].state)
      : failed
        ? "failed"
        : "pending";
    return [
      { start: 0, end: Math.min(job.bytes, total), state: "done" },
      { start: Math.min(job.bytes, total), end: total, state: tail },
    ];
  }
  const links = new Map(connections.map((c) => [c.segment_start, c]));
  const result: Range[] = [];
  for (const segment of job.segments) {
    const end = segment.end + 1;
    const done = Math.min(end, segment.start + segment.bytes);
    result.push({ start: segment.start, end: done, state: "done" });
    if (done >= end) continue;
    const link = links.get(segment.start);
    const state: MapState = link
      ? linkState(link.state)
      : segment.state === "Failed" || failed
        ? "failed"
        : "pending";
    result.push({ start: done, end, state });
  }
  return result;
}

export function SegmentMap({
  job,
  connections,
}: {
  job: Job;
  connections: Connection[];
}) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const [width, setWidth] = useState(0);
  const parts = useMemo(() => ranges(job, connections), [job, connections]);
  const totals = useMemo(() => {
    const sums = Object.fromEntries(mapStates.map((s) => [s.key, 0])) as Record<
      MapState,
      number
    >;
    for (const part of parts)
      sums[part.state] += Math.max(0, part.end - part.start);
    return sums;
  }, [parts]);

  useEffect(() => {
    const element = canvas.current;
    if (!element) return;
    const observer = new ResizeObserver(([entry]) =>
      setWidth(Math.floor(entry.contentRect.width)),
    );
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    const element = canvas.current;
    const total = job.total;
    if (!element || !width || !total) return;
    const ratio = window.devicePixelRatio || 1;
    const height = 22;
    element.width = Math.round(width * ratio);
    element.height = Math.round(height * ratio);
    const context = element.getContext("2d");
    if (!context) return;
    const style = getComputedStyle(element);
    const colour = (state: MapState) =>
      style.getPropertyValue(`--map-${state}`).trim();
    context.setTransform(ratio, 0, 0, ratio, 0, 0);
    context.fillStyle = colour("pending");
    context.fillRect(0, 0, width, height);
    // One rectangle per range; canvas cost stays flat for highly split files.
    for (const part of parts) {
      if (part.state === "pending" || part.end <= part.start) continue;
      const x = (part.start / total) * width;
      context.fillStyle = colour(part.state);
      context.fillRect(
        x,
        0,
        Math.max(1 / ratio, (part.end / total) * width - x),
        height,
      );
    }
    context.fillStyle = style.getPropertyValue("--map-divider").trim();
    if (job.segments.length <= width / 3)
      for (const segment of job.segments)
        if (segment.start > 0)
          context.fillRect((segment.start / total) * width, 0, 1, height);
  }, [parts, width, job.total, job.segments]);

  if (job.total === null)
    return (
      <div
        className="segment-unknown"
        role="img"
        aria-label="Total size unknown"
      >
        <div className={job.state === "Downloading" ? "striped" : ""} />
        <span>
          Size unknown · {bytes(job.bytes)} received. The server did not report
          a length, so the file downloads as one stream.
        </span>
      </div>
    );
  return (
    <div>
      <canvas
        ref={canvas}
        className="segment-canvas"
        role="img"
        aria-label={`File map: ${mapStates
          .filter((s) => totals[s.key])
          .map((s) => `${s.label} ${bytes(totals[s.key])}`)
          .join(", ")}`}
      />
      <ul className="map-legend">
        {mapStates.map((s) => (
          <li key={s.key} className={totals[s.key] ? "" : "empty"}>
            <i className={`swatch ${s.key}`} />
            {s.label} <span>{bytes(totals[s.key])}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

export function Connections({
  job,
  connections,
}: {
  job: Job;
  connections: Connection[];
}) {
  const index = useMemo(
    () => new Map(job.segments.map((s, i) => [s.start, i + 1])),
    [job.segments],
  );
  if (!connections.length)
    return (
      <p className="muted empty-connections">
        {job.state === "Paused"
          ? "Paused. Connections are closed; durable progress is kept."
          : job.state === "Queued"
            ? "Waiting for a download slot."
            : job.state === "Probing"
              ? "Contacting the server…"
              : job.state === "Verifying"
                ? "Transfer finished. Verifying the file…"
                : job.state === "Downloading"
                  ? "Between requests. Connections reopen automatically."
                  : "No open connections."}
      </p>
    );
  return (
    <div className="table-scroll">
      <table className="connections">
        <thead>
          <tr>
            <th>Conn.</th>
            <th>Segment</th>
            <th>Byte range</th>
            <th>Received</th>
            <th>Speed</th>
            <th>Retries</th>
            <th>Status</th>
          </tr>
        </thead>
        <tbody>
          {connections.map((c) => (
            <tr key={c.id}>
              <td>#{c.id}</td>
              <td title={`Segment starting at byte ${c.segment_start}`}>
                {job.segments.length
                  ? `S${index.get(c.segment_start) ?? "?"}`
                  : "Stream"}
              </td>
              <td>
                {bytes(c.start)}–{c.end === null ? "end" : bytes(c.end + 1)}
              </td>
              <td>{bytes(c.bytes)}</td>
              <td>{c.state === "downloading" ? rate(c.speed) : "—"}</td>
              <td>{c.retries}</td>
              <td>
                <span className={`link-state ${c.state}`}>{c.state}</span>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

export function DownloadDetails({
  job: listed,
  busy,
  act,
}: {
  job: Job;
  busy: boolean;
  act: (job: Job, action: Action) => Promise<void>;
}) {
  const { telemetry, unavailable } = useTelemetry(listed.id);
  const job = telemetry?.job ?? listed;
  const connections = useMemo(() => telemetry?.connections ?? [], [telemetry]);
  const percent = job.total
    ? Math.min(100, (job.bytes / job.total) * 100)
    : null;
  const remaining =
    job.total !== null && job.speed > 0
      ? (job.total - job.bytes) / job.speed
      : null;
  const strongEtag = !!job.etag && /^"/.test(job.etag);
  const resume = ["ftp", "ftps", "sftp"].includes(job.protocol)
    ? "Checked against remote size and modification time"
    : job.protocol.startsWith("hls+") || job.protocol.startsWith("dash+")
      ? "Checked against the VOD plan fingerprint"
      : job.protocol !== "http"
        ? "Handled by the protocol's own resume rules"
        : telemetry?.range_support === false
          ? "No — the server ignores byte ranges; an interruption restarts the file"
          : strongEtag
            ? "Yes — byte ranges with a strong ETag"
            : "Limited — no strong ETag, so resuming restarts from zero";
  const verification =
    telemetry === undefined
      ? "—"
      : telemetry.verified_bytes > 0
        ? `${bytes(telemetry.verified_bytes)} hashed${telemetry.hash_expected ? " · matches the publisher SHA-256" : " · SHA-256 recorded"}`
        : job.state === "Verifying"
          ? "Hashing the completed file…"
          : telemetry.hash_expected
            ? "Pending — publisher SHA-256 is checked after transfer"
            : "Pending — SHA-256 is computed after transfer";

  return (
    <div className="download-details">
      <section className="overview" aria-label="Download overview">
        <div className="overview-head">
          <span className={`status ${job.state.toLowerCase()}`}>
            {job.state}
          </span>
          <strong>
            {percent === null ? bytes(job.bytes) : `${percent.toFixed(1)}%`}
          </strong>
          <div className="overview-controls">
            {["Probing", "Downloading", "Verifying", "Queued"].includes(
              job.state,
            ) && (
              <button
                disabled={busy}
                onClick={() => void act(job, { kind: "Pause" })}
              >
                Pause
              </button>
            )}
            {["Paused", "Failed"].includes(job.state) && (
              <button
                disabled={busy}
                onClick={() => void act(job, { kind: "Resume" })}
              >
                Resume
              </button>
            )}
            {!["Completed", "Cancelled"].includes(job.state) && (
              <button
                className="danger-button"
                disabled={busy}
                onClick={() => void act(job, { kind: "Cancel" })}
              >
                Cancel
              </button>
            )}
          </div>
        </div>
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
        <div className="stat-grid">
          <div>
            <span>Downloaded</span>
            <strong>
              {bytes(job.bytes)} /{" "}
              {job.total === null ? "unknown" : bytes(job.total)}
            </strong>
          </div>
          <div>
            <span>Speed</span>
            <strong>
              {job.state === "Downloading" ? rate(job.speed) : "—"}
            </strong>
          </div>
          <div>
            <span>Average · peak</span>
            <strong>
              {rate(telemetry?.average_speed)} · {rate(telemetry?.peak_speed)}
            </strong>
          </div>
          <div>
            <span>Time left</span>
            <strong>
              {job.state === "Completed"
                ? "Done"
                : remaining === null
                  ? "—"
                  : duration(remaining)}
            </strong>
          </div>
          <div>
            <span>Connections</span>
            <strong>
              {telemetry ? connections.length : job.workers}
              {telemetry?.concurrency_limit
                ? ` / ${telemetry.concurrency_limit} allowed`
                : ""}
            </strong>
          </div>
          <div>
            <span>Retries</span>
            <strong>{job.retries}</strong>
          </div>
        </div>
        <dl>
          <dt>Source</dt>
          <dd>{job.url.replace(/^(hls|dash)\+/, "")}</dd>
          <dt>Destination</dt>
          <dd>{job.destination}</dd>
          <dt>Resume</dt>
          <dd>{resume}</dd>
        </dl>
      </section>
      {job.protocol !== "torrent" && (
        <>
          <section aria-label="Segment progress map">
            <h4>File map</h4>
            <SegmentMap job={job} connections={connections} />
          </section>
          <section aria-label="Connection activity">
            <h4>Connections</h4>
            {unavailable ? (
              <p className="muted">
                Live connection activity is only available when Okytes runs its
                own engine, not through an attached daemon.
              </p>
            ) : (
              <Connections job={job} connections={connections} />
            )}
          </section>
          <section aria-label="Diagnostics">
            <h4>Diagnostics</h4>
            <dl>
              <dt>HTTP response</dt>
              <dd>
                {job.response_status ?? "Not sampled this session"}
                {job.response_latency_ms !== null &&
                  ` · ${job.response_latency_ms.toFixed(0)} ms to headers`}
              </dd>
              <dt>Range support</dt>
              <dd>
                {telemetry?.range_support === undefined ||
                telemetry.range_support === null
                  ? "Not probed this session"
                  : telemetry.range_support
                    ? "Yes — segmented transfer"
                    : "No — single stream"}
              </dd>
              <dt>ETag</dt>
              <dd>{job.etag ?? "None"}</dd>
              <dt>Last-Modified</dt>
              <dd>{telemetry?.last_modified ?? (telemetry ? "None" : "—")}</dd>
              <dt>Concurrency</dt>
              <dd>
                {job.workers} worker task{job.workers === 1 ? "" : "s"}
                {telemetry?.concurrency_limit
                  ? ` · limit ${telemetry.concurrency_limit} (adaptive unless fixed)`
                  : ""}{" "}
                · {job.segments.length} segment
                {job.segments.length === 1 ? "" : "s"}
              </dd>
              <dt>Total retries</dt>
              <dd>{job.retries}</dd>
              <dt>Last error</dt>
              <dd className="error-details-text">{job.error ?? "None"}</dd>
              <dt>Verified</dt>
              <dd>{verification}</dd>
            </dl>
          </section>
        </>
      )}
    </div>
  );
}
