import { api } from "./compat";
import { checkedReply } from "./messages";
import { defaults, normalizeRules, type Rules } from "./rules";
const status = document.getElementById("status") as HTMLParagraphElement;
const urls = document.getElementById("urls") as HTMLTextAreaElement;
const auto = document.getElementById("auto") as HTMLInputElement;
const hint = document.getElementById("auto-hint") as HTMLParagraphElement;
let mediaMode = false;
async function send(value: unknown): Promise<unknown> {
  const result = await api.runtime.sendMessage(value);
  return checkedReply(result);
}
function reason(error: unknown, fallback: string): string {
  return error instanceof Error && error.message ? error.message : fallback;
}
async function stored(): Promise<Rules> {
  const value = await api.storage.local.get("rules");
  return value.rules ? normalizeRules(value.rules) : defaults;
}
function describeAuto(rules: Rules): void {
  auto.checked = rules.enabled;
  const scoped =
    rules.allow.length || rules.types.length || rules.minBytes || rules.maxBytes;
  hint.textContent = rules.enabled
    ? `New browser downloads${scoped ? " matching your Settings rules" : ""} are handed to Okytes; the browser keeps them if Okytes is unavailable. Alt-click a link to skip once on allowed sites.`
    : "Off: use the right-click menu or the box below to send links.";
}
void stored()
  .then(describeAuto)
  .catch(() => {
    hint.textContent = "Saved settings could not be read.";
  });
auto.addEventListener("change", () => {
  void stored()
    .then(async (rules) => {
      const next = normalizeRules({ ...rules, enabled: auto.checked });
      await api.storage.local.set({ rules: next });
      describeAuto(next);
    })
    .catch(() => {
      auto.checked = !auto.checked;
      hint.textContent = "The setting could not be saved.";
    });
});
async function action(kind: string): Promise<void> {
  const button = document.getElementById(kind) as HTMLButtonElement;
  button.disabled = true;
  try {
    if (kind === "send") {
      const result = (await send({
        kind: mediaMode ? "capture_media" : "capture",
        urls: urls.value.split(/\s+/).filter(Boolean),
      })) as { accepted: number; failed: number; errors: string[] };
      status.textContent = result.failed
        ? `${result.accepted} sent to Okytes, ${result.failed} not accepted: ${result.errors.join(" ")} Your links remain here.`
        : `${result.accepted} sent to Okytes.`;
    } else if (kind === "collect" || kind === "media") {
      const links = (await send({ kind })) as string[];
      urls.value = links.join("\n");
      mediaMode = kind === "media";
      status.textContent =
        kind === "media"
          ? `${links.length} manifest candidates. Review, then send; Okytes refuses DRM and unsupported streams.`
          : `${links.length} links collected. Review before sending.`;
    } else await api.runtime.openOptionsPage();
  } catch (error) {
    status.textContent = `${reason(error, "Okytes is unavailable or site access was denied.")} Your links remain here.`;
  } finally {
    button.disabled = false;
  }
}
for (const id of ["send", "collect", "media", "settings"])
  document.getElementById(id)?.addEventListener("click", () => {
    void action(id);
  });
void send({ kind: "ping" })
  .then((value) => {
    const response = value as { ok: boolean; error?: string | null };
    status.textContent = response.ok
      ? "Desktop connected"
      : (response.error ?? "Open Okytes to send downloads");
  })
  .catch((error: unknown) => {
    status.textContent = reason(
      error,
      "Open Okytes and install the native host to connect",
    );
  });
