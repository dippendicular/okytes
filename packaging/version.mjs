import { readFile, appendFile } from "node:fs/promises";
import { execFileSync } from "node:child_process";
const tag = process.argv[2];
if (!/^v\d+\.\d+\.\d+$/.test(tag ?? ""))
  throw new Error("Stable semantic version tag required");
const cargo = await readFile("Cargo.toml", "utf8");
const version = cargo
  .split("[workspace.package]")[1]
  ?.match(/^version = "([^"]+)"/m)?.[1];
const config = JSON.parse(
  await readFile("apps/desktop/src-tauri/tauri.conf.json", "utf8"),
);
const frontend = JSON.parse(
  await readFile("apps/desktop/package.json", "utf8"),
);
if (
  version !== tag.slice(1) ||
  config.version !== version ||
  frontend.version !== version
) {
  throw new Error(
    "Tag, Cargo workspace, Tauri and frontend versions must match",
  );
}
if (process.env.GITHUB_ENV) {
  const epoch = execFileSync("git", ["log", "-1", "--format=%ct"], {
    encoding: "utf8",
  }).trim();
  if (!/^\d+$/.test(epoch)) throw new Error("Invalid source commit timestamp");
  await appendFile(process.env.GITHUB_ENV, `SOURCE_DATE_EPOCH=${epoch}\n`);
}
