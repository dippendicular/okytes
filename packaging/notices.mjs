/** Preserve available upstream notices and emit a production frontend SBOM. */
import {
  readFile,
  writeFile,
  mkdir,
  readdir,
  copyFile,
  lstat,
} from "node:fs/promises";
import { dirname, join } from "node:path";
import { createHash } from "node:crypto";
const [, , cargoPath, frontendPath, output] = process.argv;
if (!cargoPath || !frontendPath || !output)
  throw new Error(
    "Usage: notices.mjs cargo-metadata.json frontend-licenses.json output-directory",
  );
const metadata = JSON.parse(await readFile(cargoPath, "utf8"));
const frontend = JSON.parse(await readFile(frontendPath, "utf8"));
await mkdir(output, { recursive: true });
const inventory = [];
async function preserve(name, version, root, license, id) {
  const folder = `${name.replaceAll(/[^A-Za-z0-9_.-]/g, "_")}-${version}-${createHash("sha256").update(id).digest("hex").slice(0, 8)}`;
  const files = (await readdir(root)).filter((name) =>
    /^(license|licence|copying|copyright|notice)([._-]|$)/i.test(name),
  );
  const copied = [];
  for (const name of files) {
    const source = join(root, name);
    const info = await lstat(source);
    if (!info.isFile() || info.size > 4 * 1024 * 1024) continue;
    await mkdir(join(output, folder), { recursive: true });
    await copyFile(source, join(output, folder, name));
    copied.push(name);
  }
  inventory.push({
    name,
    version,
    license,
    notices: copied,
    directory: folder,
  });
}
for (const pkg of metadata.packages) {
  await preserve(
    pkg.name,
    pkg.version,
    dirname(pkg.manifest_path),
    pkg.license,
    pkg.id,
  );
}
const components = [];
for (const [license, packages] of Object.entries(frontend)) {
  for (const pkg of packages) {
    for (let index = 0; index < pkg.versions.length; index++) {
      const version = pkg.versions[index];
      const root = pkg.paths[index] ?? pkg.paths[0];
      const purl = `pkg:npm/${pkg.name.replace("@", "%40")}@${version}`;
      await preserve(pkg.name, version, root, license, purl);
      components.push({
        type: "library",
        name: pkg.name,
        version,
        purl,
        licenses: [{ expression: license }],
      });
    }
  }
}
await writeFile(
  join(output, "inventory.json"),
  JSON.stringify(inventory, null, 2) + "\n",
);
await writeFile(
  join(output, "frontend.cdx.json"),
  JSON.stringify(
    { bomFormat: "CycloneDX", specVersion: "1.6", version: 1, components },
    null,
    2,
  ) + "\n",
);
await writeFile(
  join(output, "README.txt"),
  "Upstream notices preserved from locked package sources. inventory.json records packages with no root notice file; those require review before production qualification. Workspace packages are covered by the root project licenses.\n",
);
