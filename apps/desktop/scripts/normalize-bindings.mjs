// Keep the changed generated schemas canonical without changing their types.
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

for (const name of [
  "Settings",
  "Job",
  "TrafficPolicy",
  "JobLimit",
  "ScheduleWindow",
]) {
  const path = fileURLToPath(
    new URL(`../../../crates/core/bindings/${name}.ts`, import.meta.url),
  );
  const generated = readFileSync(path, "utf8");
  writeFileSync(
    path,
    generated
      .split(/\r?\n/u)
      .map((line) => line.trimEnd())
      .join("\n"),
  );
}
