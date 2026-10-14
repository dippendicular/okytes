/** Convert Cargo's locked package inventory to a CycloneDX 1.6 JSON SBOM. */
import { readFile, writeFile } from "node:fs/promises";
const [, , metadataPath, outputPath] = process.argv;
if (!metadataPath || !outputPath)
  throw new Error("Usage: sbom.mjs cargo-metadata.json output.cdx.json");
const metadata = JSON.parse(await readFile(metadataPath, "utf8"));
await writeFile(
  outputPath,
  JSON.stringify(
    {
      bomFormat: "CycloneDX",
      specVersion: "1.6",
      version: 1,
      components: metadata.packages.map((pkg) => ({
        type: "library",
        name: pkg.name,
        version: pkg.version,
        "bom-ref": pkg.id,
        ...(pkg.source?.startsWith("registry+")
          ? { purl: `pkg:cargo/${pkg.name}@${pkg.version}` }
          : {}),
        ...(pkg.license ? { licenses: [{ expression: pkg.license }] } : {}),
        ...(pkg.repository
          ? { externalReferences: [{ type: "vcs", url: pkg.repository }] }
          : {}),
        properties: [
          {
            name: "okytes:source",
            value: pkg.source ?? "workspace-or-reviewed-vendor-patch",
          },
        ],
      })),
      dependencies: (metadata.resolve?.nodes ?? []).map((node) => ({
        ref: node.id,
        dependsOn: node.dependencies,
      })),
    },
    null,
    2,
  ) + "\n",
);
