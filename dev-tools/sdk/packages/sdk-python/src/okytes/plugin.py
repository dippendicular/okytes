"""Plugin preview.

A plugin is a separate process using its own permission-restricted daemon
token, issued by ``okytes-cli plugin-grant``. The daemon enforces permissions
on every request; the manifest declares them so users can review a plugin
before granting access.
"""
from __future__ import annotations

import json
import os
import re
from dataclasses import dataclass
from pathlib import Path

from .client import OkytesClient, OkytesError

MANIFEST = "okytes-plugin.json"
PERMISSIONS = ("jobs:read", "jobs:add", "jobs:control", "events:read", "media:inspect", "settings:read")
_ID = re.compile(r"^[a-z0-9-]+(\.[a-z0-9-]+)+$")
_CONTROL = re.compile(r"[\x00-\x1f\x7f]")
_FIELDS = {"schema", "id", "name", "version", "description", "permissions"}


def _invalid(message: str) -> OkytesError:
    return OkytesError("invalid_manifest", "validation", False, f"plugin manifest: {message}")


@dataclass(frozen=True)
class PluginManifest:
    id: str
    name: str
    version: str
    permissions: tuple[str, ...]
    description: str = ""

    @classmethod
    def validate(cls, value: object) -> "PluginManifest":
        """Validate with the same rules as the Rust SDK and CLI."""
        if not isinstance(value, dict):
            raise _invalid("expected a JSON object")
        unknown = set(value) - _FIELDS
        if unknown:
            raise _invalid(f"unknown field {sorted(unknown)[0]}")
        if value.get("schema") != 1:
            raise _invalid("unsupported schema; expected 1")
        ident = value.get("id")
        if not isinstance(ident, str) or len(ident) > 100 or not _ID.fullmatch(ident):
            raise _invalid("id must be reverse-domain lowercase, e.g. org.example.tool")
        for field, limit in (("name", 64), ("version", 32), ("description", 200)):
            text = value.get(field, "" if field == "description" else None)
            if not isinstance(text, str) or len(text) > limit or _CONTROL.search(text):
                raise _invalid(f"{field} is missing, too long or has control characters")
            if field != "description" and not text.strip():
                raise _invalid(f"{field} is required")
        permissions = value.get("permissions")
        if (
            not isinstance(permissions, list)
            or not permissions
            or len(set(permissions)) != len(permissions)
            or any(p not in PERMISSIONS for p in permissions)
        ):
            raise _invalid(f"permissions must list each of {', '.join(PERMISSIONS)} at most once")
        return cls(ident, value["name"], value["version"], tuple(permissions), value.get("description", ""))

    @classmethod
    def load(cls, path: str | os.PathLike[str]) -> "PluginManifest":
        """Load a manifest file, or a directory containing okytes-plugin.json."""
        file = Path(path)
        if file.is_dir():
            file = file / MANIFEST
        text = file.read_text(encoding="utf-8")
        if len(text) > 64 * 1024:
            raise _invalid("file exceeds 64 KiB")
        try:
            data = json.loads(text)
        except json.JSONDecodeError as error:
            raise _invalid(f"invalid JSON: {error.msg}") from None
        return cls.validate(data)


class Plugin:
    """A manifest plus a client authenticated with the plugin's own token."""

    def __init__(self, manifest: PluginManifest, client: OkytesClient) -> None:
        self.manifest, self.client = manifest, client

    @classmethod
    def from_environment(cls, manifest: PluginManifest) -> "Plugin":
        """Connect with OKYTES_DAEMON_URL and OKYTES_PLUGIN_TOKEN_FILE."""
        endpoint = os.environ.get("OKYTES_DAEMON_URL")
        token_file = os.environ.get("OKYTES_PLUGIN_TOKEN_FILE")
        if not endpoint or not token_file:
            raise OkytesError(
                "plugin_environment", "validation", False,
                "Set OKYTES_DAEMON_URL and OKYTES_PLUGIN_TOKEN_FILE (from okytes-cli plugin-grant).",
            )
        # Re-read on each request so a re-granted token is picked up.
        return cls(manifest, OkytesClient(endpoint, lambda: Path(token_file).read_text(encoding="utf-8").strip()))

    def require(self, permission: str) -> None:
        """Fail early when calling outside the declared permissions."""
        if permission not in self.manifest.permissions:
            raise OkytesError(
                "undeclared_permission", "authorization", False,
                f"plugin {self.manifest.id} did not declare {permission}",
            )
