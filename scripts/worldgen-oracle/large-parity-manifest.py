#!/usr/bin/env python3
"""Validate and deterministically merge frozen-world parity manifests."""
import argparse, hashlib, mmap, pathlib, struct, sys, tempfile

MAGIC = b"LWP26P03"
MAGIC_V4 = b"LWP26P04"
MAGIC_V5 = b"LWP26P05"
MAGIC_V6 = b"LWP26P06"
HEADER = 256
WIDTH = 32
RAW_WIDTH = 2
DOMAIN = b"lodestone.worldgen.large-parity.manifest/v3/semantic"
DOMAIN_V4 = b"lodestone.worldgen.large-parity.manifest/v4/semantic"
DOMAIN_V5 = b"lodestone.worldgen.large-parity.manifest/v5/semantic"
DOMAIN_V6 = b"lodestone.worldgen.large-parity.manifest/v6/raw-packet"
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
                         count, width, 0, hashlib.sha256(domain).digest(), frozen,
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
    if magic not in (MAGIC, MAGIC_V4, MAGIC_V5, MAGIC_V6):
        if magic == b"LWP26P02":
            raise ValueError(f"{path}: v2 stores raw 16-bit packet fingerprints and is rejected; regenerate from a frozen world as v3")
        raise ValueError(f"{path}: unsupported manifest magic {magic!r}")
    valid_v3 = (magic == MAGIC and version == 3 and schema == 3)
    valid_v4 = (magic == MAGIC_V4 and version == 4 and schema == 4)
    valid_v5 = (magic == MAGIC_V5 and version == 5 and schema == 5)
    valid_v6 = (magic == MAGIC_V6 and version == 6 and schema == 6)
    expected_width = RAW_WIDTH if valid_v6 else WIDTH
    if not (valid_v3 or valid_v4 or valid_v5 or valid_v6) or (size, algorithm, protocol, seed, width, reserved) != (HEADER, 2, 776, 42, expected_width, 0):
        raise ValueError(f"{path}: unsupported parity manifest header")
    grid_min, grid_max = (RAW_GRID_MIN, RAW_GRID_MAX) if valid_v6 else (GRID_MIN, GRID_MAX)
    if (gx0, gx1, gz0, gz1) != (grid_min, grid_max, grid_min, grid_max):
        side = grid_max - grid_min + 1
        raise ValueError(f"{path}: not the required {side}x{side} grid")
    expected = (sx1-sx0+1)*(sz1-sz0+1)
    if sx0 < gx0 or sx1 > gx1 or sz0 < gz0 or sz1 > gz1 or count != expected:
        raise ValueError(f"{path}: invalid shard bounds/count")
    expected_domain = (DOMAIN if version == 3 else DOMAIN_V4 if version == 4 else
                       DOMAIN_V5 if version == 5 else DOMAIN_V6)
    if domain != hashlib.sha256(expected_domain).digest():
        kind = "raw-packet" if version == 6 else "semantic-record"
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
        h, _, dim = read(path)
        kind = "raw-packet" if h[1] == 6 else "semantic"
        print(f"ok {path}: kind={kind} width={h[16]} dimension={dim} cx={h[11]}..{h[12]} cz={h[13]}..{h[14]} payload_sha256={h[20].hex()} frozen={h[19].hex()}")


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
        seen = None
        try:
            for path in paths:
                with _ManifestStream(path) as shard:
                    h, shard_dim = shard.header, shard.dimension
                    compatibility_error = None
                    if version is None:
                        version, dim, record_width = h[1], shard_dim, shard.record_width
                        required = RAW_GRID_COUNT if version == 6 else GRID_COUNT
                        grid_min, grid_max = (RAW_GRID_MIN, RAW_GRID_MAX) if version == 6 else (GRID_MIN, GRID_MAX)
                        slot_file = slot_path.open("w+b")
                        slot_file.truncate(required * record_width)
                        slot_map = mmap.mmap(slot_file.fileno(), 0, access=mmap.ACCESS_WRITE)
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
                    for cz in range(sz0, sz1 + 1):
                        for cx in range(sx0, sx1 + 1):
                            index = (cz - grid_min) * (grid_max - grid_min + 1) + (cx - grid_min)
                            value = next(records)
                            if compatibility_error is not None:
                                continue
                            if _slot_seen(seen, index):
                                if overlap is None:
                                    overlap = f"overlap at ({cx}, {cz}): {path}"
                            else:
                                offset = index * record_width
                                slot_map[offset:offset + record_width] = value
                                seen_count += 1
                    # Exhaust the iterator here so the whole shard is
                    # authenticated before overlap reporting.
                    try:
                        next(records)
                    except StopIteration:
                        pass
                    if compatibility_error is not None:
                        raise compatibility_error
        finally:
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
    kind = "raw packet hashes" if version == 6 else "semantic SHA-256 digests"
    print(f"merged {required} {kind} into {out}")


def accept(out, first, second):
    """Freeze a baseline only after two independent read-only exports agree."""
    first_header, first_payload, first_dim = read(first)
    second_header, second_payload, second_dim = read(second)
    grid_min, grid_max = (RAW_GRID_MIN, RAW_GRID_MAX) if first_header[1] == 6 else (GRID_MIN, GRID_MAX)
    required = RAW_GRID_COUNT if first_header[1] == 6 else GRID_COUNT
    if first_header[11:16] != (grid_min, grid_max, grid_min, grid_max, required):
        raise ValueError(f"{first}: duplicate-read acceptance requires the complete {grid_max-grid_min+1}x{grid_max-grid_min+1} manifest")
    if second_header[11:16] != first_header[11:16]:
        raise ValueError("duplicate frozen-world reads cover different bounds")
    if second_header[1] != first_header[1] or second_header[18:20] != first_header[18:20] or second_dim != first_dim:
        raise ValueError("duplicate frozen-world reads have different semantic schemas, dimensions, or frozen-world identities")
    if second_payload != first_payload:
        raise ValueError("duplicate frozen-world reads differ; baseline is not accepted")
    pathlib.Path(out).write_bytes(pathlib.Path(first).read_bytes())
    print(f"accepted duplicate-read baseline into {out}")


def reproducible(first, second):
    """Require independent materializations to agree without sharing a root id."""
    first_header, first_payload, first_dim = read(first)
    second_header, second_payload, second_dim = read(second)
    raw_records = first_header[1] == 6
    record_kind = "raw packet hash payload" if raw_records else "semantic payload"
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
                                    GRID_MIN, GRID_MAX, legacy_count, WIDTH, 0,
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
        make_v6(v6_left, RAW_GRID_MIN, 0, 0x61); make_v6(v6_right, 1, RAW_GRID_MAX, 0xA2)
        merge(v6_full, [v6_left, v6_right]); v6_header, v6_payload, v6_dim = read(v6_full)
        assert v6_dim == "nether" and v6_header[1] == 6 and v6_header[16] == RAW_WIDTH
        assert len(v6_payload) == RAW_GRID_COUNT * RAW_WIDTH
        assert raw_packet_hash(b"packet body") == hashlib.sha256(b"packet body").digest()[:2]
        v6_copy = directory / "raw-copy.lwp"; v6_copy.write_bytes(v6_full.read_bytes())
        accepted_v6 = directory / "raw-accepted.lwp"; accept(accepted_v6, v6_full, v6_copy)
        reproducible(v6_full, v6_copy)
        changed = bytearray(v6_copy.read_bytes()); changed[HEADER + 1] ^= 1; v6_copy.write_bytes(changed)
        try: read(v6_copy)
        except ValueError as error: assert "checksum" in str(error)
        else: raise AssertionError("v6 payload corruption was accepted")
        bad_dim = directory / "raw-bad-dimension.lwp"
        bad = bytearray(v6_full.read_bytes()); bad[168] ^= 1; bad_dim.write_bytes(bad)
        try: read(bad_dim)
        except ValueError as error: assert "dimension" in str(error)
        else: raise AssertionError("v6 dimension identity corruption was accepted")
    print("selftest ok: authenticated v3/v4/v5 compatibility plus v6 raw-packet merge, duplicate-read, reproducibility, tamper, dimension, and frozen-world controls")


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
