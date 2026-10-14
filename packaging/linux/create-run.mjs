/** Wrap a controlled tar.gz payload; checksum verification precedes extraction. */
import { readFile, writeFile, chmod } from "node:fs/promises";
import { createHash } from "node:crypto";
import { pathToFileURL } from "node:url";

export async function createRun(payloadPath, outputPath) {
  const payload = await readFile(payloadPath);
  const checksum = createHash("sha256").update(payload).digest("hex");
  const header = `#!/bin/sh
set -eu
expected=${checksum}
prefix="\${HOME:?HOME is required}/.local/opt/okytes"
check_only=false
while [ "$#" -gt 0 ]; do
  case "$1" in
    --check) check_only=true; shift ;;
    --prefix) [ "$#" -ge 2 ] || exit 2; prefix="$2"; shift 2 ;;
    *) echo 'Usage: installer.run [--check] [--prefix /absolute/directory]' >&2; exit 2 ;;
  esac
done
case "$prefix" in /*) ;; *) echo 'Prefix must be absolute' >&2; exit 2 ;; esac
[ "$prefix" != / ] || { echo 'Refusing the filesystem root' >&2; exit 2; }
command -v sha256sum >/dev/null || { echo 'sha256sum is required' >&2; exit 1; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
line=$(awk '/^__OKYTES_PAYLOAD__$/ {print NR + 1; exit}' "$0")
tail -n +"$line" "$0" > "$tmp/payload.tar.gz"
actual=$(sha256sum "$tmp/payload.tar.gz")
actual=\${actual%% *}
[ "$actual" = "$expected" ] || { echo 'Payload checksum mismatch; nothing installed' >&2; exit 1; }
if "$check_only"; then echo 'Payload checksum verified'; exit 0; fi
# The payload is created from the release staging directory, never user archives.
tar -tzf "$tmp/payload.tar.gz" > "$tmp/entries"
if grep -Eq '(^/|(^|/)\\.\\.(/|$))' "$tmp/entries"; then echo 'Unsafe archive path' >&2; exit 1; fi
if [ -e "$prefix" ]; then
  echo 'Destination already exists. Choose a new --prefix; keep your current installation until this one works.' >&2
  exit 1
fi
mkdir -p "$(dirname "$prefix")"
stage=$(mktemp -d "$(dirname "$prefix")/.okytes-install.XXXXXX")
trap 'rm -rf "$tmp" "$stage"' EXIT HUP INT TERM
tar -xzf "$tmp/payload.tar.gz" -C "$stage" --no-same-owner
[ -x "$stage/bin/okytes-desktop" ] || { echo 'Missing executable' >&2; exit 1; }
mv -T "$stage" "$prefix"
echo "Installed in $prefix. Launch: $prefix/bin/okytes-desktop"
echo 'No service or browser host was registered. Use the included CLI to register native messaging explicitly.'
exit 0
__OKYTES_PAYLOAD__
`;
  await writeFile(outputPath, Buffer.concat([Buffer.from(header), payload]), {
    flag: "wx",
  });
  await chmod(outputPath, 0o755);
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  const [, , payload, output] = process.argv;
  if (!payload || !output)
    throw new Error("Usage: node create-run.mjs payload.tar.gz installer.run");
  await createRun(payload, output);
}
