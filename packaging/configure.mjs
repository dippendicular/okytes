import { writeFile } from "node:fs/promises";
const [, , platform, output] = process.argv;
if (!["linux", "windows", "macos"].includes(platform) || !output)
  throw new Error("Platform and output required");
const signing =
  process.env.UPDATE_SIGNING_ENABLED === "true" ||
  Boolean(process.env.TAURI_SIGNING_PRIVATE_KEY);
if (signing && !process.env.OKYTES_UPDATE_PUBLIC_KEY)
  throw new Error("Updater signing key supplied without embedded public key");
const windows =
  platform === "windows" && process.env.WINDOWS_CERTIFICATE_THUMBPRINT
    ? {
        certificateThumbprint: process.env.WINDOWS_CERTIFICATE_THUMBPRINT,
        digestAlgorithm: "sha256",
        timestampUrl: "http://timestamp.digicert.com",
      }
    : undefined;
await writeFile(
  output,
  JSON.stringify(
    {
      bundle: {
        createUpdaterArtifacts: signing,
        resources: ["third-party-notices/**/*"],
        externalBin: [
          "binaries/okytes-cli",
          "binaries/okytes-daemon",
          "binaries/okytes-native-host",
        ],
        ...(windows ? { windows } : {}),
      },
      plugins: {
        updater: {
          pubkey: process.env.OKYTES_UPDATE_PUBLIC_KEY ?? "",
          requireSignedVersion: true,
          windows: { installMode: "basicUi" },
        },
      },
    },
    null,
    2,
  ),
);
