/** Stage all user-facing binaries together for portable and .run releases. */
import {
  mkdir,
  copyFile,
  chmod,
  readFile,
  writeFile,
  cp,
} from "node:fs/promises";
import { resolve, join } from "node:path";
import { normalizeTimestamps } from "./timestamps.mjs";

const [, , binaryDirectory, outputDirectory, target] = process.argv;
if (!binaryDirectory || !outputDirectory || !target) {
  throw new Error(
    "Usage: node packaging/stage.mjs binary-directory output-directory target-triple",
  );
}
const output = resolve(outputDirectory);
const extension = target.includes("windows") ? ".exe" : "";
await mkdir(output);
await mkdir(join(output, "bin"), { recursive: true });
for (const name of [
  "okytes-desktop",
  "okytes-cli",
  "okytes-daemon",
  "okytes-native-host",
]) {
  const destination = join(output, "bin", name + extension);
  await copyFile(join(resolve(binaryDirectory), name + extension), destination);
  await chmod(destination, 0o755);
}
for (const name of ["LICENSE", "LICENSE-MIT", "LICENSE-APACHE"]) {
  await copyFile(name, join(output, name));
}
await writeFile(
  join(output, "INSTALLATION.md"),
  await readFile("INSTALLATION.md"),
);
await writeFile(join(output, "target.txt"), `${target}\n`);
await cp(
  "apps/desktop/src-tauri/third-party-notices",
  join(output, "third-party-notices"),
  { recursive: true },
);
// Cargo source archives often have epoch-zero notice timestamps, which cannot
// be represented by ZIP. A fixed fallback also keeps local archives stable.
await normalizeTimestamps(
  output,
  Number(process.env.SOURCE_DATE_EPOCH ?? 315619200),
);
