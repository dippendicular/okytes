import { useMemo, useState } from "react";
import { api } from "./api";
import type { CrawlResult } from "../../../crates/core/bindings/CrawlResult";

// Crawling only lists candidates; the user reviews and adds a selection.
export function Crawler({
  destination,
  done,
}: {
  destination: string;
  done: () => void;
}) {
  const [url, setUrl] = useState("");
  const [depth, setDepth] = useState(1);
  const [pages, setPages] = useState(30);
  const [types, setTypes] = useState("zip pdf mp4 mkv mp3 iso exe 7z rar");
  const [sameHost, setSameHost] = useState(true);
  const [result, setResult] = useState<CrawlResult>();
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const groups = useMemo(() => {
    const map = new Map<string, string[]>();
    for (const link of result?.links ?? [])
      map.set(link.extension || "other", [
        ...(map.get(link.extension || "other") ?? []),
        link.url,
      ]);
    return [...map.entries()].sort((a, b) => b[1].length - a[1].length);
  }, [result]);
  const toggle = (urls: string[], on: boolean) =>
    setSelected((old) => {
      const next = new Set(old);
      for (const u of urls) {
        if (on) next.add(u);
        else next.delete(u);
      }
      return next;
    });
  const fail = (e: unknown) => setError(String(e).replace(/^Error:\s*/i, ""));
  return (
    <div className="crawler">
      <form
        onSubmit={(e) => {
          e.preventDefault();
          setBusy(true);
          setError("");
          setResult(undefined);
          setSelected(new Set());
          api
            .crawl({
              url,
              depth,
              same_host: sameHost,
              extensions: types.split(/[\s,]+/).filter(Boolean),
              max_pages: pages,
            })
            .then(setResult)
            .catch(fail)
            .finally(() => setBusy(false));
        }}
      >
        <label>
          Website address
          <input
            type="url"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            placeholder="https://example.com/downloads/"
            required
          />
        </label>
        <div className="crawler-options">
          <label>
            Follow links
            <select
              value={depth}
              onChange={(e) => setDepth(Number(e.target.value))}
            >
              <option value={0}>This page only</option>
              <option value={1}>1 level deep</option>
              <option value={2}>2 levels deep</option>
              <option value={3}>3 levels deep</option>
            </select>
          </label>
          <label>
            Page limit
            <input
              type="number"
              min={1}
              max={200}
              value={pages}
              onChange={(e) => setPages(Number(e.target.value))}
            />
          </label>
        </div>
        <label>
          File types (empty for all)
          <input value={types} onChange={(e) => setTypes(e.target.value)} />
        </label>
        <label className="checkbox">
          <input
            type="checkbox"
            checked={sameHost}
            onChange={(e) => setSameHost(e.target.checked)}
          />
          Only files hosted on this site
        </label>
        <button className="primary" disabled={busy}>
          {busy ? "Crawling… (pages are fetched one at a time)" : "Find files"}
        </button>
      </form>
      {error && (
        <p role="alert" className="alert">
          {error}
        </p>
      )}
      {result && (
        <>
          <p className="muted">
            {result.links.length} files on {result.pages} pages
            {result.skipped_by_robots
              ? ` · ${result.skipped_by_robots} pages skipped by robots.txt`
              : ""}
            {result.truncated ? " · stopped at the limit" : ""}
          </p>
          {groups.map(([ext, urls]) => (
            <details key={ext} open={groups.length <= 3}>
              <summary>
                <label className="checkbox">
                  <input
                    type="checkbox"
                    checked={urls.every((u) => selected.has(u))}
                    onChange={(e) => toggle(urls, e.target.checked)}
                  />
                  .{ext} ({urls.length})
                </label>
              </summary>
              <div className="crawler-links">
                {urls.map((u) => (
                  <label key={u} className="checkbox" title={u}>
                    <input
                      type="checkbox"
                      checked={selected.has(u)}
                      onChange={(e) => toggle([u], e.target.checked)}
                    />
                    <span>{decodeURIComponent(u.split("/").pop() || u)}</span>
                  </label>
                ))}
              </div>
            </details>
          ))}
          <button
            className="primary"
            disabled={busy || !selected.size}
            onClick={() => {
              setBusy(true);
              const urls = [...selected];
              // The add command accepts at most 100 links per batch.
              void (async () => {
                for (let i = 0; i < urls.length; i += 100)
                  await api.add({
                    urls: urls.slice(i, i + 100),
                    destination: destination || null,
                    allow_duplicates: false,
                  });
                done();
              })()
                .catch(fail)
                .finally(() => setBusy(false));
            }}
          >
            Add {selected.size} selected
          </button>
        </>
      )}
    </div>
  );
}
