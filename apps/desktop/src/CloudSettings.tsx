import { useEffect, useState } from "react";
import { api, type CloudStatus, type StorageProvider } from "./api";

// Rendered inside the Settings form, so its buttons are type="button" and
// pairing never submits or saves the surrounding settings.
export function CloudSettings() {
  const [status, setStatus] = useState<CloudStatus | null>();
  const [url, setUrl] = useState("http://127.0.0.1:17900");
  const [code, setCode] = useState("");
  const [name, setName] = useState("My computer");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [remote, setRemote] =
    useState<Awaited<ReturnType<typeof api.cloudHistory>>>();
  useEffect(() => {
    let live = true;
    const load = () =>
      void api
        .cloudStatus()
        .then((value) => live && setStatus(value))
        .catch(() => live && setStatus(null));
    load();
    const timer = setInterval(load, 10000);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, []);
  const run = (task: () => Promise<void>) => {
    setBusy(true);
    setMessage("");
    task()
      .catch((e: unknown) => setMessage(String(e).replace(/^Error:\s*/i, "")))
      .finally(() => setBusy(false));
  };
  return (
    <fieldset className="settings-section">
      <legend>Okytes Cloud</legend>
      {status ? (
        <>
          <p>
            Paired as <strong>{status.name}</strong> with {status.url}. Links
            sent from the dashboard or your phone start here automatically, and
            download history syncs every minute without link query strings.
          </p>
          {status.error && (
            <p role="alert" className="alert">
              {status.error}
            </p>
          )}
          <button
            type="button"
            disabled={busy}
            onClick={() =>
              run(async () => {
                await api.cloudUnpair();
                setStatus(null);
                setMessage(
                  "Unpaired. Also remove this device in the dashboard to revoke its token.",
                );
              })
            }
          >
            Unpair this computer
          </button>
          <button
            type="button"
            onClick={() =>
              void api
                .cloudHistory()
                .then(setRemote)
                .catch(() => setRemote([]))
            }
          >
            Downloads on my other devices
          </button>
          {remote && (
            <div className="table-scroll">
              <table className="connections">
                <thead>
                  <tr>
                    <th>File</th>
                    <th>Device</th>
                    <th>State</th>
                    <th />
                  </tr>
                </thead>
                <tbody>
                  {remote.length === 0 && (
                    <tr>
                      <td colSpan={4}>
                        Nothing synced from other devices yet (updates every
                        minute).
                      </td>
                    </tr>
                  )}
                  {remote.slice(0, 200).map((entry) => (
                    <tr key={`${entry.device_name}-${entry.job_id}`}>
                      <td title={entry.source}>{entry.name}</td>
                      <td>{entry.device_name ?? "—"}</td>
                      <td>{entry.state}</td>
                      <td>
                        <button
                          type="button"
                          title="Synced links have their query removed; signed links may need a fresh copy"
                          onClick={() =>
                            run(async () => {
                              await api.add({
                                urls: [entry.source],
                                destination: null,
                                allow_duplicates: false,
                              });
                              setMessage(`Added ${entry.name}.`);
                            })
                          }
                        >
                          Download here
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </>
      ) : (
        <>
          <p className="muted">
            Connect to your self-hosted okytes-cloud server. Sign in to its
            dashboard, choose Pair a device, and enter the code here.
          </p>
          <label>
            Server address
            <input value={url} onChange={(e) => setUrl(e.target.value)} />
          </label>
          <label>
            Pairing code
            <input
              value={code}
              onChange={(e) => setCode(e.target.value.toUpperCase())}
              maxLength={8}
              autoComplete="off"
            />
          </label>
          <label>
            This computer's name
            <input
              value={name}
              onChange={(e) => setName(e.target.value)}
              maxLength={64}
            />
          </label>
          <button
            type="button"
            disabled={busy || code.trim().length !== 8 || !name.trim()}
            onClick={() =>
              run(async () => {
                setStatus(await api.cloudPair(url, code, name));
                setCode("");
              })
            }
          >
            {busy ? "Pairing…" : "Pair"}
          </button>
        </>
      )}
      {message && <p role="status">{message}</p>}
    </fieldset>
  );
}

const consoles: Record<string, string> = {
  gdrive:
    "Google Cloud Console → Credentials → OAuth client (Desktop app). Enable the Google Drive API.",
  dropbox:
    "Dropbox App Console → Create app (Scoped, App folder or Full Dropbox) with files.content.write.",
  onedrive:
    "Microsoft Entra → App registrations → Mobile and desktop platform, personal accounts allowed.",
};

// Each provider needs the user's own OAuth app; Okytes ships no shared keys.
export function StorageSettings() {
  const [providers, setProviders] = useState<StorageProvider[]>([]);
  const [editing, setEditing] = useState<string>();
  const [clientId, setClientId] = useState("");
  const [secret, setSecret] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const load = () =>
    api
      .storageStatus()
      .then(setProviders)
      .catch(() => {});
  useEffect(() => {
    void load();
  }, []);
  const run = (task: () => Promise<unknown>, done: string) => {
    setBusy(true);
    setMessage("");
    task()
      .then(() => {
        setMessage(done);
        return load();
      })
      .catch((e: unknown) => setMessage(String(e).replace(/^Error:\s*/i, "")))
      .finally(() => setBusy(false));
  };
  return (
    <fieldset className="settings-section">
      <legend>Cloud storage</legend>
      <p className="muted">
        Upload finished downloads to an Okytes folder (choose it per download
        under Options on completion). Register redirect URI{" "}
        <code>http://127.0.0.1:17901/callback</code> in each provider's app.
      </p>
      {providers.map((p) => (
        <div key={p.provider} className="storage-row">
          <strong>{p.name}</strong>
          <span className="muted">
            {p.connected
              ? "Connected"
              : p.configured
                ? "Not connected"
                : "Not set up"}
          </span>
          <button
            type="button"
            disabled={busy}
            onClick={() => {
              setEditing(editing === p.provider ? undefined : p.provider);
              setClientId("");
              setSecret("");
            }}
          >
            {p.configured ? "Change app" : "Set up"}
          </button>
          {p.configured && !p.connected && (
            <button
              type="button"
              disabled={busy}
              onClick={() =>
                run(
                  () => api.storageConnect(p.provider),
                  `${p.name} connected.`,
                )
              }
            >
              Sign in…
            </button>
          )}
          {p.connected && (
            <button
              type="button"
              disabled={busy}
              onClick={() =>
                run(
                  () => api.storageDisconnect(p.provider),
                  `${p.name} disconnected. Also remove Okytes from your account's connected apps to revoke access.`,
                )
              }
            >
              Disconnect
            </button>
          )}
          {editing === p.provider && (
            <div className="storage-form">
              <p className="muted">{consoles[p.provider]}</p>
              <label>
                Client ID
                <input
                  value={clientId}
                  onChange={(e) => setClientId(e.target.value)}
                />
              </label>
              {p.provider === "gdrive" && (
                <label>
                  Client secret (Google desktop clients require it)
                  <input
                    type="password"
                    value={secret}
                    onChange={(e) => setSecret(e.target.value)}
                  />
                </label>
              )}
              <button
                type="button"
                disabled={busy || !clientId.trim()}
                onClick={() =>
                  run(async () => {
                    await api.storageConfigure(
                      p.provider,
                      clientId,
                      secret || null,
                    );
                    setEditing(undefined);
                  }, `${p.name} app saved. Choose Sign in to connect.`)
                }
              >
                Save app
              </button>
            </div>
          )}
        </div>
      ))}
      {busy && <p className="muted">Waiting for the browser sign-in…</p>}
      {message && <p role="status">{message}</p>}
    </fieldset>
  );
}
