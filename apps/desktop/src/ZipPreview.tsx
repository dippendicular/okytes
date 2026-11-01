import { useEffect, useMemo, useState } from "react";
import { api, bytes } from "./api";
import type { ZipListing } from "../../../crates/core/bindings/ZipListing";

const methods: Record<number, string> = {
  0: "Stored",
  8: "Deflate",
  9: "Deflate64",
  12: "BZIP2",
  14: "LZMA",
  93: "Zstandard",
  95: "XZ",
};
// Rendering is capped; the engine already bounds the listing to 5,000 entries.
const SHOWN = 1000;

export function ZipPreview({ url }: { url: string }) {
  const [listing, setListing] = useState<ZipListing>();
  const [error, setError] = useState("");
  const [filter, setFilter] = useState("");
  useEffect(() => {
    let live = true;
    api
      .inspectZip(url)
      .then((value) => live && setListing(value))
      .catch(
        (e: unknown) => live && setError(String(e).replace(/^Error:\s*/i, "")),
      );
    return () => {
      live = false;
    };
  }, [url]);
  const files = useMemo(
    () =>
      (listing?.entries ?? []).filter(
        (entry) =>
          !entry.directory &&
          entry.name.toLowerCase().includes(filter.toLowerCase()),
      ),
    [listing, filter],
  );
  if (error)
    return (
      <p role="alert" className="alert">
        {error}
      </p>
    );
  if (!listing)
    return <p className="muted">Reading the archive's table of contents…</p>;
  const unpacked = listing.entries.reduce((sum, e) => sum + e.size, 0);
  const encrypted = listing.entries.some((e) => e.encrypted);
  return (
    <div className="zip-preview">
      <p className="muted">
        {listing.total_entries} entries · archive {bytes(listing.archive_size)}{" "}
        · listed contents unpack to {bytes(unpacked)}
        {listing.truncated && " (first 5,000 entries shown)"}
        {encrypted && " · contains encrypted files"}
      </p>
      {listing.comment && (
        <p className="error-details-text">{listing.comment}</p>
      )}
      <input
        placeholder="Filter files"
        value={filter}
        onChange={(e) => setFilter(e.target.value)}
        aria-label="Filter archive files"
      />
      <div className="table-scroll">
        <table className="connections">
          <thead>
            <tr>
              <th>Name</th>
              <th>Size</th>
              <th>Packed</th>
              <th>Method</th>
              <th>Modified</th>
            </tr>
          </thead>
          <tbody>
            {files.slice(0, SHOWN).map((entry, index) => (
              <tr key={`${index}-${entry.name}`}>
                <td className="zip-name" title={entry.name}>
                  {entry.encrypted ? "🔒 " : ""}
                  {entry.name}
                </td>
                <td>{bytes(entry.size)}</td>
                <td>{bytes(entry.compressed)}</td>
                <td>{methods[entry.method] ?? `#${entry.method}`}</td>
                <td>{entry.modified ?? "—"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {files.length > SHOWN && (
        <p className="muted">
          Showing {SHOWN} of {files.length} matching files. Filter to narrow the
          list.
        </p>
      )}
    </div>
  );
}
