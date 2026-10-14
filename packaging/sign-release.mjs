/** Sign checksums and the exact update descriptor using Tauri's offline-generated key. */
import { readFile, writeFile, readdir, rm } from "node:fs/promises";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { join, resolve } from "node:path";

const [, , directory, tag, repository] = process.argv;
if (
  !directory ||
  !/^v\d+\.\d+\.\d+$/.test(tag ?? "") ||
  !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository ?? "")
) {
  throw new Error(
    "Usage: sign-release.mjs directory vMAJOR.MINOR.PATCH owner/repository",
  );
}
const version = tag.slice(1);
const signer = resolve("apps/desktop/node_modules/@tauri-apps/cli/tauri.js");
function sign(path) {
  execFileSync(
    process.execPath,
    [signer, "signer", "sign", "--app-version", version, resolve(path)],
    {
      env: process.env,
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
}
const signing = Boolean(process.env.TAURI_SIGNING_PRIVATE_KEY);
if (signing) {
  const names = await readdir(directory);
  const records = [];
  for (const name of names.filter(
    (name) => name.startsWith("artifacts-") && name.endsWith(".json"),
  )) {
    records.push(...JSON.parse(await readFile(join(directory, name), "utf8")));
  }
  const platforms = {};
  const targets = {
    "x86_64-pc-windows-msvc": "windows-x86_64",
    "aarch64-pc-windows-msvc": "windows-aarch64",
    "x86_64-unknown-linux-gnu": "linux-x86_64",
    "aarch64-unknown-linux-gnu": "linux-aarch64",
    "x86_64-apple-darwin": "darwin-x86_64",
    "aarch64-apple-darwin": "darwin-aarch64",
  };
  for (const [target, platform] of Object.entries(targets)) {
    const variants = platform.startsWith("windows")
      ? [
          ["nsis", "-setup.exe"],
          ["msi", ".msi"],
        ]
      : platform.startsWith("linux")
        ? [["appimage", ".AppImage"]]
        : [["app", ".app.tar.gz"]];
    for (const [kind, suffix] of variants) {
      const record = records.find(
        (record) =>
          record.target === target &&
          record.name.endsWith(suffix) &&
          record.installation === "verified",
      );
      if (!record) {
        console.log(
          `No qualified ${platform}-${kind} installer; omitted from the update channel.`,
        );
        continue;
      }
      const artifact = record.name;
      const signatureName = `${artifact}.sig`;
      if (!names.includes(artifact) || !names.includes(signatureName))
        throw new Error(
          `Missing signed updater artifact for ${platform}-${kind}`,
        );
      platforms[`${platform}-${kind}`] = {
        url: `https://github.com/${repository}/releases/download/${tag}/${artifact}`,
        signature: (
          await readFile(join(directory, signatureName), "utf8")
        ).trim(),
      };
    }
  }
  if (Object.keys(platforms).length > 0) {
    const descriptor = Buffer.from(
      JSON.stringify({
        version,
        notes: await readFile(join(directory, "release-notes.md"), "utf8"),
        platforms,
      }),
    );
    const descriptorPath = join(directory, "update-descriptor.json");
    await writeFile(descriptorPath, descriptor);
    sign(descriptorPath);
    await writeFile(
      join(directory, "latest.json"),
      JSON.stringify(
        {
          payload: descriptor.toString("base64"),
          signature: (await readFile(descriptorPath + ".sig", "utf8")).trim(),
        },
        null,
        2,
      ) + "\n",
    );
  }
}
const checksumPath = join(
  directory,
  signing ? "CHECKSUMS.txt" : "CHECKSUMS-unsigned.txt",
);
let manifest = "";
for (const name of (await readdir(directory)).sort()) {
  if (name.startsWith("CHECKSUMS")) continue;
  if (!/^[A-Za-z0-9_.-]+$/.test(name))
    throw new Error("Unsafe checksum filename");
  manifest += `${createHash("sha256")
    .update(await readFile(join(directory, name)))
    .digest("hex")}  ${name}\n`;
}
await writeFile(checksumPath, manifest);
if (signing) {
  await rm(join(directory, "CHECKSUMS-unsigned.txt"), { force: true });
  sign(checksumPath);
} else {
  console.log(
    "No signing key: checksum manifest is explicitly unsigned; no update feed was generated.",
  );
}
