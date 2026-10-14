export interface Rules {
  version: 1;
  enabled: boolean;
  allow: string[];
  deny: string[];
  types: string[];
  minBytes: number;
  maxBytes: number;
}
export const defaults: Rules = {
  version: 1,
  enabled: false,
  allow: [],
  deny: [],
  types: [],
  minBytes: 0,
  maxBytes: 0,
};
export function validUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return (
      value.length <= 8192 &&
      ["http:", "https:"].includes(url.protocol) &&
      !url.username &&
      !url.password
    );
  } catch {
    return false;
  }
}
export function normalizeRules(value: unknown): Rules {
  if (!value || typeof value !== "object") throw new Error("Invalid rules");
  const r = value as Record<string, unknown>;
  for (const key of ["allow", "deny", "types"]) {
    if (
      !Array.isArray(r[key]) ||
      r[key].length > 100 ||
      r[key].some(
        (item: unknown) => typeof item !== "string" || item.length > 253,
      )
    )
      throw new Error("Use at most 100 entries per rule");
  }
  const allow = (r.allow as string[]).map(domain);
  const deny = (r.deny as string[]).map(domain);
  const types = (r.types as string[]).map((s) =>
    s.toLowerCase().replace(/^\./, ""),
  );
  if (types.some((s) => !/^[a-z0-9]{1,16}$/.test(s)))
    throw new Error("File types must be extensions such as zip or pdf");
  const min = r.minBytes;
  const max = r.maxBytes;
  if (
    r.version !== 1 ||
    typeof r.enabled !== "boolean" ||
    typeof min !== "number" ||
    typeof max !== "number" ||
    !Number.isSafeInteger(min) ||
    !Number.isSafeInteger(max) ||
    min < 0 ||
    max < 0 ||
    (max && max < min)
  )
    throw new Error("Invalid size limits");
  return {
    version: 1,
    enabled: r.enabled,
    allow,
    deny,
    types,
    minBytes: min,
    maxBytes: max,
  };
}
function domain(value: string): string {
  const name = value.toLowerCase().trim();
  if (
    !name ||
    !/^[a-z0-9.-]+$/.test(name) ||
    name.startsWith(".") ||
    name.endsWith(".")
  )
    throw new Error("Use domain names without paths or wildcards");
  return name;
}
function matches(host: string, entries: string[]): boolean {
  return entries.some((entry) => host === entry || host.endsWith(`.${entry}`));
}
export function shouldCapture(
  rules: Rules,
  input: { url: string; filename: string; size: number; bypass: boolean },
): boolean {
  if (!rules.enabled || input.bypass || !validUrl(input.url)) return false;
  const url = new URL(input.url);
  if (
    matches(url.hostname, rules.deny) ||
    (rules.allow.length && !matches(url.hostname, rules.allow))
  )
    return false;
  const ext =
    (input.filename || url.pathname)
      .split(/[\\/]/)
      .pop()
      ?.split(".")
      .pop()
      ?.toLowerCase() ?? "";
  if (rules.types.length && !rules.types.includes(ext)) return false;
  if ((rules.minBytes || rules.maxBytes) && input.size < 0) return false;
  if (input.size < 0) return true;
  return (
    input.size >= rules.minBytes &&
    (!rules.maxBytes || input.size <= rules.maxBytes)
  );
}
