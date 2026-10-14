/** Preserve signing/verification labels while collecting a native runner's artifacts. */
import { cp, mkdir, readdir, stat, writeFile } from "node:fs/promises";
import { join, basename } from "node:path";

const [, , directory, destination, target, signing = "unsigned"] = process.argv;
if (
  !directory ||
  !destination ||
  !target ||
  !["unsigned", "signed", "signing-unverified"].includes(signing)
) {
  throw new Error(
    "Usage: collect.mjs bundle-directory destination target [signed|unsigned]",
  );
}
await mkdir(destination, { recursive: true });
const artifacts = [];
async function collect(path) {
  for (const entry of await readdir(path, { withFileTypes: true })) {
    const source = join(path, entry.name);
    if (entry.isDirectory()) {
      if (!entry.name.endsWith(".app")) await collect(source);
      continue;
    }
    if (
      !/\.(exe|msi|deb|rpm|AppImage|dmg|run|zip|tar\.gz|sig)$/.test(entry.name)
    )
      continue;
    const name = `Okytes-${target}-${signing}-unverified-${basename(source).replaceAll(" ", "-")}`;
    await cp(source, join(destination, name), {
      errorOnExist: true,
      force: false,
    });
    artifacts.push({
      name,
      target,
      signing,
      installation: "unverified",
      size: (await stat(source)).size,
    });
  }
}
await collect(directory);
if (artifacts.length === 0) throw new Error("No bundles were produced");
await writeFile(
  join(destination, `artifacts-${target}.json`),
  JSON.stringify(artifacts, null, 2) + "\n",
);
