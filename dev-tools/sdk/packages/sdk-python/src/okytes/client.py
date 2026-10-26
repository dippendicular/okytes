from __future__ import annotations

import asyncio
import inspect
import json
import re
import ssl
import urllib.error
import urllib.parse
import urllib.request
import uuid
from collections.abc import AsyncIterator, Awaitable, Callable
from typing import Any

from .models import Capabilities, Json, Job, MediaChoice, Snapshot, TorrentMetadata, TorrentSnapshot, Update

TokenProvider = str | Callable[[], str | Awaitable[str]]
TOKEN = re.compile(r"^[a-f0-9-]{36}\.[a-f0-9]{64}$")

class OkytesError(Exception):
    def __init__(self, code: str, category: str, retryable: bool, message: str, status: int | None = None) -> None:
        super().__init__(message)
        self.code, self.category, self.retryable, self.status = code, category, retryable, status

class OkytesClient:
    def __init__(self, endpoint: str, token: TokenProvider, timeout: float = 15.0, max_retries: int = 3) -> None:
        parsed = urllib.parse.urlsplit(endpoint)
        local = parsed.hostname in {"localhost", "127.0.0.1", "::1"}
        if parsed.scheme not in {"http", "https"} or (parsed.scheme == "http" and not local) or parsed.username or parsed.password or parsed.path != "/" or parsed.query or parsed.fragment:
            raise ValueError("use loopback HTTP or verified HTTPS with a root endpoint")
        self._endpoint, self._provider, self._timeout = endpoint, token, timeout
        self._closed, self._max_retries = False, max_retries

    async def __aenter__(self) -> "OkytesClient": return self
    async def __aexit__(self, *_: object) -> None: await self.close()
    async def close(self) -> None: self._closed = True

    async def _token(self) -> str:
        value = self._provider() if callable(self._provider) else self._provider
        if inspect.isawaitable(value): value = await value
        if not TOKEN.fullmatch(value): raise OkytesError("invalid_token", "authentication", False, "The token provider returned an invalid credential.")
        return value

    async def _request(self, path: str, method: str = "GET", body: Json | str | None = None, idempotency_key: str | None = None) -> Any:
        if self._closed: raise OkytesError("client_closed", "validation", False, "The client is closed.")
        token = await self._token()
        data = None if body is None else json.dumps(body).encode()
        headers = {"Authorization": f"Bearer {token}", "Accept": "application/json"}
        if data is not None: headers["Content-Type"] = "application/json"
        if idempotency_key is not None: headers["Idempotency-Key"] = idempotency_key
        request = urllib.request.Request(urllib.parse.urljoin(self._endpoint, path), data=data, headers=headers, method=method)
        def send() -> Any:
            try:
                with urllib.request.urlopen(request, timeout=self._timeout, context=ssl.create_default_context()) as response:
                    payload = response.read(16 * 1024 * 1024 + 1)
                    if len(payload) > 16 * 1024 * 1024: raise OkytesError("response_too_large", "internal", False, "Daemon response exceeded the SDK limit.")
                    return None if not payload else json.loads(payload)
            except urllib.error.HTTPError as error:
                try: detail = json.loads(error.read(65536))
                except Exception: detail = {}
                raise OkytesError(detail.get("error", "request_failed"), detail.get("category", "internal"), bool(detail.get("retryable", error.code >= 500)), detail.get("message", f"Okytes request failed ({error.code})."), error.code) from None
        for attempt in range(self._max_retries + 1):
            try:
                return await asyncio.to_thread(send)
            except OkytesError as error:
                if not error.retryable or attempt == self._max_retries: raise
            except (urllib.error.URLError, TimeoutError) as error:
                if attempt == self._max_retries:
                    raise OkytesError("connection_failed", "unavailable", True, "The daemon could not be reached.") from error
            await asyncio.sleep(min(0.25 * 2 ** attempt, 10.0) * (0.5 + (uuid.uuid4().int % 1000) / 1000))

    async def capabilities(self) -> Capabilities: return Capabilities.from_json(await self._request("v1/capabilities"))
    async def supports(self, protocol: str, operation: str) -> bool:
        capabilities = await self.capabilities()
        return any(item.protocol == protocol and operation in item.operations for item in capabilities.protocols)
    async def jobs(self, offset: int = 0) -> Snapshot: return Snapshot.from_json(await self._request(f"v1/jobs?offset={offset}"))
    async def job(self, transfer_id: str) -> Job: return Job.from_json(await self._request(f"v1/jobs/{urllib.parse.quote(transfer_id)}"))
    async def add(self, urls: list[str], destination: str | None = None, allow_duplicates: bool = False) -> list[str]:
        return list(await self._request("v1/jobs", "POST", {"urls": urls, "destination": destination, "allow_duplicates": allow_duplicates}, str(uuid.uuid4())))
    async def download(self, url: str, destination: str | None = None, filename: str | None = None, expected_sha256: str | None = None, conflict: str = "rename", credentials: Json | None = None, idempotency_key: str | None = None) -> str:
        body: Json = {"url": url, "destination": destination, "filename": filename, "sha256": expected_sha256, "conflict": conflict, "credentials": credentials}
        return str(await self._request("v1/submit", "POST", body, idempotency_key or str(uuid.uuid4())))
    async def inspect_media(self, url: str) -> tuple[MediaChoice, ...]:
        return tuple(MediaChoice.from_json(item) for item in await self._request("v1/media/inspect", "POST", url))
    async def inspect_torrent(self, input: Json) -> TorrentMetadata:
        return TorrentMetadata.from_json(await self._request("v1/torrents/inspect", "POST", input))
    async def add_torrent(self, input: Json, idempotency_key: str | None = None) -> str:
        return str(await self._request("v1/torrents", "POST", input, idempotency_key or str(uuid.uuid4())))
    async def torrent(self, transfer_id: str) -> TorrentSnapshot:
        return TorrentSnapshot.from_json(await self._request(f"v1/torrents/{urllib.parse.quote(transfer_id)}"))
    async def authorize_file(self, transfer_id: str, username: str, password: str, confirmed_host_key: str | None = None) -> None:
        await self._request(f"v1/jobs/{urllib.parse.quote(transfer_id)}/credentials", "POST", {"username": username, "password": password, "confirmed_host_key": confirmed_host_key})
    async def send_to_peer(self, input: Json, idempotency_key: str | None = None) -> str:
        return str(await self._request("v1/peers/send", "POST", input, idempotency_key or str(uuid.uuid4())))
    async def control(self, transfer_id: str, action: Json) -> None:
        await self._request(f"v1/jobs/{urllib.parse.quote(transfer_id)}/control", "POST", action)
    async def peer_requests(self, offset: int = 0) -> list[Json]: return list(await self._request(f"v1/peers/requests?offset={offset}"))
    async def peer_decide(self, request_id: str, revision: int, action: str, destination: str | None = None) -> Json:
        return await self._request(f"v1/peers/requests/{urllib.parse.quote(request_id)}/decision", "POST", {"expected_revision": revision, "action": action, "destination": destination})

    async def events(self) -> AsyncIterator[Update]:
        delay = 0.25
        cursor: str | None = None
        while not self._closed:
            token = await self._token()
            headers = {"Authorization": f"Bearer {token}", "Accept": "text/event-stream"}
            if cursor is not None: headers["Last-Event-ID"] = cursor
            request = urllib.request.Request(urllib.parse.urljoin(self._endpoint, "v1/events"), headers=headers)
            try:
                response = await asyncio.to_thread(urllib.request.urlopen, request, None, 3600, context=ssl.create_default_context())
                try:
                    data: list[str] = []
                    while not self._closed:
                        line = await asyncio.to_thread(response.readline, 131073)
                        if len(line) > 131072: raise OkytesError("event_too_large", "internal", False, "Event frame exceeded the SDK limit.")
                        if not line: break
                        text = line.decode("utf-8").rstrip("\r\n")
                        if text.startswith("data:"): data.append(text[5:].lstrip())
                        elif text.startswith("id:"): cursor = text[3:].strip()
                        elif not text and data:
                            item = json.loads("\n".join(data)); data.clear()
                            yield Update(item["kind"], item.get("value"))
                finally: response.close()
                delay = 0.25
            except Exception:
                if self._closed: return
                yield Update("Resync")
                await asyncio.sleep(delay)
                delay = min(delay * 2, 10.0)
