import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export function QuickAccess() {
  const [pairing, setPairing] = useState<{ code: string; url: string }>();
  const [unavailable, setUnavailable] = useState(false);
  const [busy, setBusy] = useState(false);
  async function open() {
    setBusy(true);
    setUnavailable(false);
    setPairing(undefined);
    try {
      setPairing(await invoke("open_quick"));
    } catch {
      setUnavailable(true);
    } finally {
      setBusy(false);
    }
  }
  useEffect(() => {
    let live = true;
    let remove: (() => void) | undefined;
    void listen("quick-open", () => {
      void open();
    })
      .then((unlisten) => {
        if (live) remove = unlisten;
        else unlisten();
      })
      .catch(() => {});
    return () => {
      live = false;
      remove?.();
    };
  }, []);
  useEffect(() => {
    if (!pairing) return;
    const timer = setTimeout(() => setPairing(undefined), 120000);
    return () => clearTimeout(timer);
  }, [pairing]);
  return (
    <section className="quick-access" aria-label="Quick Download access">
      <button
        type="button"
        disabled={busy}
        onClick={() => {
          void open();
        }}
      >
        {busy ? "Opening…" : "Quick Download"}
      </button>
      {pairing && (
        <div role="status">
          <p>Enter this one-use code in {pairing.url} within two minutes:</p>
          <code>{pairing.code}</code>
          <button type="button" onClick={() => setPairing(undefined)}>
            Hide code
          </button>
        </div>
      )}
      {unavailable && (
        <p role="status">Quick Download is unavailable right now.</p>
      )}
    </section>
  );
}
