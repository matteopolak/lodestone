#!/usr/bin/env python3
"""Validate and recover the compact streaming parity digest ledger.

The ledger deliberately contains only one full SHA-256 digest per committed
coordinate.  Packet bytes stay in the pipe and are retained by the comparator
only for a mismatch, so a long comparison does not create a second world or a
packet corpus on disk.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import pathlib
import struct
import tempfile

HEADER = 256
WIDTH = 32
STREAM_MAGIC = b"LWS26S01"
LEDGER_MAGIC = b"LWL26D01"
VERSION = 1
ALGORITHM = 2
SCHEMA = 1
PROTOCOL = 776
SEED = 42
DOMAIN = b"lodestone.worldgen.streaming-parity/v1/raw-packet"


class LedgerError(ValueError):
    """A ledger is incomplete, inconsistent, or not from this stream schema."""


def _u16(data: bytes, offset: int) -> int:
    return struct.unpack_from(">H", data, offset)[0]


def _u32(data: bytes, offset: int) -> int:
    return struct.unpack_from(">I", data, offset)[0]


def _i32(data: bytes, offset: int) -> int:
    return struct.unpack_from(">i", data, offset)[0]


def _i64(data: bytes, offset: int) -> int:
    return struct.unpack_from(">q", data, offset)[0]


def _u64(data: bytes, offset: int) -> int:
    return struct.unpack_from(">Q", data, offset)[0]


def parse_header(raw: bytes, path: pathlib.Path, expected_magic: bytes = LEDGER_MAGIC) -> dict[str, object]:
    if len(raw) != HEADER:
        raise LedgerError(f"{path}: expected {HEADER}-byte header, got {len(raw)}")
    if raw[:8] != expected_magic:
        raise LedgerError(f"{path}: wrong magic {raw[:8]!r}")
    values = {
        "version": _u16(raw, 8),
        "header": _u16(raw, 10),
        "algorithm": _u16(raw, 12),
        "schema": _u16(raw, 14),
        "protocol": _u32(raw, 16),
        "seed": _i64(raw, 20),
        "cx0": _i32(raw, 28),
        "cx1": _i32(raw, 32),
        "cz0": _i32(raw, 36),
        "cz1": _i32(raw, 40),
        "count": _u64(raw, 44),
        "width": _u16(raw, 52),
        "reserved": _u16(raw, 54),
        "domain": raw[56:88],
        "dimension": raw[88:120],
        "server": raw[120:152],
        "oracle": raw[152:184],
        "start": _u64(raw, 184),
        "payload_digest": raw[192:224],
    }
    if (
        values["version"],
        values["header"],
        values["algorithm"],
        values["schema"],
        values["protocol"],
        values["seed"],
        values["width"],
        values["reserved"],
    ) != (VERSION, HEADER, ALGORITHM, SCHEMA, PROTOCOL, SEED, WIDTH, 0):
        raise LedgerError(f"{path}: stream schema/protocol fields differ")
    if values["domain"] != hashlib.sha256(DOMAIN).digest():
        raise LedgerError(f"{path}: stream domain differs")
    if any(raw[224:]):
        raise LedgerError(f"{path}: reserved header bytes are non-zero")
    if values["cx0"] > values["cx1"] or values["cz0"] > values["cz1"]:
        raise LedgerError(f"{path}: coordinate bounds are reversed")
    expected = (values["cx1"] - values["cx0"] + 1) * (values["cz1"] - values["cz0"] + 1)
    if values["count"] != expected:
        raise LedgerError(f"{path}: count {values['count']} differs from bounds ({expected})")
    return values


def _cursor_path(path: pathlib.Path) -> pathlib.Path:
    return pathlib.Path(f"{path}.cursor")


def read_cursor(path: pathlib.Path) -> int | None:
    cursor = _cursor_path(path)
    if not cursor.exists():
        return None
    fields: dict[str, str] = {}
    for line in cursor.read_text(encoding="ascii").splitlines():
        key, separator, value = line.partition("=")
        if not separator or not key or key in fields:
            raise LedgerError(f"{cursor}: malformed cursor")
        fields[key] = value
    if set(fields) != {"count", "ledger_bytes"}:
        raise LedgerError(f"{cursor}: cursor fields differ")
    try:
        count = int(fields["count"])
        ledger_bytes = int(fields["ledger_bytes"])
    except ValueError as error:
        raise LedgerError(f"{cursor}: cursor values are not integers") from error
    if count < 0 or ledger_bytes != HEADER + count * WIDTH:
        raise LedgerError(f"{cursor}: cursor geometry differs")
    return count


def inspect(path: pathlib.Path) -> tuple[dict[str, object], int, bytes]:
    if not path.is_file():
        raise LedgerError(f"{path}: ledger is missing")
    size = path.stat().st_size
    if size < HEADER or (size - HEADER) % WIDTH:
        raise LedgerError(f"{path}: payload is truncated or has trailing bytes")
    with path.open("rb") as source:
        header = parse_header(source.read(HEADER), path)
        records = (size - HEADER) // WIDTH
        if records > header["count"]:
            raise LedgerError(f"{path}: {records} records exceed {header['count']}")
        payload = source.read()
    if header["payload_digest"] != bytes(32):
        if records != header["count"]:
            raise LedgerError(f"{path}: completed payload checksum on a partial ledger")
        if hashlib.sha256(payload).digest() != header["payload_digest"]:
            raise LedgerError(f"{path}: payload checksum differs")
    cursor = read_cursor(path)
    if cursor is not None:
        if cursor > records:
            raise LedgerError(f"{path}: cursor {cursor} is ahead of {records} durable records")
        if cursor < records:
            raise LedgerError(f"{path}: {records - cursor} records trail the durable cursor")
    return header, records, payload


def repair(path: pathlib.Path) -> int:
    """Discard only records written after a durable cursor checkpoint."""
    header, records, _ = inspect_without_cursor(path)
    cursor = read_cursor(path)
    if cursor is None:
        cursor = records
        write_cursor(path, cursor)
        return cursor
    if cursor > records:
        raise LedgerError(f"{path}: cursor {cursor} is ahead of {records} durable records")
    if cursor < records:
        with path.open("r+b") as target:
            target.truncate(HEADER + cursor * WIDTH)
        records = cursor
    write_cursor(path, records)
    return records


def inspect_without_cursor(path: pathlib.Path) -> tuple[dict[str, object], int, bytes]:
    if not path.is_file():
        raise LedgerError(f"{path}: ledger is missing")
    size = path.stat().st_size
    if size < HEADER or (size - HEADER) % WIDTH:
        raise LedgerError(f"{path}: payload is truncated or has trailing bytes")
    with path.open("rb") as source:
        header = parse_header(source.read(HEADER), path)
        records = (size - HEADER) // WIDTH
        if records > header["count"]:
            raise LedgerError(f"{path}: {records} records exceed {header['count']}")
        payload = source.read()
    return header, records, payload


def write_cursor(path: pathlib.Path, count: int) -> None:
    cursor = _cursor_path(path)
    temporary = pathlib.Path(f"{cursor}.tmp")
    with temporary.open("w", encoding="ascii") as output:
        output.write(f"count={count}\nledger_bytes={HEADER + count * WIDTH}\n")
        output.flush()
        os.fsync(output.fileno())
    temporary.replace(cursor)
    directory = temporary.parent
    directory_fd = os.open(directory, os.O_RDONLY)
    try:
        os.fsync(directory_fd)
    finally:
        os.close(directory_fd)


def command_cursor(path: pathlib.Path) -> None:
    print(repair(path))


def command_validate(path: pathlib.Path) -> None:
    header, records, payload = inspect(path)
    digest = hashlib.sha256(payload).hexdigest()
    print(
        f"ok {path}: records={records}/{header['count']} bounds=({header['cx0']},{header['cz0']}).."
        f"({header['cx1']},{header['cz1']}) payload_sha256={digest}"
    )


def selftest() -> None:
    with tempfile.TemporaryDirectory(prefix="stream-ledger-") as directory:
        path = pathlib.Path(directory) / "sample.ledger"
        header = bytearray(HEADER)
        header[:8] = LEDGER_MAGIC
        struct.pack_into(">HHHHIq", header, 8, VERSION, HEADER, ALGORITHM, SCHEMA, PROTOCOL, SEED)
        struct.pack_into(">iiiiQHH", header, 28, -1, 0, -1, -1, 2, WIDTH, 0)
        header[56:88] = hashlib.sha256(DOMAIN).digest()
        header[88:120] = hashlib.sha256(b"minecraft:overworld").digest()
        header[120:152] = bytes(range(32))
        header[152:184] = bytes(range(32, 64))
        path.write_bytes(header + b"a" * WIDTH * 2)
        write_cursor(path, 2)
        assert repair(path) == 2
        command_validate(path)

        complete = pathlib.Path(directory) / "complete.ledger"
        complete_header = bytearray(header)
        complete_payload = b"b" * WIDTH * 2
        complete_header[192:224] = hashlib.sha256(complete_payload).digest()
        complete.write_bytes(complete_header + complete_payload)
        write_cursor(complete, 2)
        command_validate(complete)
        corrupted = bytearray(complete.read_bytes())
        corrupted[-1] ^= 1
        complete.write_bytes(corrupted)
        try:
            inspect(complete)
        except LedgerError as error:
            assert "checksum" in str(error)
        else:
            raise AssertionError("payload-tamper control accepted")

        # A partial ledger has no final checksum yet, so recovery is driven by
        # the durable cursor and the comparator's Rust-prefix digest check.
        header[192:224] = bytes(32)
        path.write_bytes(header + b"a" * WIDTH * 2)
        write_cursor(path, 2)
        write_cursor(path, 1)
        assert repair(path) == 1
        assert path.stat().st_size == HEADER + WIDTH
        write_cursor(path, 2)
        try:
            inspect(path)
        except LedgerError as error:
            assert "ahead" in str(error)
        else:
            raise AssertionError("cursor-ahead control accepted")
    print("stream ledger selftest ok")


def main() -> None:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    for name in ("cursor", "validate"):
        subparser = subparsers.add_parser(name)
        subparser.add_argument("ledger", type=pathlib.Path)
    subparsers.add_parser("selftest")
    args = parser.parse_args()
    if args.command == "selftest":
        selftest()
    elif args.command == "cursor":
        command_cursor(args.ledger)
    else:
        command_validate(args.ledger)


if __name__ == "__main__":
    main()
