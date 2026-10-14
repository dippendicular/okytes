// Restore ignored torrent sources from checksum-pinned crates and reviewed patches.
// Existing sources are verified and never overwritten.
import { createHash } from "node:crypto";
import {
  readFile,
  writeFile,
  mkdir,
  mkdtemp,
  access,
  cp,
  rm,
} from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
const root = fileURLToPath(new URL("../", import.meta.url));
const patches = join(root, "scripts/vendor-patches");
const manifest = JSON.parse(
  await readFile(join(patches, "manifest.json"), "utf8"),
);
const cacheIndex = process.argv.indexOf("--cache");
const cache = cacheIndex < 0 ? null : resolve(process.argv[cacheIndex + 1]);
const verifyArchives = process.argv.includes("--verify-archives");
const digest = (data) => createHash("sha256").update(data).digest("hex");
async function exists(path) {
  try {
    await access(path);
    return true;
  } catch {
    return false;
  }
}
function run(command, args, cwd) {
  const result = spawnSync(command, args, {
    cwd,
    stdio: "inherit",
    windowsHide: true,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} failed`);
}
async function verify(target, item) {
  for (const [path, hash] of Object.entries(item.files)) {
    const contents = await readFile(join(target, path), "utf8");
    if (digest(contents.replace(/\r\n/g, "\n")) !== hash)
      throw new Error(
        `Existing ${item.name}/${path} differs from the reviewed patch set; preserve and review local edits before rebuilding.`,
      );
  }
}
await mkdir(join(root, "vendor"), { recursive: true });
for (const item of manifest) {
  const target = join(root, "vendor", item.name);
  if ((await exists(target)) && !verifyArchives) {
    await verify(target, item);
    console.log(`Verified ${item.name} ${item.version}`);
    continue;
  }
  const temporary = await mkdtemp(join(tmpdir(), "okytes-vendor-"));
  try {
    const archiveName = `${item.name}-${item.version}.crate`;
    let bytes;
    if (cache) {
      bytes = await readFile(join(cache, archiveName));
    } else {
      const response = await fetch(
        `https://static.crates.io/crates/${item.name}/${archiveName}`,
        { signal: AbortSignal.timeout(60000) },
      );
      if (!response.ok)
        throw new Error(`Unable to fetch ${archiveName}: ${response.status}`);
      bytes = Buffer.from(await response.arrayBuffer());
    }
    if (digest(bytes) !== item.sha256)
      throw new Error(`Archive checksum mismatch: ${archiveName}`);
    const archive = join(temporary, archiveName),
      source = join(temporary, "source");
    await mkdir(source);
    await writeFile(archive, bytes);
    run(
      "tar",
      ["-xzf", archive, "--strip-components=1", "-C", source],
      temporary,
    );
    const patch = join(temporary, "changes.patch");
    await writeFile(
      patch,
      (await readFile(join(patches, `${item.name}.patch`), "utf8")).replace(
        /\r\n/g,
        "\n",
      ),
    );
    if ((await readFile(patch)).length)
      run(
        "git",
        ["-c", "core.autocrlf=false", "apply", "--whitespace=nowarn", patch],
        source,
      );
    await verify(source, item);
    if (!verifyArchives)
      await cp(source, target, {
        recursive: true,
        errorOnExist: true,
        force: false,
      });
    console.log(
      `${verifyArchives ? "Verified archive reconstruction" : "Prepared"} ${item.name} ${item.version}`,
    );
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
}
