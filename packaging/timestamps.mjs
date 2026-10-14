/** Normalize staging timestamps into ZIP's representable range. */
import { readdir, utimes } from "node:fs/promises";
import { join } from "node:path";

export async function normalizeTimestamps(directory, epochSeconds) {
  const timestamp = new Date(epochSeconds * 1000);
  if (
    !Number.isFinite(epochSeconds) ||
    timestamp.getUTCFullYear() < 1980 ||
    timestamp.getUTCFullYear() > 2106
  ) {
    throw new Error(
      "SOURCE_DATE_EPOCH must be representable in a ZIP archive (1980–2106)",
    );
  }
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isSymbolicLink())
      throw new Error("Staging must not contain symbolic links");
    if (entry.isDirectory()) await normalizeTimestamps(path, epochSeconds);
    else if (entry.isFile()) await utimes(path, timestamp, timestamp);
    else throw new Error("Unexpected staging file type");
  }
  await utimes(directory, timestamp, timestamp);
}
