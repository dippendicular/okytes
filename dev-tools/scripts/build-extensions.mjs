#!/usr/bin/env node
// One-click browser extension build for Windows, Linux and macOS.
// Produces target/release/extension_release with Chrome/Chromium and Firefox
// packages. Requires Node.js 24 and pnpm 11; no other tools (ZIP files are
// written with Node's built-in zlib).
//
// Usage:  node dev-tools/scripts/build-extensions.mjs [--skip-install] [--skip-checks]
import { spawnSync } from "node:child_process";
import { readFile, readdir, rm, mkdir, writeFile, stat } from "node:fs/promises";
import { join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { deflateRawSync, crc32 } from "node:zlib";

const root = fileURLToPath(new URL("../../", import.meta.url));
const extensions = join(root, "extensions");
const out = join(root, "target", "release", "extension_release");
const args = new Set(process.argv.slice(2));
const step = (name) => console.log(`\n\x1b[36m==> ${name}\x1b[0m`);

function run(...command) {
  // pnpm is a .cmd shim on Windows and needs a shell there. Every command in
  // this script is a fixed string, so joining it into one line is safe.
  const windows = process.platform === "win32";
  const result = windows
    ? spawnSync(command.join(" "), { cwd: extensions, stdio: "inherit", shell: true })
    : spawnSync(command[0], command.slice(1), { cwd: extensions, stdio: "inherit" });
  if (result.status !== 0) throw new Error(`Command failed: ${command.join(" ")}`);
}

async function files(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const nested = await Promise.all(
    entries.map((entry) => {
      const path = join(directory, entry.name);
      return entry.isDirectory() ? files(path) : [path];
    }),
  );
  return nested.flat().sort();
}

// Minimal ZIP writer (deflate, no data descriptors). Fixed timestamps make
// repeated builds of the same sources produce identical archives.
async function zip(source, destination) {
  const local = [];
  const central = [];
  let offset = 0;
  for (const path of await files(source)) {
    const name = Buffer.from(relative(source, path).split(sep).join("/"));
    const data = await readFile(path);
    const packed = deflateRawSync(data, { level: 9 });
    const crc = crc32(data);
    const header = Buffer.alloc(30);
    header.writeUInt32LE(0x04034b50, 0);
    header.writeUInt16LE(20, 4);
    header.writeUInt16LE(0x0800, 6); // UTF-8 names
    header.writeUInt16LE(8, 8); // deflate
    header.writeUInt16LE(0, 10);
    header.writeUInt16LE(0x21, 12); // 1980-01-01
    header.writeUInt32LE(crc, 14);
    header.writeUInt32LE(packed.length, 18);
    header.writeUInt32LE(data.length, 22);
    header.writeUInt16LE(name.length, 26);
    local.push(header, name, packed);
    const entry = Buffer.alloc(46);
    entry.writeUInt32LE(0x02014b50, 0);
    entry.writeUInt16LE(20, 4);
    entry.writeUInt16LE(20, 6);
    entry.writeUInt16LE(0x0800, 8);
    entry.writeUInt16LE(8, 10);
    entry.writeUInt16LE(0, 12);
    entry.writeUInt16LE(0x21, 14);
    entry.writeUInt32LE(crc, 16);
    entry.writeUInt32LE(packed.length, 20);
    entry.writeUInt32LE(data.length, 24);
    entry.writeUInt16LE(name.length, 28);
    entry.writeUInt32LE(offset, 42);
    central.push(entry, name);
    offset += header.length + name.length + packed.length;
  }
  const directory = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(central.length / 2, 8);
  end.writeUInt16LE(central.length / 2, 10);
  end.writeUInt32LE(directory.length, 12);
  end.writeUInt32LE(offset, 16);
  await writeFile(destination, Buffer.concat([...local, directory, end]));
}

const version = JSON.parse(await readFile(join(extensions, "package.json"), "utf8")).version;

if (!args.has("--skip-install")) {
  step("Installing extension dependencies");
  run("pnpm", "install", "--frozen-lockfile");
}
if (!args.has("--skip-checks")) {
  step("Type-checking and linting");
  run("pnpm", "typecheck");
  run("pnpm", "lint");
}
step("Building Chromium and Firefox extensions");
run("pnpm", "build");

step(`Packaging into ${relative(root, out)}`);
await rm(out, { recursive: true, force: true });
await mkdir(out, { recursive: true });
const chrome = join(out, `Okytes-Chrome-extension-${version}.zip`);
const firefox = join(out, `Okytes-Firefox-extension-${version}.zip`);
await zip(join(extensions, "dist", "chromium"), chrome);
await zip(join(extensions, "dist", "firefox"), firefox);
// Firefox installs .xpi files; it is the same ZIP with another extension.
await writeFile(firefox.replace(/\.zip$/, ".xpi"), await readFile(firefox));
for (const name of ["INSTALLATION.md"])
  await writeFile(join(out, `BROWSER-${name}`), await readFile(join(extensions, name)));

for (const name of (await readdir(out)).sort())
  console.log(`  ${name}  ${((await stat(join(out, name))).size / 1024).toFixed(0)} KiB`);
console.log(
  "\x1b[32mDone.\x1b[0m Chrome: extract the ZIP and use Load unpacked. Firefox: unsigned; load it from about:debugging, or sign it with web-ext for a permanent install.",
);
