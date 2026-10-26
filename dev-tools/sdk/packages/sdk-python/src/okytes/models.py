from dataclasses import dataclass
from typing import Any, Literal

Json = dict[str, Any]

@dataclass(frozen=True)
class ProtocolCapability:
    protocol: str
    operations: tuple[str, ...]
    resume: bool
    integrity: bool
    approval_required: bool
    credential_kind: str | None = None

@dataclass(frozen=True)
class Capabilities:
    api_version: str
    sdk_compatibility: str
    protocols: tuple[ProtocolCapability, ...]
    event_delivery: str
    max_page_size: int
    max_batch_size: int
    idempotency_retention_seconds: int
    destination_semantics: str
    credential_policy: str

    @classmethod
    def from_json(cls, value: Json) -> "Capabilities":
        return cls(
            api_version=str(value["api_version"]),
            sdk_compatibility=str(value["sdk_compatibility"]),
            protocols=tuple(ProtocolCapability(**item) for item in value["protocols"]),
            event_delivery=str(value["event_delivery"]),
            max_page_size=int(value["max_page_size"]),
            max_batch_size=int(value["max_batch_size"]),
            idempotency_retention_seconds=int(value["idempotency_retention_seconds"]),
            destination_semantics=str(value["destination_semantics"]),
            credential_policy=str(value["credential_policy"]),
        )

@dataclass(frozen=True)
class Job:
    id: str
    name: str
    state: str
    protocol: str
    destination: str
    bytes: float
    total: float | None
    speed: float
    error: str | None

    @classmethod
    def from_json(cls, value: Json) -> "Job":
        return cls(**{field: value.get(field) for field in cls.__dataclass_fields__})

@dataclass(frozen=True)
class Snapshot:
    revision: str
    jobs: tuple[Job, ...]
    next: int | None

    @classmethod
    def from_json(cls, value: Json) -> "Snapshot":
        return cls(str(value["revision"]), tuple(Job.from_json(job) for job in value["jobs"]), value.get("next"))

@dataclass(frozen=True)
class Update:
    kind: Literal["Changed", "Resync"]
    value: Json | None = None

@dataclass(frozen=True)
class MediaChoice:
    id: str
    label: str
    url: str
    kind: str
    extension: str
    @classmethod
    def from_json(cls, value: Json) -> "MediaChoice": return cls(**value)

@dataclass(frozen=True)
class TorrentMetadata:
    info_hash: str
    name: str
    files: tuple[Json, ...]
    pieces: int
    piece_bytes: int
    trackers: tuple[str, ...]
    @classmethod
    def from_json(cls, value: Json) -> "TorrentMetadata":
        return cls(str(value["info_hash"]), str(value["name"]), tuple(value["files"]), int(value["pieces"]), int(value["piece_bytes"]), tuple(value["trackers"]))

@dataclass(frozen=True)
class TorrentSnapshot:
    id: str
    activity: str
    metadata: TorrentMetadata | None
    verified: tuple[bool, ...]
    peers: str
    error: str | None
    uploaded_bytes: int
    options: Json
    tracker_status: str
    @classmethod
    def from_json(cls, value: Json) -> "TorrentSnapshot":
        metadata = value.get("metadata")
        return cls(str(value["id"]), str(value["activity"]), TorrentMetadata.from_json(metadata) if metadata else None, tuple(value["verified"]), str(value["peers"]), value.get("error"), int(value["uploaded_bytes"]), value["options"], str(value["tracker_status"]))

