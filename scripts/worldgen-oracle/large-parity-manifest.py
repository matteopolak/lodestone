#!/usr/bin/env python3
"""Validate and deterministically merge frozen-world parity manifests."""
import argparse, hashlib, mmap, pathlib, struct, sys, tempfile

MAGIC = b"LWP26P03"
MAGIC_V4 = b"LWP26P04"
MAGIC_V5 = b"LWP26P05"
MAGIC_V6 = b"LWP26P06"
MAGIC_V7 = b"LWP26P07"
LIGHT_FREE_AUDIT_MAGIC = b"LWP26A07"
HEADER = 256
WIDTH = 32
RAW_WIDTH = 2
# Header bytes 70..72 record the terrain-adaptation scope. Authenticated
# full-world records use the production scope; the composed stage oracle keeps
# its empty scope in its separate text fixture schema.
STRUCTURE_BEARD_SCOPE_PRODUCTION_REAL = 0
DOMAIN = b"lodestone.worldgen.large-parity.manifest/v3/semantic"
DOMAIN_V4 = b"lodestone.worldgen.large-parity.manifest/v4/semantic"
DOMAIN_V5 = b"lodestone.worldgen.large-parity.manifest/v5/semantic"
DOMAIN_V6 = b"lodestone.worldgen.large-parity.manifest/v6/raw-packet"
PACKET_AUDIT_MAGIC = b"LWP26A06"
PACKET_AUDIT_DOMAIN = b"lodestone.worldgen.large-parity.packet-audit/v6/raw-packet"
DOMAIN_V7 = b"lodestone.worldgen.large-parity.manifest/v7/light-free"
LIGHT_FREE_AUDIT_DOMAIN = b"lodestone.worldgen.large-parity.audit/v7/light-free"
# These names are retained for the v3-v5 semantic diagnostics. Do not change
# them when the raw-packet grid changes: old manifests are still readable.
GRID_MIN = -250
GRID_MAX = 250
GRID_SIDE = GRID_MAX - GRID_MIN + 1
GRID_COUNT = GRID_SIDE * GRID_SIDE
RAW_GRID_MIN = -500
RAW_GRID_MAX = 500
RAW_GRID_SIDE = RAW_GRID_MAX - RAW_GRID_MIN + 1
RAW_GRID_COUNT = RAW_GRID_SIDE * RAW_GRID_SIDE
DIMENSIONS = {"overworld": b"minecraft:overworld", "nether": b"minecraft:the_nether", "end": b"minecraft:the_end"}
# magic, version, header, digest algorithm, schema, protocol, seed, global/shard
# bounds, record count, per-record digest width, reserved, schema/world/payload SHA-256;
# v4 and v5 store sha256(dimension resource location) in otherwise-unused bytes 168..200.
FMT = ">8sHHHHIqiiiiiiiiQHH32s32s32s88x"


def dimension(raw, h):
    """Return the authenticated dimension identity (v3 is legacy overworld)."""
    if h[0] == MAGIC:
        return "overworld"
    value = raw[168:200]
    for name, location in DIMENSIONS.items():
        if hashlib.sha256(location).digest() == value:
            return name
    raise ValueError("manifest has an unknown dimension identity")


def _format(version):
    if version == 3:
        return MAGIC, 3, DOMAIN, WIDTH, (GRID_MIN, GRID_MAX)
    if version == 4:
        return MAGIC_V4, 4, DOMAIN_V4, WIDTH, (GRID_MIN, GRID_MAX)
    if version == 5:
        return MAGIC_V5, 5, DOMAIN_V5, WIDTH, (GRID_MIN, GRID_MAX)
    if version == 6:
        return MAGIC_V6, 6, DOMAIN_V6, RAW_WIDTH, (RAW_GRID_MIN, RAW_GRID_MAX)
    if version == 7:
        return MAGIC_V7, 7, DOMAIN_V7, RAW_WIDTH, (RAW_GRID_MIN, RAW_GRID_MAX)
    raise ValueError(f"unsupported manifest version {version}")


def raw_packet_hash(packet_body):
    """Return the committed 16-bit prefix of the exact packet-body digest.

    The complete SHA-256 remains available to callers for an audit sidecar;
    manifest records intentionally stay two bytes per coordinate.
    """
    return hashlib.sha256(packet_body).digest()[:RAW_WIDTH]


def raw_packet_full_digest(packet_body):
    """Return the optional audit digest without changing the v6 record width."""
    return hashlib.sha256(packet_body).digest()


def hash_exact_chunk_with_light_body(packet_body):
    """Name the v6 hash input explicitly for exporter/audit callers."""
    return raw_packet_hash(packet_body)


def make_header(version, dim, sx0, sx1, sz0, sz1, count, frozen, payload_digest):
    magic, schema, domain, width, (grid_min, grid_max) = _format(version)
    if version == 3:
        dim_digest = bytes(32)
    else:
        try:
            dim_digest = hashlib.sha256(DIMENSIONS[dim]).digest()
        except KeyError as error:
            raise ValueError(f"unsupported dimension {dim!r}") from error
    header = struct.pack(FMT, magic, version, HEADER, 2, schema, 776, 42,
                         grid_min, grid_max, grid_min, grid_max, sx0, sx1, sz0, sz1,
                         count, width, STRUCTURE_BEARD_SCOPE_PRODUCTION_REAL, hashlib.sha256(domain).digest(), frozen,
                         payload_digest)
    return header[:168] + dim_digest + header[200:] if version != 3 else header


def _parse_header(path, raw_header, payload_size):
    """Validate a header and return it with its dimension and record width."""
    if len(raw_header) < HEADER:
        raise ValueError(f"{path}: shorter than {HEADER}-byte v3 header")
    h = struct.unpack(FMT, raw_header)
    (magic, version, size, algorithm, schema, protocol, seed, gx0, gx1, gz0,
     gz1, sx0, sx1, sz0, sz1, count, width, reserved, domain, frozen,
     payload_digest) = h
    if magic not in (MAGIC, MAGIC_V4, MAGIC_V5, MAGIC_V6, MAGIC_V7):
        if magic == b"LWP26P02":
            raise ValueError(f"{path}: v2 stores raw 16-bit packet fingerprints and is rejected; regenerate from a frozen world as v3")
        raise ValueError(f"{path}: unsupported manifest magic {magic!r}")
    valid_v3 = (magic == MAGIC and version == 3 and schema == 3)
    valid_v4 = (magic == MAGIC_V4 and version == 4 and schema == 4)
    valid_v5 = (magic == MAGIC_V5 and version == 5 and schema == 5)
    valid_v6 = (magic == MAGIC_V6 and version == 6 and schema == 6)
    valid_v7 = (magic == MAGIC_V7 and version == 7 and schema == 7)
    expected_width = RAW_WIDTH if (valid_v6 or valid_v7) else WIDTH
    if not (valid_v3 or valid_v4 or valid_v5 or valid_v6 or valid_v7) or (size, algorithm, protocol, seed, width, reserved) != (HEADER, 2, 776, 42, expected_width, STRUCTURE_BEARD_SCOPE_PRODUCTION_REAL):
        raise ValueError(f"{path}: unsupported parity manifest header")
    grid_min, grid_max = (RAW_GRID_MIN, RAW_GRID_MAX) if (valid_v6 or valid_v7) else (GRID_MIN, GRID_MAX)
    if (gx0, gx1, gz0, gz1) != (grid_min, grid_max, grid_min, grid_max):
        side = grid_max - grid_min + 1
        raise ValueError(f"{path}: not the required {side}x{side} grid")
    expected = (sx1-sx0+1)*(sz1-sz0+1)
    if sx0 < gx0 or sx1 > gx1 or sz0 < gz0 or sz1 > gz1 or count != expected:
        raise ValueError(f"{path}: invalid shard bounds/count")
    expected_domain = (DOMAIN if version == 3 else DOMAIN_V4 if version == 4 else
                       DOMAIN_V5 if version == 5 else DOMAIN_V6 if version == 6 else DOMAIN_V7)
    if domain != hashlib.sha256(expected_domain).digest():
        kind = "raw-packet" if version == 6 else "light-free" if version == 7 else "semantic-record"
        raise ValueError(f"{path}: {kind} schema digest differs")
    dim = dimension(raw_header, h)
    if frozen == bytes(32):
        raise ValueError(f"{path}: missing frozen-world identity")
    expected_payload_size = count * expected_width
    if payload_size != expected_payload_size:
        raise ValueError(f"{path}: payload size is {payload_size}, expected {expected_payload_size}")
    return h, dim, expected_width


def read(path):
    raw = pathlib.Path(path).read_bytes()
    h, dim, expected_width = _parse_header(path, raw[:HEADER], len(raw) - HEADER)
    payload = raw[HEADER:]
    if len(payload) != h[15] * expected_width:
        raise ValueError(f"{path}: payload size is {len(payload)}, expected {h[15] * expected_width}")
    if h[20] != hashlib.sha256(payload).digest():
        raise ValueError(f"{path}: payload checksum differs")
    return h, payload, dim


def packet_audit_path(manifest_path):
    """Return the required full-digest sidecar path for a v6 manifest."""
    return pathlib.Path(str(manifest_path) + ".packet-audit")


def _parse_packet_audit_header(path, raw_header, payload_size, manifest_header, manifest_dimension):
    if len(raw_header) != HEADER:
        raise ValueError(f"{path}: shorter than {HEADER}-byte packet-audit header")
    h = struct.unpack(FMT, raw_header)
    (magic, version, size, algorithm, schema, protocol, seed, gx0, gx1, gz0,
     gz1, sx0, sx1, sz0, sz1, count, width, reserved, domain, frozen,
     payload_digest) = h
    if (magic, version, size, algorithm, schema, protocol, seed, width, reserved) != (
        PACKET_AUDIT_MAGIC, 6, HEADER, 3, 6, 776, 42, WIDTH, STRUCTURE_BEARD_SCOPE_PRODUCTION_REAL
    ):
        raise ValueError(f"{path}: packet-audit header identity differs")
    if (gx0, gx1, gz0, gz1) != (RAW_GRID_MIN, RAW_GRID_MAX, RAW_GRID_MIN, RAW_GRID_MAX):
        raise ValueError(f"{path}: packet-audit global bounds differ")
    if (sx0, sx1, sz0, sz1, count) != tuple(manifest_header[11:16]):
        raise ValueError(f"{path}: packet-audit shard geometry differs from its manifest")
    if domain != hashlib.sha256(PACKET_AUDIT_DOMAIN).digest():
        raise ValueError(f"{path}: packet-audit schema digest differs")
    if frozen != manifest_header[19]:
        raise ValueError(f"{path}: packet-audit frozen-world identity differs")
    expected_dimension = hashlib.sha256(DIMENSIONS[manifest_dimension]).digest()
    if raw_header[168:200] != expected_dimension:
        raise ValueError(f"{path}: packet-audit dimension identity differs")
    expected_size = count * WIDTH
    if payload_size != expected_size:
        raise ValueError(f"{path}: payload size is {payload_size}, expected {expected_size}")
    return h


class _PacketAuditStream:
    """Stream one authenticated v6 full-digest sidecar."""

    def __init__(self, path, manifest_header, manifest_dimension):
        self.path = packet_audit_path(path)
        self.manifest_header = manifest_header
        self.manifest_dimension = manifest_dimension
        self.source = None
        self.header = None

    def __enter__(self):
        if not self.path.is_file():
            raise ValueError(f"{self.path}: required packet-audit sidecar is missing")
        self.source = self.path.open("rb")
        try:
            raw_header = self.source.read(HEADER)
            self.source.seek(0, 2)
            payload_size = self.source.tell() - HEADER
            self.source.seek(HEADER)
            self.header = _parse_packet_audit_header(
                self.path, raw_header, payload_size, self.manifest_header, self.manifest_dimension
            )
        except BaseException:
            self.source.close()
            raise
        self.payload_digest = hashlib.sha256()
        return self

    def records(self):
        count = self.header[15]
        for _ in range(count):
            record = self.source.read(WIDTH)
            if len(record) != WIDTH:
                raise ValueError(f"{self.path}: payload ended before {count} full digests")
            self.payload_digest.update(record)
            yield record
        if self.source.read(1):
            raise ValueError(f"{self.path}: payload has trailing bytes")
        if self.header[20] != self.payload_digest.digest():
            raise ValueError(f"{self.path}: payload checksum differs")

    def __exit__(self, _type, _value, _traceback):
        self.source.close()


def read_packet_audit(manifest_path, manifest_header, manifest_payload, manifest_dimension):
    """Authenticate a v6 full-digest sidecar and return its payload."""
    if manifest_header[1] != 6:
        return None
    with _PacketAuditStream(manifest_path, manifest_header, manifest_dimension) as audit:
        payload = b"".join(audit.records())
    for index, prefix in enumerate(
        manifest_payload[offset:offset + RAW_WIDTH]
        for offset in range(0, len(manifest_payload), RAW_WIDTH)
    ):
        if payload[index * WIDTH:index * WIDTH + RAW_WIDTH] != prefix:
            width = manifest_header[12] - manifest_header[11] + 1
            cx = manifest_header[11] + index % width
            cz = manifest_header[13] + index // width
            raise ValueError(f"{packet_audit_path(manifest_path)}: digest prefix differs at ({cx},{cz})")
    return payload


def make_packet_audit_header(manifest_header, manifest_dimension, payload_digest):
    """Build the authenticated v6 full-digest sidecar header."""
    sx0, sx1, sz0, sz1, count = manifest_header[11:16]
    header = struct.pack(
        FMT, PACKET_AUDIT_MAGIC, 6, HEADER, 3, 6, 776, 42,
        RAW_GRID_MIN, RAW_GRID_MAX, RAW_GRID_MIN, RAW_GRID_MAX,
        sx0, sx1, sz0, sz1, count, WIDTH, STRUCTURE_BEARD_SCOPE_PRODUCTION_REAL,
        hashlib.sha256(PACKET_AUDIT_DOMAIN).digest(), manifest_header[19],
        payload_digest,
    )
    dimension_digest = hashlib.sha256(DIMENSIONS[manifest_dimension]).digest()
    return header[:168] + dimension_digest + header[200:]


class _ManifestStream:
    """Stream one authenticated shard without retaining its payload."""

    def __init__(self, path):
        self.path = pathlib.Path(path)
        self.source = None
        self.header = None
        self.dimension = None
        self.record_width = None
        self.payload_digest = None

    def __enter__(self):
        self.source = self.path.open("rb")
        try:
            raw_header = self.source.read(HEADER)
            self.source.seek(0, 2)
            payload_size = self.source.tell() - HEADER
            self.source.seek(HEADER)
            self.header, self.dimension, self.record_width = _parse_header(
                self.path, raw_header, payload_size
            )
        except BaseException:
            self.source.close()
            raise
        self.payload_digest = hashlib.sha256()
        return self

    def records(self):
        for _ in range(self.header[15]):
            record = self.source.read(self.record_width)
            if len(record) != self.record_width:
                expected = self.header[15] * self.record_width
                actual = self.source.tell() - HEADER
                raise ValueError(f"{self.path}: payload size is {actual}, expected {expected}")
            self.payload_digest.update(record)
            yield record
        if self.source.read(1):
            expected = self.header[15] * self.record_width
            actual = self.source.tell() - HEADER
            raise ValueError(f"{self.path}: payload size is {actual}, expected {expected}")
        if self.header[20] != self.payload_digest.digest():
            raise ValueError(f"{self.path}: payload checksum differs")

    def __exit__(self, _type, _value, _traceback):
        self.source.close()


class _LightFreeAuditStream:
    """Stream one authenticated P07 full-digest sidecar."""

    def __init__(self, path, manifest_header, manifest_dimension):
        self.path = pathlib.Path(str(path) + ".light-free-audit")
        self.manifest_header = manifest_header
        self.manifest_dimension = manifest_dimension
        self.source = None
        self.header = None

    def __enter__(self):
        if not self.path.is_file():
            raise ValueError(f"{self.path}: required light-free audit sidecar is missing")
        self.source = self.path.open("rb")
        try:
            raw_header = self.source.read(HEADER)
            self.source.seek(0, 2)
            payload_size = self.source.tell() - HEADER
            self.source.seek(HEADER)
            self.header = _parse_light_free_audit_header(
                self.path, raw_header, payload_size, self.manifest_header, self.manifest_dimension
            )
        except BaseException:
            self.source.close()
            raise
        self.payload_digest = hashlib.sha256()
        return self

    def records(self):
        count = self.header[15]
        for _ in range(count):
            record = self.source.read(WIDTH)
            if len(record) != WIDTH:
                raise ValueError(f"{self.path}: payload ended before {count} full digests")
            self.payload_digest.update(record)
            yield record
        if self.source.read(1):
            raise ValueError(f"{self.path}: payload has trailing bytes")
        if self.header[20] != self.payload_digest.digest():
            raise ValueError(f"{self.path}: payload checksum differs")

    def __exit__(self, _type, _value, _traceback):
        self.source.close()


def light_free_audit_path(manifest_path):
    """Return the only accepted P07 sidecar spelling."""
    return pathlib.Path(str(manifest_path) + ".light-free-audit")


def _parse_light_free_audit_header(path, raw_header, payload_size, manifest_header, manifest_dimension):
    if len(raw_header) != HEADER:
        raise ValueError(f"{path}: shorter than {HEADER}-byte light-free audit header")
    h = struct.unpack(FMT, raw_header)
    (magic, version, size, algorithm, schema, protocol, seed, gx0, gx1, gz0,
     gz1, sx0, sx1, sz0, sz1, count, width, reserved, domain, frozen,
     payload_digest) = h
    if (magic, version, size, algorithm, schema, protocol, seed, width, reserved) != (
        LIGHT_FREE_AUDIT_MAGIC, 7, HEADER, 3, 7, 776, 42, WIDTH, STRUCTURE_BEARD_SCOPE_PRODUCTION_REAL
    ):
        raise ValueError(f"{path}: light-free audit header identity differs")
    if (gx0, gx1, gz0, gz1) != (RAW_GRID_MIN, RAW_GRID_MAX, RAW_GRID_MIN, RAW_GRID_MAX):
        raise ValueError(f"{path}: light-free audit global bounds differ")
    if (sx0, sx1, sz0, sz1, count) != tuple(manifest_header[11:16]):
        raise ValueError(f"{path}: light-free audit shard geometry differs from its manifest")
    if domain != hashlib.sha256(LIGHT_FREE_AUDIT_DOMAIN).digest():
        raise ValueError(f"{path}: light-free audit schema digest differs")
    if frozen != manifest_header[19]:
        raise ValueError(f"{path}: light-free audit frozen-world identity differs")
    expected_dimension = hashlib.sha256(DIMENSIONS[manifest_dimension]).digest()
    if raw_header[168:200] != expected_dimension:
        raise ValueError(f"{path}: light-free audit dimension identity differs")
    expected_size = count * WIDTH
    if payload_size != expected_size:
        raise ValueError(f"{path}: payload size is {payload_size}, expected {expected_size}")
    return h


def read_light_free_audit(manifest_path, manifest_header, manifest_dimension):
    """Authenticate a P07 sidecar and return its complete payload digest.

    The payload itself is streamed, so validating a 1001 by 1001 sidecar does
    not retain 32 MiB of records in Python memory.
    """
    with pathlib.Path(manifest_path).open("rb") as main, _LightFreeAuditStream(
        manifest_path, manifest_header, manifest_dimension
    ) as audit:
        main.seek(HEADER)
        for index, full in enumerate(audit.records()):
            prefix = main.read(RAW_WIDTH)
            if prefix != full[:RAW_WIDTH]:
                raise ValueError(
                    f"{audit.path}: full digest prefix differs from manifest at record {index}"
                )
        if main.read(1):
            raise ValueError(f"{manifest_path}: payload has trailing bytes")
        return audit.header[20]


def make_light_free_audit_header(manifest_header, manifest_dimension, payload_digest):
    """Build the authenticated P07 full-digest sidecar header."""
    sx0, sx1, sz0, sz1, count = manifest_header[11:16]
    header = struct.pack(
        FMT, LIGHT_FREE_AUDIT_MAGIC, 7, HEADER, 3, 7, 776, 42,
        RAW_GRID_MIN, RAW_GRID_MAX, RAW_GRID_MIN, RAW_GRID_MAX,
        sx0, sx1, sz0, sz1, count, WIDTH, STRUCTURE_BEARD_SCOPE_PRODUCTION_REAL,
        hashlib.sha256(LIGHT_FREE_AUDIT_DOMAIN).digest(), manifest_header[19],
        payload_digest,
    )
    dimension_digest = hashlib.sha256(DIMENSIONS[manifest_dimension]).digest()
    return header[:168] + dimension_digest + header[200:]


def _slot_seen(slots, index):
    """Return whether a dense occupancy bit is set, and set it otherwise."""
    byte = index >> 3
    mask = 1 << (index & 7)
    if slots[byte] & mask:
        return True
    slots[byte] |= mask
    return False


def validate(paths):
    for path in paths:
        h, payload, dim = read(path)
        kind = "raw-packet" if h[1] == 6 else "light-free" if h[1] == 7 else "semantic"
        audit = read_packet_audit(path, h, payload, dim) if h[1] == 6 else read_light_free_audit(path, h, dim) if h[1] == 7 else None
        suffix = f" audit_sha256={audit.hex()}" if audit is not None else ""
        print(f"ok {path}: kind={kind} width={h[16]} dimension={dim} cx={h[11]}..{h[12]} cz={h[13]}..{h[14]} payload_sha256={h[20].hex()} frozen={h[19].hex()}{suffix}")


def merge(out, paths):
    """Merge shards through bounded external storage in canonical grid order."""
    frozen, dim, version = None, None, None
    record_width = None
    required = None
    grid_min = grid_max = None
    seen_count = 0
    overlap = None
    with tempfile.TemporaryDirectory(prefix="large-parity-merge-") as directory:
        slot_path = pathlib.Path(directory) / "records"
        slot_file = None
        slot_map = None
        audit_slot_path = pathlib.Path(directory) / "light-free-audit-records"
        audit_slot_file = None
        audit_slot_map = None
        packet_audit_slot_path = pathlib.Path(directory) / "packet-audit-records"
        packet_audit_slot_file = None
        packet_audit_slot_map = None
        seen = None
        try:
            for path in paths:
                with _ManifestStream(path) as shard:
                    h, shard_dim = shard.header, shard.dimension
                    compatibility_error = None
                    if version is None:
                        version, dim, record_width = h[1], shard_dim, shard.record_width
                        required = RAW_GRID_COUNT if version in (6, 7) else GRID_COUNT
                        grid_min, grid_max = (RAW_GRID_MIN, RAW_GRID_MAX) if version in (6, 7) else (GRID_MIN, GRID_MAX)
                        slot_file = slot_path.open("w+b")
                        slot_file.truncate(required * record_width)
                        slot_map = mmap.mmap(slot_file.fileno(), 0, access=mmap.ACCESS_WRITE)
                        if version == 7:
                            audit_slot_file = audit_slot_path.open("w+b")
                            audit_slot_file.truncate(required * WIDTH)
                            audit_slot_map = mmap.mmap(audit_slot_file.fileno(), 0, access=mmap.ACCESS_WRITE)
                        if version == 6:
                            packet_audit_slot_file = packet_audit_slot_path.open("w+b")
                            packet_audit_slot_file.truncate(required * WIDTH)
                            packet_audit_slot_map = mmap.mmap(packet_audit_slot_file.fileno(), 0, access=mmap.ACCESS_WRITE)
                        seen = bytearray((required + 7) // 8)
                    elif (h[1], shard_dim) != (version, dim):
                        compatibility_error = ValueError(
                            f"{path}: manifest format or dimension differs; do not merge dimensions or schema versions"
                        )
                    if frozen is None:
                        frozen = h[19]
                    elif frozen != h[19]:
                        compatibility_error = compatibility_error or ValueError(
                            f"{path}: frozen-world identity differs; never merge independently generated worlds"
                        )
                    sx0, sx1, sz0, sz1 = h[11:15]
                    records = shard.records()
                    audit_context = (
                        _PacketAuditStream(path, h, shard_dim) if h[1] == 6
                        else _LightFreeAuditStream(path, h, shard_dim) if h[1] == 7
                        else None
                    )
                    audit_records = None
                    if audit_context is not None:
                        audit_context.__enter__()
                        audit_records = audit_context.records()
                    try:
                        for cz in range(sz0, sz1 + 1):
                            for cx in range(sx0, sx1 + 1):
                                index = (cz - grid_min) * (grid_max - grid_min + 1) + (cx - grid_min)
                                value = next(records)
                                audit_value = next(audit_records) if audit_records is not None else None
                                if compatibility_error is not None:
                                    continue
                                if _slot_seen(seen, index):
                                    if overlap is None:
                                        overlap = f"overlap at ({cx}, {cz}): {path}"
                                else:
                                    offset = index * record_width
                                    slot_map[offset:offset + record_width] = value
                                    if audit_value is not None:
                                        audit_offset = index * WIDTH
                                        target_map = packet_audit_slot_map if h[1] == 6 else audit_slot_map
                                        target_map[audit_offset:audit_offset + WIDTH] = audit_value
                                    seen_count += 1
                        # Exhaust both iterators here so the whole shard is
                        # authenticated before overlap reporting.
                        try:
                            next(records)
                        except StopIteration:
                            pass
                        if audit_records is not None:
                            try:
                                next(audit_records)
                            except StopIteration:
                                pass
                    finally:
                        if audit_context is not None:
                            audit_context.__exit__(None, None, None)
                    if compatibility_error is not None:
                        raise compatibility_error
        finally:
            if packet_audit_slot_map is not None:
                packet_audit_slot_map.flush()
                packet_audit_slot_map.close()
            if packet_audit_slot_file is not None:
                packet_audit_slot_file.close()
            if audit_slot_map is not None:
                audit_slot_map.flush()
                audit_slot_map.close()
            if audit_slot_file is not None:
                audit_slot_file.close()
            if slot_map is not None:
                slot_map.flush()
                slot_map.close()
            if slot_file is not None:
                slot_file.close()
        if version is None:
            raise ValueError(f"incomplete merge: 0/{GRID_COUNT}; missing shards are not silently zero-filled")
        if overlap is not None:
            raise ValueError(overlap)
        if seen_count != required:
            raise ValueError(f"incomplete merge: {seen_count}/{required}; missing shards are not silently zero-filled")
        with slot_path.open("rb") as source:
            payload_digest = hashlib.sha256()
            while True:
                block = source.read(1024 * 1024)
                if not block:
                    break
                payload_digest.update(block)
        header = make_header(version, dim, grid_min, grid_max, grid_min, grid_max,
                             required, frozen, payload_digest.digest())
        with pathlib.Path(out).open("wb") as target, slot_path.open("rb") as source:
            target.write(header)
            while True:
                block = source.read(1024 * 1024)
                if not block:
                    break
                target.write(block)
        if version in (6, 7):
            sidecar_path = packet_audit_path(out) if version == 6 else light_free_audit_path(out)
            sidecar_source = packet_audit_slot_path if version == 6 else audit_slot_path
            audit_payload_digest = hashlib.sha256()
            with sidecar_source.open("rb") as source:
                while True:
                    block = source.read(1024 * 1024)
                    if not block:
                        break
                    audit_payload_digest.update(block)
            audit_header = (make_packet_audit_header if version == 6 else make_light_free_audit_header)(
                struct.unpack(FMT, header), dim, audit_payload_digest.digest()
            )
            with sidecar_path.open("wb") as target, sidecar_source.open("rb") as source:
                target.write(audit_header)
                while True:
                    block = source.read(1024 * 1024)
                    if not block:
                        break
                    target.write(block)
    kind = "raw packet hashes" if version == 6 else "light-free hashes" if version == 7 else "semantic SHA-256 digests"
    print(f"merged {required} {kind} into {out}")


def accept(out, first, second):
    """Freeze a baseline only after two independent read-only exports agree."""
    first_header, first_payload, first_dim = read(first)
    second_header, second_payload, second_dim = read(second)
    grid_min, grid_max = (RAW_GRID_MIN, RAW_GRID_MAX) if first_header[1] in (6, 7) else (GRID_MIN, GRID_MAX)
    required = RAW_GRID_COUNT if first_header[1] in (6, 7) else GRID_COUNT
    if first_header[11:16] != (grid_min, grid_max, grid_min, grid_max, required):
        raise ValueError(f"{first}: duplicate-read acceptance requires the complete {grid_max-grid_min+1}x{grid_max-grid_min+1} manifest")
    if second_header[11:16] != first_header[11:16]:
        raise ValueError("duplicate frozen-world reads cover different bounds")
    if second_header[1] != first_header[1] or second_header[18:20] != first_header[18:20] or second_dim != first_dim:
        raise ValueError("duplicate frozen-world reads have different semantic schemas, dimensions, or frozen-world identities")
    if second_payload != first_payload:
        raise ValueError("duplicate frozen-world reads differ; baseline is not accepted")
    pathlib.Path(out).write_bytes(pathlib.Path(first).read_bytes())
    if first_header[1] == 6:
        first_audit = read_packet_audit(first, first_header, first_payload, first_dim)
        second_audit = read_packet_audit(second, second_header, second_payload, second_dim)
        if second_audit != first_audit:
            raise ValueError("duplicate frozen-world packet-audit reads differ; baseline is not accepted")
        pathlib.Path(packet_audit_path(out)).write_bytes(
            make_packet_audit_header(first_header, first_dim, hashlib.sha256(first_audit).digest()) + first_audit
        )
    elif first_header[1] == 7:
        first_audit = read_light_free_audit(first, first_header, first_dim)
        second_audit = read_light_free_audit(second, second_header, second_dim)
        if first_audit != second_audit:
            raise ValueError("duplicate frozen-world light-free audit reads differ; baseline is not accepted")
        pathlib.Path(light_free_audit_path(out)).write_bytes(light_free_audit_path(first).read_bytes())
    print(f"accepted duplicate-read baseline into {out}")


def reproducible(first, second):
    """Require independent materializations to agree without sharing a root id."""
    first_header, first_payload, first_dim = read(first)
    second_header, second_payload, second_dim = read(second)
    raw_records = first_header[1] in (6, 7)
    record_kind = "raw packet hash payload" if first_header[1] == 6 else "light-free hash payload" if first_header[1] == 7 else "semantic payload"
    grid_min, grid_max = (RAW_GRID_MIN, RAW_GRID_MAX) if raw_records else (GRID_MIN, GRID_MAX)
    required = RAW_GRID_COUNT if raw_records else GRID_COUNT
    complete = (grid_min, grid_max, grid_min, grid_max, required)
    if first_header[11:16] != complete:
        raise ValueError(f"{first}: independent-root reproducibility requires the complete {grid_max-grid_min+1}x{grid_max-grid_min+1} manifest")
    if second_header[11:16] != complete:
        raise ValueError(f"{second}: independent-root reproducibility requires the complete {grid_max-grid_min+1}x{grid_max-grid_min+1} manifest")
    if second_header[1] != first_header[1] or second_header[18] != first_header[18]:
        raise ValueError("independent materializations have different record schemas")
    if second_dim != first_dim:
        raise ValueError("independent materializations have different dimensions")
    if second_header[11:16] != first_header[11:16]:
        raise ValueError("independent materializations cover different geometry")
    if second_payload != first_payload:
        raise ValueError(f"independent materializations differ in {record_kind}")
    if first_header[1] == 6:
        first_audit = read_packet_audit(first, first_header, first_payload, first_dim)
        second_audit = read_packet_audit(second, second_header, second_payload, second_dim)
        if second_audit != first_audit:
            raise ValueError("independent materializations differ in raw packet audit payload")
    elif first_header[1] == 7:
        first_audit = read_light_free_audit(first, first_header, first_dim)
        second_audit = read_light_free_audit(second, second_header, second_dim)
        if first_audit != second_audit:
            raise ValueError("independent materializations differ in light-free full-digest sidecar")
    print(f"independent materializations reproduce {record_kind}: dimension={first_dim} cx={first_header[11]}..{first_header[12]} cz={first_header[13]}..{first_header[14]}")


def selftest():
    """Exercise a full merge, payload tampering, incompatible worlds, and v2 refusal."""
    def make(path, sx0, sx1, byte, frozen):
        count = (sx1-sx0+1)*GRID_SIDE
        payload = bytes([byte])*WIDTH*count
        header = make_header(3, "overworld", sx0, sx1, GRID_MIN, GRID_MAX,
                             count, frozen, hashlib.sha256(payload).digest())
        pathlib.Path(path).write_bytes(header + payload)
    with tempfile.TemporaryDirectory(prefix="large-parity-v3-") as directory:
        directory = pathlib.Path(directory)
        left, right, out = directory/"left.lwp", directory/"right.lwp", directory/"full.lwp"
        frozen = hashlib.sha256(b"frozen world control").digest()
        legacy_count = (0 - GRID_MIN + 1) * GRID_SIDE
        legacy_payload = bytes([0x11]) * WIDTH * legacy_count
        legacy_header = struct.pack(FMT, MAGIC, 3, HEADER, 2, 3, 776, 42,
                                    GRID_MIN, GRID_MAX, GRID_MIN, GRID_MAX, GRID_MIN, 0,
                                    GRID_MIN, GRID_MAX, legacy_count, WIDTH, STRUCTURE_BEARD_SCOPE_PRODUCTION_REAL,
                                    hashlib.sha256(DOMAIN).digest(), frozen,
                                    hashlib.sha256(legacy_payload).digest())
        assert make_header(3, "overworld", GRID_MIN, 0, GRID_MIN, GRID_MAX,
                           legacy_count, frozen,
                           hashlib.sha256(legacy_payload).digest()) == legacy_header
        mismatched = directory / "mismatched-format.lwp"
        mismatched.write_bytes(MAGIC_V4 + legacy_header[8:] + legacy_payload)
        try: read(mismatched)
        except ValueError: pass
        else: raise AssertionError("v3 fields under the v4 magic were accepted")
        make(left, GRID_MIN, 0, 0x11, frozen); make(right, 1, GRID_MAX, 0x22, frozen)
        merge(out, [left, right]); h, payload, dim = read(out)
        assert dim == "overworld"
        left_width = 0 - GRID_MIN + 1
        assert h[15] == GRID_COUNT and payload[:WIDTH] == bytes([0x11])*WIDTH and payload[left_width*WIDTH:(left_width+1)*WIDTH] == bytes([0x22])*WIDTH
        duplicate = directory/"duplicate.lwp"; accepted = directory/"accepted.lwp"; duplicate.write_bytes(out.read_bytes()); accept(accepted, out, duplicate); assert accepted.read_bytes() == out.read_bytes()
        raw = bytearray(duplicate.read_bytes()); raw[HEADER+3] ^= 1; duplicate.write_bytes(raw)
        try: accept(accepted, out, duplicate)
        except ValueError: pass
        else: raise AssertionError("mismatched duplicate frozen-world read was accepted")
        independent = directory/"independent.lwp"
        _, full_payload, _ = read(out)
        independent.write_bytes(make_header(3, "overworld", GRID_MIN, GRID_MAX, GRID_MIN, GRID_MAX,
                                            GRID_COUNT, hashlib.sha256(b"independent root").digest(),
                                            hashlib.sha256(full_payload).digest()) + full_payload)
        reproducible(out, independent)
        raw = bytearray(independent.read_bytes()); raw[HEADER+3] ^= 1; independent.write_bytes(raw)
        try: reproducible(out, independent)
        except ValueError as error: assert "checksum" in str(error)
        else: raise AssertionError("independent payload bit flip was accepted")
        one_record = bytes([0x5A]) * WIDTH
        incomplete = directory/"incomplete.lwp"
        incomplete.write_bytes(make_header(3, "overworld", GRID_MIN, GRID_MIN, GRID_MIN, GRID_MIN,
                                           1, frozen, hashlib.sha256(one_record).digest()) + one_record)
        try: reproducible(incomplete, incomplete)
        except ValueError as error: assert "complete" in str(error)
        else: raise AssertionError("matching incomplete independent-root manifests were accepted")
        schema_v4 = directory/"schema-v4.lwp"
        schema_v4.write_bytes(make_header(4, "overworld", GRID_MIN, GRID_MAX, GRID_MIN, GRID_MAX,
                                          GRID_COUNT, hashlib.sha256(b"schema root").digest(),
                                          hashlib.sha256(full_payload).digest()) + full_payload)
        try: reproducible(out, schema_v4)
        except ValueError as error: assert "schema" in str(error)
        else: raise AssertionError("independent schema mismatch was accepted")
        dimension_v4 = directory/"dimension-v4.lwp"
        dimension_v4.write_bytes(make_header(4, "nether", GRID_MIN, GRID_MAX, GRID_MIN, GRID_MAX,
                                             GRID_COUNT, hashlib.sha256(b"dimension root").digest(),
                                             hashlib.sha256(full_payload).digest()) + full_payload)
        try: reproducible(schema_v4, dimension_v4)
        except ValueError as error: assert "dimension" in str(error)
        else: raise AssertionError("independent dimension mismatch was accepted")
        raw = bytearray(left.read_bytes()); raw[HEADER+3] ^= 1; left.write_bytes(raw)
        try: read(left)
        except ValueError: pass
        else: raise AssertionError("one-bit payload corruption was accepted")
        make(left, GRID_MIN, 0, 0x11, frozen); make(right, 1, GRID_MAX, 0x22, hashlib.sha256(b"other frozen world").digest())
        try: merge(out, [left, right])
        except ValueError: pass
        else: raise AssertionError("different frozen worlds were merged")
        v2 = directory/"old.lwp"; v2.write_bytes(b"LWP26P02" + bytes(HEADER))
        try: read(v2)
        except ValueError as error: assert "v2" in str(error)
        else: raise AssertionError("v2 was accepted")
        # A dimension identity is authenticated independently of the frozen
        # world and payload. Equal payloads must still never cross-merge or
        # cross-accept between dimensions.
        def make_v4(path, dim, byte):
            payload = bytes([byte]) * WIDTH * GRID_COUNT
            path.write_bytes(make_header(4, dim, GRID_MIN, GRID_MAX, GRID_MIN, GRID_MAX,
                                         GRID_COUNT, frozen, hashlib.sha256(payload).digest()) + payload)
        overworld = directory / "overworld-v4.lwp"; nether = directory / "nether-v4.lwp"
        make_v4(overworld, "overworld", 0x31); make_v4(nether, "nether", 0x31)
        assert read(overworld)[2] == "overworld" and read(nether)[2] == "nether"
        try: merge(out, [overworld, nether])
        except ValueError as error: assert "dimension" in str(error)
        else: raise AssertionError("different dimensions were merged")
        try: accept(accepted, overworld, nether)
        except ValueError as error: assert "dimension" in str(error)
        else: raise AssertionError("different dimensions were accepted")
        v5 = directory / "end-v5.lwp"; v5_payload = bytes([0x53]) * WIDTH * GRID_COUNT
        v5.write_bytes(make_header(5, "end", GRID_MIN, GRID_MAX, GRID_MIN, GRID_MAX,
                                   GRID_COUNT, frozen, hashlib.sha256(v5_payload).digest()) + v5_payload)
        assert read(v5)[1] == v5_payload and read(v5)[2] == "end"
        raw = bytearray(v5.read_bytes()); raw[72] ^= 1; v5.write_bytes(raw)
        try: read(v5)
        except ValueError as error: assert "schema" in str(error)
        else: raise AssertionError("v5 schema/domain mismatch was accepted")
        # v6 is deliberately a different geometry and record kind. Build two
        # half-grid raw-packet-hash shards and prove merge/accept/reproducible
        # preserve the 16-bit records without silently widening them.
        v6_left = directory / "raw-left.lwp"; v6_right = directory / "raw-right.lwp"; v6_full = directory / "raw-full.lwp"
        raw_frozen = hashlib.sha256(b"raw frozen world control").digest()
        def make_v6(path, sx0, sx1, value):
            count = (sx1 - sx0 + 1) * RAW_GRID_SIDE
            payload = bytes([value, value ^ 0x5A]) * count
            path.write_bytes(make_header(6, "nether", sx0, sx1, RAW_GRID_MIN, RAW_GRID_MAX,
                                         count, raw_frozen, hashlib.sha256(payload).digest()) + payload)
        def make_v6_audit(manifest):
            """Build the required P06 full-digest sidecar for a synthetic shard."""
            header, payload, dim = read(manifest)
            audit = bytearray()
            for offset in range(0, len(payload), RAW_WIDTH):
                suffix = hashlib.sha256(f"audit-{offset}".encode()).digest()[RAW_WIDTH:]
                audit.extend(payload[offset:offset + RAW_WIDTH] + suffix)
            audit = bytes(audit)
            packet_audit_path(manifest).write_bytes(
                make_packet_audit_header(header, dim, hashlib.sha256(audit).digest()) + audit
            )
        make_v6(v6_left, RAW_GRID_MIN, 0, 0x61); make_v6(v6_right, 1, RAW_GRID_MAX, 0xA2)
        make_v6_audit(v6_left); make_v6_audit(v6_right)
        merge(v6_full, [v6_left, v6_right]); v6_header, v6_payload, v6_dim = read(v6_full)
        assert v6_dim == "nether" and v6_header[1] == 6 and v6_header[16] == RAW_WIDTH
        assert len(v6_payload) == RAW_GRID_COUNT * RAW_WIDTH
        assert raw_packet_hash(b"packet body") == hashlib.sha256(b"packet body").digest()[:2]
        v6_copy = directory / "raw-copy.lwp"; v6_copy.write_bytes(v6_full.read_bytes())
        packet_audit_path(v6_copy).write_bytes(packet_audit_path(v6_full).read_bytes())
        accepted_v6 = directory / "raw-accepted.lwp"; accept(accepted_v6, v6_full, v6_copy)
        assert packet_audit_path(accepted_v6).read_bytes() == packet_audit_path(v6_full).read_bytes()
        reproducible(v6_full, v6_copy)
        changed = bytearray(v6_copy.read_bytes()); changed[HEADER + 1] ^= 1; v6_copy.write_bytes(changed)
        try: read(v6_copy)
        except ValueError as error: assert "checksum" in str(error)
        else: raise AssertionError("v6 payload corruption was accepted")
        missing_audit = directory / "raw-missing-audit.lwp"
        missing_audit.write_bytes(v6_full.read_bytes())
        try: validate([missing_audit])
        except ValueError as error: assert "sidecar" in str(error)
        else: raise AssertionError("v6 manifest without its sidecar was accepted")
        bad_dim = directory / "raw-bad-dimension.lwp"
        bad = bytearray(v6_full.read_bytes()); bad[168] ^= 1; bad_dim.write_bytes(bad)
        try: read(bad_dim)
        except ValueError as error: assert "dimension" in str(error)
        else: raise AssertionError("v6 dimension identity corruption was accepted")

        # P07 keeps the same 1001-square geometry and compact main records,
        # but authenticates canonical light-free terrain records in a required
        # full-digest sidecar.  Exercise merge, duplicate-read acceptance,
        # independent-root comparison, and both payload streams' tamper gates.
        v7_left = directory / "light-free-left.lwp"; v7_right = directory / "light-free-right.lwp"; v7_full = directory / "light-free-full.lwp"
        v7_frozen = hashlib.sha256(b"light-free frozen world control").digest()
        def make_v7(path, sx0, sx1, value, sidecar_value=None):
            count = (sx1 - sx0 + 1) * RAW_GRID_SIDE
            prefixes = bytes([value, value ^ 0xA5]) * count
            full = bytearray([sidecar_value if sidecar_value is not None else value] * WIDTH * count)
            # Make every full record's prefix agree with the compact payload.
            for record in range(count):
                full[record * WIDTH:record * WIDTH + RAW_WIDTH] = prefixes[record * RAW_WIDTH:(record + 1) * RAW_WIDTH]
            full = bytes(full)
            main_header = make_header(7, "end", sx0, sx1, RAW_GRID_MIN, RAW_GRID_MAX,
                                      count, v7_frozen, hashlib.sha256(prefixes).digest())
            path.write_bytes(main_header + prefixes)
            audit_header = make_light_free_audit_header(
                struct.unpack(FMT, main_header), "end", hashlib.sha256(full).digest()
            )
            light_free_audit_path(path).write_bytes(audit_header + full)
        make_v7(v7_left, RAW_GRID_MIN, 0, 0x71); make_v7(v7_right, 1, RAW_GRID_MAX, 0xB2)
        merge(v7_full, [v7_left, v7_right])
        v7_header, v7_payload, v7_dim = read(v7_full)
        assert v7_header[1] == 7 and v7_dim == "end" and len(v7_payload) == RAW_GRID_COUNT * RAW_WIDTH
        assert read_light_free_audit(v7_full, v7_header, v7_dim)
        v7_copy = directory / "light-free-copy.lwp"
        v7_copy.write_bytes(v7_full.read_bytes())
        light_free_audit_path(v7_copy).write_bytes(light_free_audit_path(v7_full).read_bytes())
        accepted_v7 = directory / "light-free-accepted.lwp"
        accept(accepted_v7, v7_full, v7_copy)
        reproducible(v7_full, v7_copy)
        bad_audit = bytearray(light_free_audit_path(v7_copy).read_bytes()); bad_audit[HEADER + 7] ^= 1
        light_free_audit_path(v7_copy).write_bytes(bad_audit)
        try: read_light_free_audit(v7_copy, v7_header, v7_dim)
        except ValueError as error: assert "checksum" in str(error) or "prefix" in str(error)
        else: raise AssertionError("v7 light-free sidecar corruption was accepted")
        light_free_audit_path(v7_copy).write_bytes(light_free_audit_path(v7_full).read_bytes())
        mismatched_audit = bytearray(light_free_audit_path(v7_copy).read_bytes()); mismatched_audit[HEADER + 1] ^= 1
        light_free_audit_path(v7_copy).write_bytes(mismatched_audit)
        try: read_light_free_audit(v7_copy, v7_header, v7_dim)
        except ValueError as error: assert "prefix" in str(error)
        else: raise AssertionError("v7 sidecar prefix collision was accepted")
    print("selftest ok: authenticated v3/v4/v5 compatibility plus v6 raw-packet and v7 light-free merge, duplicate-read, reproducibility, tamper, dimension, and frozen-world controls")


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(required=True)
    validate_parser = sub.add_parser("validate"); validate_parser.add_argument("paths", nargs="+")
    merge_parser = sub.add_parser("merge"); merge_parser.add_argument("--out", required=True); merge_parser.add_argument("paths", nargs="+")
    accept_parser = sub.add_parser("accept"); accept_parser.add_argument("--out", required=True); accept_parser.add_argument("first"); accept_parser.add_argument("second")
    reproducible_parser = sub.add_parser("reproducible"); reproducible_parser.add_argument("first"); reproducible_parser.add_argument("second")
    sub.add_parser("selftest")
    args = parser.parse_args()
    try:
        if args.__dict__.get("first"):
            if args.__dict__.get("out"):
                accept(args.out, args.first, args.second)
            else:
                reproducible(args.first, args.second)
        elif args.__dict__.get("out"):
            merge(args.out, args.paths)
        elif args.__dict__.get("paths"):
            validate(args.paths)
        else:
            selftest()
    except ValueError as error:
        sys.exit(f"large-parity manifest error: {error}")


if __name__ == "__main__":
    main()
