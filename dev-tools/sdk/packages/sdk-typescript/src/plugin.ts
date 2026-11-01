// Plugin preview. A plugin is a separate Node.js process using its own
// permission-restricted daemon token (issued by `okytes-cli plugin-grant`).
// The daemon enforces permissions on every request; the manifest declares them
// so users can review a plugin before granting access.
import { readFile, stat } from "node:fs/promises";
import { join } from "node:path";
import { OkytesClient, OkytesError } from "./node.js";

export const MANIFEST = "okytes-plugin.json";
export const PERMISSIONS = [
  "jobs:read",
  "jobs:add",
  "jobs:control",
  "events:read",
  "media:inspect",
  "settings:read",
] as const;
export type Permission = (typeof PERMISSIONS)[number];

export interface PluginManifest {
  schema: 1;
  id: string;
  name: string;
  version: string;
  description?: string;
  permissions: Permission[];
}

function invalid(message: string): never {
  throw new OkytesError("invalid_manifest", "validation", false, `plugin manifest: ${message}`);
}

/** Validate a parsed manifest with the same rules as the Rust SDK and CLI. */
export function validateManifest(value: unknown): PluginManifest {
  if (!value || typeof value !== "object") invalid("expected a JSON object");
  const m = value as Record<string, unknown>;
  const allowed = new Set(["schema", "id", "name", "version", "description", "permissions"]);
  for (const key of Object.keys(m)) if (!allowed.has(key)) invalid(`unknown field ${key}`);
  if (m.schema !== 1) invalid("unsupported schema; expected 1");
  if (
    typeof m.id !== "string" ||
    m.id.length > 100 ||
    !/^[a-z0-9-]+(\.[a-z0-9-]+)+$/.test(m.id)
  )
    invalid("id must be reverse-domain lowercase, e.g. org.example.tool");
  for (const [field, limit] of [["name", 64], ["version", 32], ["description", 200]] as const) {
    const text = m[field];
    if (field === "description" && text === undefined) continue;
    // eslint-disable-next-line no-control-regex
    if (typeof text !== "string" || text.length > limit || /[\u0000-\u001f\u007f]/.test(text))
      invalid(`${field} is missing, too long or has control characters`);
    if (field !== "description" && !text.trim()) invalid(`${field} is required`);
  }
  const permissions = m.permissions;
  if (
    !Array.isArray(permissions) ||
    !permissions.length ||
    new Set(permissions).size !== permissions.length ||
    permissions.some((p) => !PERMISSIONS.includes(p as Permission))
  )
    invalid(`permissions must list each of ${PERMISSIONS.join(", ")} at most once`);
  return m as unknown as PluginManifest;
}

/** Load a manifest from a file or a directory containing okytes-plugin.json. */
export async function loadManifest(path: string): Promise<PluginManifest> {
  const file = (await stat(path)).isDirectory() ? join(path, MANIFEST) : path;
  const text = await readFile(file, "utf8");
  if (text.length > 64 * 1024) invalid("file exceeds 64 KiB");
  try {
    return validateManifest(JSON.parse(text));
  } catch (error) {
    if (error instanceof OkytesError) throw error;
    return invalid("invalid JSON");
  }
}

export class Plugin {
  private constructor(
    readonly manifest: PluginManifest,
    readonly client: OkytesClient,
  ) {}

  /** Connect with OKYTES_DAEMON_URL and OKYTES_PLUGIN_TOKEN_FILE. */
  static async fromEnvironment(manifest: PluginManifest): Promise<Plugin> {
    const endpoint = process.env.OKYTES_DAEMON_URL;
    const file = process.env.OKYTES_PLUGIN_TOKEN_FILE;
    if (!endpoint || !file)
      throw new OkytesError(
        "plugin_environment",
        "validation",
        false,
        "Set OKYTES_DAEMON_URL and OKYTES_PLUGIN_TOKEN_FILE (from okytes-cli plugin-grant).",
      );
    // Re-read the file on each call so a re-granted token is picked up.
    const token = async () => (await readFile(file, "utf8")).trim();
    return new Plugin(manifest, new OkytesClient({ endpoint, token }));
  }

  /** Fail early when calling outside the declared permissions. */
  require(permission: Permission): void {
    if (!this.manifest.permissions.includes(permission))
      throw new OkytesError(
        "undeclared_permission",
        "authorization",
        false,
        `plugin ${this.manifest.id} did not declare ${permission}`,
      );
  }
}
