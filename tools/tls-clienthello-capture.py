#!/usr/bin/env python3
"""Capture and compare normalized TLS ClientHello fingerprints.

This helper starts a local TCP listener, reads the first TLS ClientHello from a
client, and writes a normalized JSON fingerprint. It intentionally does not
finish the TLS handshake; clients are expected to fail after their hello is
captured.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import socket
import struct
import subprocess
import sys
import time
from pathlib import Path
from typing import Any


GREASE_VALUES = {
    0x0A0A,
    0x1A1A,
    0x2A2A,
    0x3A3A,
    0x4A4A,
    0x5A5A,
    0x6A6A,
    0x7A7A,
    0x8A8A,
    0x9A9A,
    0xAAAA,
    0xBABA,
    0xCACA,
    0xDADA,
    0xEAEA,
    0xFAFA,
}


def is_grease(value: int) -> bool:
    return value in GREASE_VALUES


def u8(data: bytes, offset: int) -> tuple[int, int]:
    if offset + 1 > len(data):
        raise ValueError("unexpected end of data")
    return data[offset], offset + 1


def u16(data: bytes, offset: int) -> tuple[int, int]:
    if offset + 2 > len(data):
        raise ValueError("unexpected end of data")
    return struct.unpack_from("!H", data, offset)[0], offset + 2


def u24(data: bytes, offset: int) -> tuple[int, int]:
    if offset + 3 > len(data):
        raise ValueError("unexpected end of data")
    return int.from_bytes(data[offset : offset + 3], "big"), offset + 3


def read_exact(conn: socket.socket, size: int) -> bytes:
    chunks: list[bytes] = []
    remaining = size
    while remaining:
        chunk = conn.recv(remaining)
        if not chunk:
            break
        chunks.append(chunk)
        remaining -= len(chunk)
    data = b"".join(chunks)
    if len(data) != size:
        raise ValueError(f"expected {size} bytes, got {len(data)}")
    return data


def parse_vector_u16(data: bytes) -> list[int]:
    if len(data) % 2 != 0:
        raise ValueError("u16 vector length is not even")
    return [struct.unpack_from("!H", data, idx)[0] for idx in range(0, len(data), 2)]


def parse_client_hello(record: bytes, source: dict[str, Any]) -> dict[str, Any]:
    offset = 0
    content_type, offset = u8(record, offset)
    record_version, offset = u16(record, offset)
    record_len, offset = u16(record, offset)
    if content_type != 22:
        raise ValueError(f"expected TLS handshake record, got content_type={content_type}")
    if offset + record_len > len(record):
        raise ValueError("record length exceeds captured bytes")

    body = record[offset : offset + record_len]
    offset = 0
    handshake_type, offset = u8(body, offset)
    handshake_len, offset = u24(body, offset)
    if handshake_type != 1:
        raise ValueError(f"expected ClientHello, got handshake_type={handshake_type}")
    hello = body[offset : offset + handshake_len]

    offset = 0
    legacy_version, offset = u16(hello, offset)
    random_bytes = hello[offset : offset + 32]
    offset += 32
    session_id_len, offset = u8(hello, offset)
    session_id = hello[offset : offset + session_id_len]
    offset += session_id_len

    cipher_len, offset = u16(hello, offset)
    cipher_bytes = hello[offset : offset + cipher_len]
    offset += cipher_len
    cipher_suites_raw = parse_vector_u16(cipher_bytes)
    cipher_suites = [value for value in cipher_suites_raw if not is_grease(value)]

    compression_len, offset = u8(hello, offset)
    compression_methods = list(hello[offset : offset + compression_len])
    offset += compression_len

    extensions: list[dict[str, Any]] = []
    extensions_raw: list[int] = []
    alpn: list[str] = []
    supported_versions_raw: list[int] = []
    supported_groups_raw: list[int] = []
    ec_point_formats: list[int] = []
    signature_algorithms: list[int] = []
    server_name = None

    if offset < len(hello):
        extensions_len, offset = u16(hello, offset)
        extensions_end = offset + extensions_len
        if extensions_end > len(hello):
            raise ValueError("extension length exceeds ClientHello")
        while offset < extensions_end:
            ext_type, offset = u16(hello, offset)
            ext_len, offset = u16(hello, offset)
            ext_data = hello[offset : offset + ext_len]
            offset += ext_len
            extensions_raw.append(ext_type)
            if is_grease(ext_type):
                continue
            extensions.append({"type": ext_type, "length": ext_len})

            if ext_type == 0:
                server_name = parse_sni(ext_data)
            elif ext_type == 10:
                supported_groups_raw = parse_len_prefixed_u16_vector(ext_data)
            elif ext_type == 11:
                ec_point_formats = parse_len_prefixed_u8_vector(ext_data)
            elif ext_type == 13:
                signature_algorithms = parse_len_prefixed_u16_vector(ext_data)
            elif ext_type == 16:
                alpn = parse_alpn(ext_data)
            elif ext_type == 43:
                supported_versions_raw = parse_len_prefixed_u8_u16_vector(ext_data)

    extension_types = [entry["type"] for entry in extensions]
    supported_versions = [
        value for value in supported_versions_raw if not is_grease(value)
    ]
    supported_groups = [value for value in supported_groups_raw if not is_grease(value)]
    ja3 = ",".join(
        [
            str(legacy_version),
            "-".join(str(value) for value in cipher_suites),
            "-".join(str(value) for value in extension_types),
            "-".join(str(value) for value in supported_groups),
            "-".join(str(value) for value in ec_point_formats),
        ]
    )

    return {
        "schema_version": 1,
        "source": source,
        "record": {
            "content_type": content_type,
            "record_version": record_version,
            "record_length": record_len,
        },
        "client_hello": {
            "legacy_version": legacy_version,
            "session_id_len": len(session_id),
            "random_len": len(random_bytes),
            "cipher_suites": cipher_suites,
            "cipher_suites_raw": cipher_suites_raw,
            "compression_methods": compression_methods,
            "extensions": extensions,
            "extensions_raw": extensions_raw,
            "server_name": server_name,
            "alpn": alpn,
            "supported_versions": supported_versions,
            "supported_versions_raw": supported_versions_raw,
            "supported_groups": supported_groups,
            "supported_groups_raw": supported_groups_raw,
            "ec_point_formats": ec_point_formats,
            "signature_algorithms": signature_algorithms,
        },
        "ja3": ja3,
        "ja3_hash": hashlib.md5(ja3.encode("ascii")).hexdigest(),
    }


def parse_len_prefixed_u16_vector(data: bytes) -> list[int]:
    length, offset = u16(data, 0)
    end = offset + length
    if end > len(data):
        raise ValueError("length-prefixed u16 vector exceeds extension data")
    return parse_vector_u16(data[offset:end])


def parse_len_prefixed_u8_u16_vector(data: bytes) -> list[int]:
    length, offset = u8(data, 0)
    end = offset + length
    if end > len(data):
        raise ValueError("length-prefixed u8/u16 vector exceeds extension data")
    return parse_vector_u16(data[offset:end])


def parse_len_prefixed_u8_vector(data: bytes) -> list[int]:
    length, offset = u8(data, 0)
    end = offset + length
    if end > len(data):
        raise ValueError("length-prefixed u8 vector exceeds extension data")
    return list(data[offset:end])


def parse_alpn(data: bytes) -> list[str]:
    length, offset = u16(data, 0)
    end = offset + length
    if end > len(data):
        raise ValueError("ALPN length exceeds extension data")
    protocols: list[str] = []
    while offset < end:
        item_len, offset = u8(data, offset)
        item = data[offset : offset + item_len]
        offset += item_len
        protocols.append(item.decode("ascii", errors="replace"))
    return protocols


def parse_sni(data: bytes) -> str | None:
    list_len, offset = u16(data, 0)
    end = offset + list_len
    if end > len(data):
        raise ValueError("SNI list exceeds extension data")
    while offset < end:
        name_type, offset = u8(data, offset)
        name_len, offset = u16(data, offset)
        name = data[offset : offset + name_len]
        offset += name_len
        if name_type == 0:
            try:
                return name.decode("idna")
            except UnicodeError:
                return name.decode("ascii", errors="replace")
    return None


def run_capture(args: argparse.Namespace) -> int:
    out_path = Path(args.out)
    command = args.command
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind((args.host, args.port))
        listener.listen(1)
        host, port = listener.getsockname()
        url = f"https://{host}:{port}"
        print(f"TLS_CAPTURE_URL={url}", flush=True)

        process = None
        launched_command = None
        if command:
            launched_command = [
                part.replace("{host}", str(host))
                .replace("{port}", str(port))
                .replace("{url}", url)
                for part in command
            ]
            env = os.environ.copy()
            env["TLS_CAPTURE_HOST"] = str(host)
            env["TLS_CAPTURE_PORT"] = str(port)
            env["TLS_CAPTURE_URL"] = url
            process = subprocess.Popen(launched_command, env=env)

        listener.settimeout(args.timeout)
        try:
            conn, addr = listener.accept()
        except socket.timeout as exc:
            if process is not None:
                process.terminate()
            raise SystemExit(f"timed out waiting for ClientHello: {exc}") from exc

        with conn:
            conn.settimeout(args.timeout)
            header = read_exact(conn, 5)
            record_len = struct.unpack_from("!H", header, 3)[0]
            payload = read_exact(conn, record_len)
            fingerprint = parse_client_hello(
                header + payload,
                {
                    "remote_addr": addr[0],
                    "remote_port": addr[1],
                    "captured_at_unix_secs": int(time.time()),
                    "command": launched_command,
                },
            )

    if process is not None:
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            process.terminate()

    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(fingerprint, indent=2, sort_keys=True) + "\n")
    print(json.dumps(fingerprint, indent=2, sort_keys=True))
    return 0


def comparable(value: dict[str, Any]) -> dict[str, Any]:
    hello = value["client_hello"]
    return {
        "record_version": value["record"]["record_version"],
        "legacy_version": hello["legacy_version"],
        "cipher_suites": hello["cipher_suites"],
        "extension_types": [entry["type"] for entry in hello["extensions"]],
        "supported_versions": hello["supported_versions"],
        "supported_groups": hello["supported_groups"],
        "ec_point_formats": hello["ec_point_formats"],
        "signature_algorithms": hello["signature_algorithms"],
        "alpn": hello["alpn"],
        "ja3": value["ja3"],
        "ja3_hash": value["ja3_hash"],
    }


def run_compare(args: argparse.Namespace) -> int:
    left = json.loads(Path(args.left).read_text())
    right = json.loads(Path(args.right).read_text())
    left_cmp = comparable(left)
    right_cmp = comparable(right)
    if left_cmp == right_cmp:
        print("MATCH")
        print(json.dumps(left_cmp, indent=2, sort_keys=True))
        return 0

    print("MISMATCH")
    keys = sorted(set(left_cmp) | set(right_cmp))
    diff: dict[str, dict[str, Any]] = {}
    for key in keys:
        if left_cmp.get(key) != right_cmp.get(key):
            diff[key] = {"left": left_cmp.get(key), "right": right_cmp.get(key)}
    print(json.dumps(diff, indent=2, sort_keys=True))
    return 1


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="cmd", required=True)

    capture = subcommands.add_parser("capture")
    capture.add_argument("--host", default="127.0.0.1")
    capture.add_argument("--port", type=int, default=0)
    capture.add_argument("--timeout", type=float, default=20)
    capture.add_argument("--out", required=True)
    capture.add_argument(
        "command",
        nargs=argparse.REMAINDER,
        help="optional command to run after the listener starts",
    )
    capture.set_defaults(func=run_capture)

    compare = subcommands.add_parser("compare")
    compare.add_argument("left")
    compare.add_argument("right")
    compare.set_defaults(func=run_compare)
    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    if getattr(args, "command", None) and args.command[:1] == ["--"]:
        args.command = args.command[1:]
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
