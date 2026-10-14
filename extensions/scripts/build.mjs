import { build } from "esbuild";
import { cp, mkdir } from "node:fs/promises";
for (const browser of ["chromium", "firefox"]) {
  const outdir = `dist/${browser}`;
  await mkdir(outdir, { recursive: true });
  await build({
    entryPoints: [
      "shared/background.ts",
      "shared/popup.ts",
      "shared/options.ts",
      "shared/bypass.ts",
    ],
    bundle: true,
    outdir,
    format: "iife",
    target: "es2022",
  });
  for (const directory of ["brand", "icons"])
    await cp(`shared/${directory}`, `${outdir}/${directory}`, { recursive: true });
  await cp(`${browser}/manifest.json`, `${outdir}/manifest.json`);
  for (const file of ["popup.html", "options.html", "style.css"])
    await cp(`shared/${file}`, `${outdir}/${file}`);
}
