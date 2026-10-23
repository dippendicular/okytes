from __future__ import annotations

import asyncio
import inspect
import json
import re
import ssl
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import AsyncIterator, Awaitable, Callable
from typing import Any

from .models import Capabilities, Json, Snapshot, Update

TokenProvider = str | Callable[[], str | Awaitable[str]]
TOKEN = re.compile(r"^[a-f0-9-]{36}\.[a-f0-9]{64}$")

class OkytesError(Exception):
    def __init__(self, code: str, category: str, retryable: bool, message: str, status: int | None = None) -> None:
        super().__init__(message)
        self.code, self.category, self.retryable, self.status = code, category, retryable, status

class OkytesClient:
    def __init__(self, endpoint: str, token: TokenProvider, timeout: float = 15.0) -> None:
        parsed = urllib.parse.urlsplit(endpoint)
        local = parsed.hostname in {"localhost", "127.0.0.1", "::1"}
        if parsed.scheme not in {"http", "https"} or (parsed.scheme == "http" and not local) or parsed.username or parsed.password or parsed.path != "/" or parsed.query or parsed.fragment:
            raise ValueError("use loopback HTTP or verified HTTPS with a root endpoint")
        self._endpoint, self._provider, self._timeout = endpoint, token, timeout
        self._closed = False

    async def __aenter__(self) -> "OkytesClient": return self
    async def __aexit__(self, *_: object) -> None: await self.close()
    async def close(self) -> None: self._closed = True

    async def _token(self) -> str:
        value = self._provider() if callable(self._provider) else self._provider
        if inspect.isawaitable(value): value = await value
        if not TOKEN.fullmatch(value): raise OkytesError("invalid_token", "authentication", False, "The token provider returned an invalid credential.")
        return value

    async def _request(self, path: str, method: str = "GET", body: Json | None = None) -> Any:
        if self._closed: raise OkytesError("client_closed", "validation", False, "The client is closed.")
        token = await self._token()
        data = None if body is None else json.dumps(body).encode()
        headers = {"Authorization": f"Bearer {token}", "Accept": "application/json"}
        if data is not None: headers["Content-Type"] = "application/json"
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
        return await asyncio.to_thread(send)

    async def capabilities(self) -> Capabilities: return Capabilities.from_json(await self._request("v1/capabilities"))
    async def jobs(self, offset: int = 0) -> Snapshot: return Snapshot.from_json(await self._request(f"v1/jobs?offset={offset}"))
    async def add(self, urls: list[str], destination: str | None = None, allow_duplicates: bool = False) -> list[str]:
        return list(await self._request("v1/jobs", "POST", {"urls": urls, "destination": destination, "allow_duplicates": allow_duplicates}))
    async def control(self, transfer_id: str, action: Json) -> None:
        await self._request(f"v1/jobs/{urllib.parse.quote(transfer_id)}/control", "POST", action)
    async def peer_requests(self, offset: int = 0) -> list[Json]: return list(await self._request(f"v1/peers/requests?offset={offset}"))
    async def peer_decide(self, request_id: str, revision: int, action: str, destination: str | None = None) -> Json:
        return await self._request(f"v1/peers/requests/{urllib.parse.quote(request_id)}/decision", "POST", {"expected_revision": revision, "action": action, "destination": destination})

    async def events(self) -> AsyncIterator[Update]:
        delay = 0.25
        while not self._closed:
            token = await self._token()
            request = urllib.request.Request(urllib.parse.urljoin(self._endpoint, "v1/events"), headers={"Authorization": f"Bearer {token}", "Accept": "text/event-stream"})
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
