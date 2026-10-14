import { test } from "node:test";
import assert from "node:assert/strict";
import {
  mkdtemp,
  readFile,
  writeFile,
  mkdir,
  rm,
  stat,
  utimes,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { createRun } from "../linux/create-run.mjs";
import { normalizeTimestamps } from "../timestamps.mjs";

test("self-extractor embeds the exact payload checksum and refuses replacement", async () => {
  const directory = await mkdtemp(join(tmpdir(), "okytes-run-test-"));
  try {
    const payload = Buffer.from([0, 1, 2, 3, 255]);
    await writeFile(join(directory, "payload"), payload);
    await createRun(
      join(directory, "payload"),
      join(directory, "installer.run"),
    );
    const installer = await readFile(join(directory, "installer.run"));
    assert.ok(
      installer.includes(
        Buffer.from(
          `expected=${createHash("sha256").update(payload).digest("hex")}`,
        ),
      ),
    );
    const marker = Buffer.from("\n__OKYTES_PAYLOAD__\n");
    const start = installer.indexOf(marker) + marker.length;
    assert.deepEqual(installer.subarray(start), payload);
    assert.ok(
      installer.indexOf(Buffer.from("Payload checksum mismatch")) <
        installer.indexOf(Buffer.from("tar -xzf")),
    );
    await assert.rejects(
      createRun(join(directory, "payload"), join(directory, "installer.run")),
      { code: "EEXIST" },
    );
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("unsigned dry run includes artifact evidence and checksums for generated documentation", async () => {
  const directory = await mkdtemp(join(tmpdir(), "okytes-release-test-"));
  try {
    const bundles = join(directory, "bundles");
    const output = join(directory, "artifacts");
    await mkdir(bundles);
    await writeFile(join(bundles, "Okytes.exe"), "fixture executable");
    execFileSync(process.execPath, [
      "packaging/collect.mjs",
      bundles,
      output,
      "x86_64-pc-windows-msvc",
    ]);
    execFileSync(process.execPath, ["packaging/release.mjs", output, "v0.1.0"]);
    execFileSync(
      process.execPath,
      ["packaging/sign-release.mjs", output, "v0.1.0", "dippendicular/okytes"],
      {
        env: { ...process.env, TAURI_SIGNING_PRIVATE_KEY: "" },
      },
    );
    const notes = await readFile(join(output, "release-notes.md"), "utf8");
    assert.match(notes, /unsigned-unverified/);
    assert.match(notes, /glibc 2.35/);
    const manifest = await readFile(
      join(output, "CHECKSUMS-unsigned.txt"),
      "utf8",
    );
    for (const line of manifest.trim().split("\n")) {
      const [hash, name] = line.split("  ");
      assert.equal(
        createHash("sha256")
          .update(await readFile(join(output, name)))
          .digest("hex"),
        hash,
      );
    }
    assert.match(manifest, /INSTALLATION.md/);
    assert.match(manifest, /release-notes.md/);
    await assert.rejects(readFile(join(output, "latest.json")), {
      code: "ENOENT",
    });
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("glibc floor gate rejects a newer symbol requirement", async () => {
  const directory = await mkdtemp(join(tmpdir(), "okytes-glibc-test-"));
  try {
    const path = join(directory, "requirements");
    await writeFile(path, "GLIBC_2.17 GLIBC_2.35");
    execFileSync(process.execPath, ["packaging/linux/check-glibc.mjs", path]);
    await writeFile(path, "GLIBC_2.36");
    assert.throws(() =>
      execFileSync(
        process.execPath,
        ["packaging/linux/check-glibc.mjs", path],
        { stdio: "pipe" },
      ),
    );
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("release notices preserve Rust and production frontend licenses", async () => {
  const directory = await mkdtemp(join(tmpdir(), "okytes-notices-test-"));
  try {
    const rust = join(directory, "rust");
    const frontend = join(directory, "frontend");
    const output = join(directory, "notices");
    await mkdir(rust);
    await mkdir(frontend);
    await writeFile(join(rust, "LICENSE-MIT"), "Rust copyright notice");
    await writeFile(join(frontend, "LICENSE"), "Frontend copyright notice");
    const metadata = join(directory, "metadata.json");
    const licenses = join(directory, "frontend.json");
    await writeFile(
      metadata,
      JSON.stringify({
        packages: [
          {
            name: "fixture-rust",
            version: "1.0.0",
            license: "MIT",
            id: "fixture-rust-1.0.0",
            manifest_path: join(rust, "Cargo.toml"),
          },
        ],
      }),
    );
    await writeFile(
      licenses,
      JSON.stringify({
        MIT: [
          { name: "@scope/fixture", versions: ["1.2.3"], paths: [frontend] },
        ],
      }),
    );
    execFileSync(process.execPath, [
      "packaging/notices.mjs",
      metadata,
      licenses,
      output,
    ]);
    const inventory = JSON.parse(
      await readFile(join(output, "inventory.json"), "utf8"),
    );
    assert.equal(inventory.length, 2);
    assert.equal(
      await readFile(
        join(output, inventory[0].directory, "LICENSE-MIT"),
        "utf8",
      ),
      "Rust copyright notice",
    );
    assert.equal(
      await readFile(join(output, inventory[1].directory, "LICENSE"), "utf8"),
      "Frontend copyright notice",
    );
    const sbom = JSON.parse(
      await readFile(join(output, "frontend.cdx.json"), "utf8"),
    );
    assert.equal(sbom.components[0].purl, "pkg:npm/%40scope/fixture@1.2.3");
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("epoch-zero dependency notices become representable in portable ZIPs", async () => {
  const directory = await mkdtemp(join(tmpdir(), "okytes-zip-time-test-"));
  try {
    const notice = join(directory, "LICENSE");
    await writeFile(notice, "Preserved copyright text");
    await utimes(notice, new Date(0), new Date(0));
    assert.equal((await stat(notice)).mtime.getUTCFullYear(), 1970);
    await normalizeTimestamps(directory, 315619200);
    assert.equal((await stat(notice)).mtime.getUTCFullYear(), 1980);
    assert.equal(await readFile(notice, "utf8"), "Preserved copyright text");
    await assert.rejects(normalizeTimestamps(directory, 0), /representable/);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
