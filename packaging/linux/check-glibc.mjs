import { readFile } from "node:fs/promises";
const text = await readFile(process.argv[2], "utf8");
const versions = [...text.matchAll(/GLIBC_(\d+)\.(\d+)/g)].map((match) => [
  Number(match[1]),
  Number(match[2]),
]);
if (!versions.length)
  throw new Error("No glibc requirements found in the built ELF");
if (versions.some(([major, minor]) => major > 2 || (major === 2 && minor > 35)))
  throw new Error("Built artifact exceeds the declared glibc 2.35 floor");
