import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type Offer = { version: string; notes: string };

export function Updates() {
  const [offer, setOffer] = useState<Offer>();
  const [verified, setVerified] = useState(false);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  async function run(action: () => Promise<void>) {
    setBusy(true);
    setMessage("");
    try {
      await action();
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  }
  return (
    <fieldset>
      <legend>Software updates</legend>
      <p>
        Okytes checks for updates only when you ask. Checking connects to
        GitHub; no account, device ID, download history or analytics are sent.
      </p>
      <button
        type="button"
        disabled={busy}
        onClick={() =>
          void run(async () => {
            setOffer(undefined);
            setVerified(false);
            const update = await invoke<Offer | null>("check_update");
            if (update) setOffer(update);
            else setMessage("You have the latest release.");
          })
        }
      >
        Check for updates
      </button>
      {offer && (
        <>
          <h3>Okytes {offer.version}</h3>
          <pre className="release-notes">{offer.notes}</pre>
          {!verified ? (
            <button
              type="button"
              disabled={busy}
              onClick={() =>
                void run(async () => {
                  await invoke("download_update", { confirmed: true });
                  setVerified(true);
                  setMessage(
                    "Signature verified. Install when you are ready to close Okytes.",
                  );
                })
              }
            >
              Download and verify update
            </button>
          ) : (
            <button
              type="button"
              disabled={busy}
              onClick={() =>
                void run(async () => {
                  await invoke("install_update", { confirmed: true });
                  setOffer(undefined);
                  setVerified(false);
                  setMessage(
                    "Installer started. Close and reopen Okytes when installation finishes.",
                  );
                })
              }
            >
              Install verified update
            </button>
          )}
        </>
      )}
      <p role="status">{busy ? "Working…" : message}</p>
    </fieldset>
  );
}
