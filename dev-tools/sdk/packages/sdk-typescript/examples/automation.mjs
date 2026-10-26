import { OkytesClient } from "@okytes/sdk";

const client = new OkytesClient({ endpoint: process.env.OKYTES_DAEMON_URL, token: () => process.env.OKYTES_TOKEN });
const capabilities = await client.capabilities();
const supports = (protocol, operation) => capabilities.protocols.some((item) => item.protocol === protocol && item.operations.includes(operation));

const ids = await client.add({ urls: ["https://example.invalid/a", "https://example.invalid/b"], destination: null, allow_duplicates: false });
await client.control(ids[0], { kind: "Pause" });
await client.control(ids[0], { kind: "Resume" }); // the durable job survives client/process restart
if (supports("torrent", "inspect")) await client.inspectTorrent({ magnet: process.env.MAGNET_URI, metainfo: null, destination: null, options: {} });
if (supports("sftp", "download")) await client.download({ url: "sftp://host.example.invalid/archive", destination: null, filename: null, sha256: null, conflict: "rename", credentials: null });
if (supports("okytes-peer", "send") && process.env.OKYTES_PEER_ID) await client.sendToPeer({ peer_id: process.env.OKYTES_PEER_ID, address: process.env.OKYTES_PEER_ADDRESS, paths: ids });
