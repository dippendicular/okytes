/** Assemble checksums and accurate release notes. Signing is a separate explicit step. */
import { readFile, writeFile, readdir } from "node:fs/promises";
import { createHash } from "node:crypto";
import { join } from "node:path";

const [, , directory, tag] = process.argv;
if (!directory || !/^v\d+\.\d+\.\d+$/.test(tag ?? ""))
  throw new Error("Usage: release.mjs artifact-directory vMAJOR.MINOR.PATCH");
const version = tag.slice(1);
const changelog = await readFile("CHANGELOG.md", "utf8");
const section = changelog.split(`## [${version}]`)[1]?.split(/\n## /)[0];
if (!section?.trim())
  throw new Error(`CHANGELOG.md has no ${version} release entry`);
const names = (await readdir(directory)).sort();
const artifacts = [];
for (const name of names.filter(
  (name) => name.startsWith("artifacts-") && name.endsWith(".json"),
)) {
  artifacts.push(...JSON.parse(await readFile(join(directory, name), "utf8")));
}
if (!artifacts.length) throw new Error("No artifact verification records");
let checksums = "";
for (const name of names) {
  if (!/^[A-Za-z0-9_.-]+$/.test(name))
    throw new Error(`Unsafe artifact name: ${name}`);
  const bytes = await readFile(join(directory, name));
  checksums += `${createHash("sha256").update(bytes).digest("hex")}  ${name}\n`;
}
await writeFile(join(directory, "CHECKSUMS-unsigned.txt"), checksums);
const table = artifacts
  .map(
    (item) =>
      `| ${item.name} | ${item.target} | ${item.signing} | ${item.installation} |`,
  )
  .join("\n");
await writeFile(
  join(directory, "release-notes.md"),
  `# Okytes ${version}\n\n${section.trim()}\n\n## Artifacts\n\nA successful build is not an installation test. Labels below describe recorded evidence. Unverified artifacts are for testing; do not treat them as production-qualified releases.\n\n| Artifact | OS / architecture | Code signing | Install / upgrade / uninstall |\n|---|---|---|---|\n${table}\n\n## Requirements\n\nWindows x64 requires Windows 10/11 and WebView2; ARM64 requires Windows 11 ARM64 and WebView2. Linux x86_64 / ARM64 builds use Ubuntu 22.04 (declared glibc 2.35 floor), GTK 3, WebKitGTK 4.1, fonts, tray libraries and a working X11/Wayland graphics session. The .run installer does not remove these requirements. macOS Intel / Apple Silicon requires macOS 13 or later. See INSTALLATION.md for details.\n\nSigning and notarization are never implied by an updater signature. Checksums without an authenticated manifest only detect damage. Review all unsigned / unverified labels before approving this draft.\n`,
);
await writeFile(
  join(directory, "INSTALLATION.md"),
  await readFile("INSTALLATION.md"),
);
