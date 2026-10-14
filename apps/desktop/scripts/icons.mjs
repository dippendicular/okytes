// Generate app and browser branding from the original assets using Tauri's icon tooling.
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { inflateSync } from "node:zlib";
const root = new URL("../../../", import.meta.url);
const require = createRequire(import.meta.url);
const temporary = mkdtempSync(join(tmpdir(), "okytes-icons-"));
function save(path, data) {
  const url = new URL(path, root);
  mkdirSync(new URL(".", url), { recursive: true });
  writeFileSync(url, data);
}
function generate(output, sizes = []) {
  const result = spawnSync(
    process.execPath,
    [
      require.resolve("@tauri-apps/cli/tauri.js"),
      "icon",
      fileURLToPath(
        new URL("assets/images/brand/okytes_icon_transperant.png", root),
      ),
      "--fit",
      "contain",
      "--output",
      output,
      ...sizes.flatMap((size) => ["--png", String(size)]),
    ],
    { stdio: "inherit" },
  );
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error("Brand icon generation failed.");
}
try {
  generate(temporary);
  for (const name of [
    "32x32.png",
    "128x128.png",
    "128x128@2x.png",
    "icon.png",
    "icon.ico",
    "icon.icns",
  ])
    save(
      `apps/desktop/src-tauri/icons/${name}`,
      readFileSync(join(temporary, name)),
    );
  save(
    "apps/desktop/public/favicon.ico",
    readFileSync(join(temporary, "icon.ico")),
  );
  const small = join(temporary, "small");
  generate(small, [16, 32, 48, 64, 128]);
  for (const size of [16, 32, 48, 128])
    save(
      `extensions/shared/icons/${size}.png`,
      readFileSync(join(small, `${size}x${size}.png`)),
    );
  // Decode the generated tray PNG to the RGBA bytes expected by Tauri.
  const png = readFileSync(join(small, "64x64.png")),
    chunks = [];
  if (png[24] !== 8 || png[25] !== 6 || png[28] !== 0)
    throw new Error("Expected RGBA tray PNG.");
  for (let i = 8; i < png.length;) {
    const length = png.readUInt32BE(i);
    if (png.toString("ascii", i + 4, i + 8) === "IDAT")
      chunks.push(png.subarray(i + 8, i + 8 + length));
    i += length + 12;
  }
  const rows = inflateSync(Buffer.concat(chunks)),
    rgba = Buffer.alloc(64 * 64 * 4),
    stride = 64 * 4;
  function paeth(a, b, c) {
    const p = a + b - c,
      x = Math.abs(p - a),
      y = Math.abs(p - b),
      z = Math.abs(p - c);
    return x <= y && x <= z ? a : y <= z ? b : c;
  }
  for (let y = 0; y < 64; y++)
    for (let x = 0; x < stride; x++) {
      const i = y * stride + x,
        filter = rows[y * (stride + 1)];
      if (filter > 4) throw new Error("Unexpected PNG filter.");
      const a = x >= 4 ? rgba[i - 4] : 0,
        b = y ? rgba[i - stride] : 0,
        c = y && x >= 4 ? rgba[i - stride - 4] : 0;
      rgba[i] =
        rows[y * (stride + 1) + x + 1] +
        [0, a, b, Math.floor((a + b) / 2), paeth(a, b, c)][filter];
    }
  save("apps/desktop/src-tauri/icons/icon.rgba", rgba);
  for (const name of [
    "okytes_icon_transperant.png",
    "okytes_logo_transperant.png",
  ])
    save(
      `apps/desktop/public/brand/${name}`,
      readFileSync(new URL(`assets/images/brand/${name}`, root)),
    );
  save(
    "extensions/shared/brand/okytes_logo_transperant.png",
    readFileSync(
      new URL("assets/images/brand/okytes_logo_transperant.png", root),
    ),
  );
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
