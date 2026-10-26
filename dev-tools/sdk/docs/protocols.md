# SDK protocol capabilities

Always call `/v1/capabilities` rather than assuming an adapter exists.

| Protocol | Operations | Security and resume boundary |
| --- | --- | --- |
| HTTP/HTTPS | Download and lifecycle control | Strong validators and safe byte ranges; optional expected SHA-256 |
| FTP/FTPS | Download | Explicit credentials; FTPS validates TLS |
| SFTP | Download | Requires an explicitly confirmed SSH host-key fingerprint |
| BitTorrent | Inspect, select files, download, seed | Piece hashes and torrent privacy controls |
| HLS/DASH | Inspect and download supported VOD | Unprotected media only; no DRM bypass |
| Okytes peer | Discover, send, receive, approve | Exact pinned mutual TLS identity and separate receiver approval |

Destination paths belong to the engine or daemon host. SDK callers do not gain arbitrary remote filesystem access.
