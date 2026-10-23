/** Stage real compiled companions for Tauri's native sidecar packaging. */
import { copyFile, mkdir, chmod } from "node:fs/promises";
import { resolve, join } from "node:path";
const [, , directory, target, outputDirectory] = process.argv;
if (!directory || !/^[a-zA-Z0-9_-]+$/.test(target ?? ""))
  throw new Error(
    "Usage: companions.mjs compiled-binary-directory target-triple",
  );
const output = resolve(outputDirectory ?? "apps/desktop/src-tauri/binaries");
await mkdir(output, { recursive: true });
const suffix = target.includes("windows") ? ".exe" : "";
for (const name of ["okytes-cli", "okytes-daemon", "okytes-native-host"]) {
  const destination = join(output, `${name}-${target}${suffix}`);
  await copyFile(join(resolve(directory), `${name}${suffix}`), destination);
  await chmod(destination, 0o755);
}
