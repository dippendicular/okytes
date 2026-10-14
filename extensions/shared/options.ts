import { api } from "./compat";
import { defaults, normalizeRules, validUrl } from "./rules";
const field = (id: string) => document.getElementById(id) as HTMLInputElement;
const status = document.getElementById("status") as HTMLParagraphElement;
void api.storage.local
  .get("rules")
  .then((value) => {
    const rules = value.rules ? normalizeRules(value.rules) : defaults;
    field("enabled").checked = rules.enabled;
    for (const key of ["allow", "deny", "types"] as const)
      field(key).value = rules[key].join(" ");
    field("min").value = String(rules.minBytes);
    field("max").value = String(rules.maxBytes);
  })
  .catch(() => {
    status.textContent = "Saved rules could not be read.";
  });
document.getElementById("rules")?.addEventListener("submit", (event) => {
  event.preventDefault();
  try {
    const list = (id: string) => field(id).value.split(/\s+/).filter(Boolean);
    const rules = normalizeRules({
      version: 1,
      enabled: field("enabled").checked,
      allow: list("allow"),
      deny: list("deny"),
      types: list("types"),
      minBytes: Number(field("min").value),
      maxBytes: Number(field("max").value),
    });
    void api.storage.local
      .set({ rules })
      .then(() => {
        status.textContent = "Rules saved.";
      })
      .catch(() => {
        status.textContent = "Rules could not be saved.";
      });
  } catch (error) {
    status.textContent =
      error instanceof Error ? error.message : "Invalid rules";
  }
});
document.getElementById("grant")?.addEventListener("click", () => {
  const value = field("origin").value;
  if (!validUrl(value)) {
    status.textContent = "Enter a full HTTP/HTTPS origin.";
    return;
  }
  const origin = `${new URL(value).origin}/*`;
  // Request from the actual click gesture; no automatic blanket grants.
  void api.permissions
    .request({ permissions: ["cookies", "webRequest"], origins: [origin] })
    .then(async (granted) => {
      if (!granted) {
        status.textContent = "Permission was not granted.";
        return;
      }
      const id = `bypass-${new URL(value).hostname.replace(/[^a-z0-9]/gi, "-")}`;
      await api.scripting
        .unregisterContentScripts({ ids: [id] })
        .catch(() => {});
      await api.scripting.registerContentScripts([
        { id, matches: [origin], js: ["bypass.js"], runAt: "document_start" },
      ]);
      status.textContent =
        "Site access granted. Reload the page to enable Alt-click bypass.";
    })
    .catch(() => {
      status.textContent = "Site access could not be enabled.";
    });
});
