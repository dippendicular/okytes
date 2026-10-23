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

@dataclass(frozen=True)
class Capabilities:
    api_version: str
    sdk_compatibility: str
    protocols: tuple[ProtocolCapability, ...]
    event_delivery: str
    max_page_size: int
    max_batch_size: int

    @classmethod
    def from_json(cls, value: Json) -> "Capabilities":
        return cls(
            api_version=str(value["api_version"]),
            sdk_compatibility=str(value["sdk_compatibility"]),
            protocols=tuple(ProtocolCapability(**item) for item in value["protocols"]),
            event_delivery=str(value["event_delivery"]),
            max_page_size=int(value["max_page_size"]),
            max_batch_size=int(value["max_batch_size"]),
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

