// Reproducible product icon: a download arrow drawn as pixels, without font dependencies.
import { deflateSync } from "node:zlib";
import { mkdirSync, writeFileSync } from "node:fs";
const size = 64;
const rgba = Buffer.alloc((size * 4 + 1) * size);
for (let y = 0; y < size; y++)
  for (let x = 0; x < size; x++) {
    const white =
      (x >= 29 && x <= 34 && y >= 14 && y <= 40) ||
      (y >= 31 &&
        y <= 45 &&
        Math.abs(x - 31.5) <= 45 - y &&
        Math.abs(x - 31.5) >= 39 - y) ||
      (y >= 49 && y <= 52 && x >= 17 && x <= 46);
    const offset = y * (size * 4 + 1) + 1 + x * 4;
    rgba.set(white ? [255, 255, 255, 255] : [36, 93, 212, 255], offset);
  }
function crc(data) {
  let n = 0xffffffff;
  for (const b of data) {
    n ^= b;
    for (let i = 0; i < 8; i++) n = (n >>> 1) ^ (n & 1 ? 0xedb88320 : 0);
  }
  return (n ^ 0xffffffff) >>> 0;
}
function chunk(type, data) {
  const name = Buffer.from(type);
  const result = Buffer.alloc(data.length + 12);
  result.writeUInt32BE(data.length);
  name.copy(result, 4);
  data.copy(result, 8);
  result.writeUInt32BE(crc(Buffer.concat([name, data])), data.length + 8);
  return result;
}
const header = Buffer.alloc(13);
header.writeUInt32BE(size);
header.writeUInt32BE(size, 4);
header[8] = 8;
header[9] = 6;
const png = Buffer.concat([
  Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
  chunk("IHDR", header),
  chunk("IDAT", deflateSync(rgba)),
  chunk("IEND", Buffer.alloc(0)),
]);
const ico = Buffer.alloc(22);
ico.writeUInt16LE(1, 2);
ico.writeUInt16LE(1, 4);
ico[6] = size;
ico[7] = size;
ico.writeUInt16LE(1, 10);
ico.writeUInt16LE(32, 12);
ico.writeUInt32LE(png.length, 14);
ico.writeUInt32LE(22, 18);
const dir = new URL("../src-tauri/icons/", import.meta.url);
mkdirSync(dir, { recursive: true });
writeFileSync(new URL("icon.png", dir), png);
writeFileSync(new URL("icon.ico", dir), Buffer.concat([ico, png]));
writeFileSync(
  new URL("icon.rgba", dir),
  Buffer.concat(
    Array.from({ length: size }, (_, y) =>
      rgba.subarray(y * (size * 4 + 1) + 1, (y + 1) * (size * 4 + 1)),
    ),
  ),
);
