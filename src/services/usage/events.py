"""
Usage 事件定义与序列化工具（用于 Redis Streams）
"""

from __future__ import annotations

import json
import time
from dataclasses import dataclass
from enum import Enum
from typing import Any

import msgpack
from msgpack.exceptions import OutOfData

USAGE_EVENT_VERSION = 1


class UsageEventType(str, Enum):
    STREAMING = "streaming"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"


def now_ms() -> int:
    return int(time.time() * 1000)


def _sanitize_value(value: Any) -> Any:
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    if isinstance(value, dict):
        return {str(k): _sanitize_value(v) for k, v in value.items()}
    if isinstance(value, list):
        return [_sanitize_value(item) for item in value]
    return str(value)


def sanitize_payload(data: dict[str, Any]) -> dict[str, Any]:
    return {str(k): _sanitize_value(v) for k, v in data.items()}


def _decode_payload(raw: Any) -> dict[str, Any]:
    """兼容解码：优先 msgpack，回退旧 JSON。

    统一将输入归一化为 bytes 后走单一解码路径：msgpack → JSON fallback。
    str 输入来自 decode_responses=True + surrogateescape 的 Redis 客户端，
    通过 surrogateescape 可无损还原回原始 bytes。
    """
    if isinstance(raw, str):
        raw = raw.encode("utf-8", errors="surrogateescape")

    if not isinstance(raw, (bytes, bytearray, memoryview)):
        raise ValueError("Invalid payload field in usage event")

    payload_bytes = bytes(raw)

    # 新格式：msgpack
    try:
        payload = msgpack.unpackb(payload_bytes, raw=False)
    except (ValueError, OutOfData, TypeError):
        # 兼容旧格式：JSON bytes（含 surrogateescape 还原后的纯 UTF-8 JSON）
        try:
            payload = json.loads(payload_bytes.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError, TypeError) as exc:
            raise ValueError("Invalid payload field in usage event") from exc

    if not isinstance(payload, dict):
        raise ValueError("Invalid payload field in usage event")
    return payload


@dataclass
class UsageEvent:
    event_type: UsageEventType
    request_id: str
    timestamp_ms: int
    data: dict[str, Any]

    def to_stream_fields(self) -> dict[str, bytes]:
        """序列化为 Redis Stream 字段。

        该函数返回 bytes payload，要求读写 usage queue 的 Redis 客户端使用
        decode_responses=True，并以 surrogateescape 做 UTF-8 编解码，
        以保证 bytes <-> str 往返无损。
        """
        payload = {
            "v": USAGE_EVENT_VERSION,
            "type": self.event_type.value,
            "request_id": self.request_id,
            "timestamp_ms": self.timestamp_ms,
            # 兜底清洗，避免 metadata 中混入非 JSON 类型导致队列写入失败。
            "data": sanitize_payload(self.data),
        }
        return {"payload": msgpack.packb(payload, use_bin_type=True)}

    @classmethod
    def from_stream_fields(cls, fields: dict[str, Any]) -> UsageEvent:
        raw = fields.get("payload")
        if not raw:
            raise ValueError("Missing payload field in usage event")
        payload = _decode_payload(raw)
        event_type = UsageEventType(payload["type"])
        return cls(
            event_type=event_type,
            request_id=payload["request_id"],
            timestamp_ms=int(payload.get("timestamp_ms", 0)),
            data=payload.get("data", {}) or {},
        )


def build_usage_event(
    *,
    event_type: UsageEventType,
    request_id: str,
    data: dict[str, Any],
    timestamp_ms: int | None = None,
) -> UsageEvent:
    return UsageEvent(
        event_type=event_type,
        request_id=request_id,
        timestamp_ms=timestamp_ms or now_ms(),
        data=data,
    )
