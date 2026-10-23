import { useCallback, useEffect, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { api, bytes, type PeerDevice, type PeerRequest, type PeerTrust } from "./api";

export function PeerShare({ report }: { report: (message: string) => void }) {
  const [identity, setIdentity] = useState<{
    fingerprint: string;
    certificate: string;
  }>();
  const [devices, setDevices] = useState<PeerDevice[]>([]);
  const [requests, setRequests] = useState<PeerRequest[]>([]);
  const [trusts, setTrusts] = useState<PeerTrust[]>([]);
  const [rates, setRates] = useState<Record<string, number>>({});
  const [discoveryEnabled, setDiscoveryEnabled] = useState(false);
  const samples = useRef<Record<string, { bytes: number; at: number }>>({});
  const [peer, setPeer] = useState("");
  const [address, setAddress] = useState("");
  const [targets, setTargets] = useState<Array<{ peer_id: string; address: string }>>([]);
  const [paths, setPaths] = useState<string[]>([]);
  const [certificate, setCertificate] = useState("");
  const [fingerprint, setFingerprint] = useState("");
  const [preview, setPreview] = useState(false);
  const refresh = useCallback(async () => {
    const [me, nearby, history, paired, discovery] = await Promise.all([
      api.peerIdentity(),
      api.peerDiscover(),
      api.peerRequests(),
      api.peerTrusts(),
      api.peerDiscoverySetting(),
    ]);
    setIdentity(me);
    setDevices(nearby);
    setRequests(history);
    setTrusts(paired);
    setDiscoveryEnabled(discovery.enabled);
    const now = Date.now();
    const nextRates: Record<string, number> = {};
    for (const request of history) {
      const previous = samples.current[request.id];
      if (previous && request.bytes >= previous.bytes && now > previous.at) {
        nextRates[request.id] =
          ((request.bytes - previous.bytes) * 1000) / (now - previous.at);
      }
      samples.current[request.id] = { bytes: request.bytes, at: now };
    }
    setRates(nextRates);
  }, []);
  useEffect(() => {
    void refresh().catch((e) => report(String(e)));
    const timer = window.setInterval(
      () => void refresh().catch((e) => report(String(e))),
      1500,
    );
    return () => window.clearInterval(timer);
  }, [refresh, report]);
  const choose = async (directory: boolean) => {
    const selected = await open({ multiple: !directory, directory });
    const values =
      selected === null ? [] : Array.isArray(selected) ? selected : [selected];
    setPaths((old) => [...new Set([...old, ...values])].slice(0, 32));
  };
  return (
    <div className="peer-share">
      {identity && (
        <section>
          <h3>This device</h3>
          <p className="muted">
            Compare this complete fingerprint on the other device before
            pairing.
          </p>
          <code>{identity.fingerprint}</code>
          <details>
            <summary>Public pairing certificate</summary>
            <textarea readOnly value={identity.certificate} rows={4} />
          </details>
        </section>
      )}
      <section>
        <h3>Nearby devices</h3>
        <label>
          <input
            type="checkbox"
            checked={discoveryEnabled}
            onChange={(event) =>
              void api
                .setPeerDiscovery(event.target.checked)
                .then((setting) => setDiscoveryEnabled(setting.enabled))
                .catch((e) => report(String(e)))
            }
          />{" "}
          Make this device visible and discover nearby Okytes devices
        </label>
        {devices.length === 0 ? (
          <p className="muted">
            No devices found. Enter a paired device ID and address manually when
            multicast is unavailable.
          </p>
        ) : (
          devices.map((device) => (
            <button
              key={device.fullname}
              disabled={!device.peer_id}
              onClick={() => {
                if (!device.peer_id || !device.addresses[0]) return;
                const target = {
                  peer_id: device.peer_id,
                  address: `${device.addresses[0]}:${device.port}`,
                };
                setTargets((current) =>
                  current.some((item) => item.peer_id === target.peer_id)
                    ? current.filter((item) => item.peer_id !== target.peer_id)
                    : [...current, target],
                );
              }}
            >
              <strong>{device.name}</strong> · {device.os} ·{" "}
              {device.trusted
                ? targets.some((target) => target.peer_id === device.peer_id)
                  ? "Selected"
                  : "Paired"
                : "Pair first"}
            </button>
          ))
        )}
      </section>
      <section>
        <h3>Pair a device</h3>
        <label>
          Public certificate
          <textarea
            value={certificate}
            onChange={(e) => setCertificate(e.target.value.trim())}
            rows={3}
          />
        </label>
        <label>
          Confirmed fingerprint
          <input
            value={fingerprint}
            onChange={(e) =>
              setFingerprint(e.target.value.trim().toLowerCase())
            }
          />
        </label>
        <button
          disabled={!certificate || fingerprint.length !== 64}
          onClick={() =>
            void api
              .peerTrust({
                id: crypto.randomUUID(),
                certificate,
                confirmed_fingerprint: fingerprint,
                expected_revision: null,
              })
              .then(refresh)
              .catch((e) => report(String(e)))
          }
        >
          Pair device
        </button>
        <h4>Trusted devices</h4>
        {trusts.filter((trust) => !trust.revoked).length === 0 ? (
          <p className="muted">No paired devices.</p>
        ) : (
          trusts
            .filter((trust) => !trust.revoked)
            .map((trust) => (
              <div className="actions" key={trust.id}>
                <code>{trust.fingerprint.slice(0, 20)}…</code>
                <button
                  onClick={() =>
                    void api
                      .peerRevoke(trust.id, trust.revision)
                      .then(refresh)
                      .catch((e) => report(String(e)))
                  }
                >
                  Revoke
                </button>
              </div>
            ))
        )}
      </section>
      <section>
        <h3>Send files</h3>
        <label>
          Paired device ID
          <input value={peer} onChange={(e) => setPeer(e.target.value)} />
        </label>
        <label>
          IP address and port
          <input
            placeholder="192.168.1.20:17842"
            value={address}
            onChange={(e) => setAddress(e.target.value)}
          />
        </label>
        <div className="actions">
          <button onClick={() => void choose(false)}>Choose files</button>
          <button onClick={() => void choose(true)}>Choose folder</button>
        </div>
        <ul>
          {paths.map((path) => (
            <li key={path}>
              {path}{" "}
              <button
                aria-label={`Remove ${path}`}
                onClick={() => setPaths(paths.filter((item) => item !== path))}
              >
                ×
              </button>
            </li>
          ))}
        </ul>
        {preview && (
          <article>
            <strong>Review transfer</strong>
            <p>
              {paths.length} selected item(s) will be offered to{" "}
              {targets.length || 1} device(s). The receiver must approve before
              any file data is sent.
            </p>
            <ul>{paths.map((path) => <li key={path}>{path}</li>)}</ul>
          </article>
        )}
        <button
          className="primary"
          disabled={
            (targets.length === 0 && (!peer || !address)) || paths.length === 0
          }
          onClick={() => {
            if (!preview) {
              setPreview(true);
              return;
            }
            void Promise.all(
              (targets.length > 0 ? targets : [{ peer_id: peer, address }]).map(
                (target) => api.peerSend({ ...target, paths }),
              ),
            )
              .then((ids) => {
                report(`${ids.length} transfer request(s) sent`);
                setPaths([]);
                setTargets([]);
                setPreview(false);
                return refresh();
              })
              .catch((e) => report(String(e)));
          }}
        >
          {preview ? "Confirm and send" : "Review transfer"}
        </button>
      </section>
      <section>
        <h3>Transfer requests</h3>
        {requests.map((request) => {
          const rate = rates[request.id] ?? 0;
          const eta = rate > 0 ? Math.ceil((request.total - request.bytes) / rate) : 0;
          const fileCount = request.manifest.entries.filter(
            (entry) => entry.kind === "file",
          ).length;
          const decide = (action: string, destination: string | null = null) =>
            void api
              .peerDecide(
                request.id,
                request.revision,
                action as "accept" | "reject" | "pause" | "retry" | "cancel",
                destination,
              )
              .then(refresh)
              .catch((e) => report(String(e)));
          return (
            <article key={request.id}>
              <strong>
                {request.direction === "inbound" ? "Incoming" : "Outgoing"} ·{" "}
                {request.state}
              </strong>
              <p>
                {request.files_completed} of {fileCount} files · {bytes(request.bytes)} of{" "}
                {bytes(request.total)}
              </p>
              {rate > 0 && request.state === "transferring" && (
                <p className="muted">{bytes(rate)}/s · about {eta}s remaining</p>
              )}
              <progress value={request.bytes} max={Math.max(1, request.total)} />
              {request.failure && <p className="error">{request.failure}</p>}
              {request.state === "completed" && request.destination && (
                <button
                  onClick={() =>
                    void api
                      .openPeerDestination(request.destination as string)
                      .catch((e) => report(String(e)))
                  }
                >
                  Open destination
                </button>
              )}
              {request.state === "awaiting_approval" &&
                request.direction === "inbound" && (
                  <div className="actions">
                    <button
                      onClick={() =>
                        void open({ directory: true }).then((destination) => {
                          if (typeof destination === "string") {
                            decide("accept", destination);
                          }
                        })
                      }
                    >
                      Choose destination and accept
                    </button>
                    <button onClick={() => decide("reject")}>Reject</button>
                  </div>
                )}
              {["negotiating", "transferring"].includes(request.state) && (
                <div className="actions">
                  <button onClick={() => decide("pause")}>Pause</button>
                  <button onClick={() => decide("cancel")}>Cancel</button>
                </div>
              )}
              {["paused", "interrupted", "failed"].includes(request.state) && (
                <div className="actions">
                  <button onClick={() => decide("retry")}>Retry</button>
                  <button onClick={() => decide("cancel")}>Cancel</button>
                </div>
              )}
              {request.state === "awaiting_approval" &&
                request.direction === "outbound" && (
                  <button onClick={() => decide("cancel")}>Cancel</button>
                )}
            </article>
          );
        })}
      </section>
    </div>
  );
}
