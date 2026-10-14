import { api } from "./compat";
import { checkedReply } from "./messages";
const status = document.getElementById("status") as HTMLParagraphElement;
const urls = document.getElementById("urls") as HTMLTextAreaElement;
async function send(value: unknown): Promise<unknown> {
  const result = await api.runtime.sendMessage(value);
  return checkedReply(result);
}
async function action(kind: string): Promise<void> {
  const button = document.getElementById(kind) as HTMLButtonElement;
  button.disabled = true;
  try {
    if (kind === "send") {
      const result = (await send({
        kind: "capture",
        urls: urls.value.split(/\s+/).filter(Boolean),
      })) as { accepted: number; failed: number };
      status.textContent = `${result.accepted} sent to Okytes. ${result.failed} not accepted; their links remain here.`;
    } else if (kind === "collect" || kind === "media") {
      const links = (await send({ kind })) as string[];
      urls.value = links.join("\n");
      status.textContent =
        kind === "media"
          ? `${links.length} candidate manifests; detection only.`
          : `${links.length} links collected. Review before sending.`;
    } else await api.runtime.openOptionsPage();
  } catch {
    status.textContent =
      "Okytes is unavailable or site access was denied. Your links remain here.";
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
    status.textContent = (value as { ok: boolean }).ok
      ? "Desktop connected"
      : "Open Okytes to send downloads";
  })
  .catch(() => {
    status.textContent = "Open Okytes and install the native host to connect";
  });
