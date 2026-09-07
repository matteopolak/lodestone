#!/usr/bin/env python3
"""Validate and deterministically merge frozen-world semantic parity manifests."""
import argparse, hashlib, pathlib, struct, sys, tempfile

MAGIC = b"LWP26P03"
MAGIC_V4 = b"LWP26P04"
MAGIC_V5 = b"LWP26P05"
HEADER = 256
WIDTH = 32
DOMAIN = b"lodestone.worldgen.large-parity.manifest/v3/semantic"
DOMAIN_V4 = b"lodestone.worldgen.large-parity.manifest/v4/semantic"
DOMAIN_V5 = b"lodestone.worldgen.large-parity.manifest/v5/semantic"
GRID_MIN = -250
GRID_MAX = 250
GRID_SIDE = GRID_MAX - GRID_MIN + 1
GRID_COUNT = GRID_SIDE * GRID_SIDE
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


def make_header(version, dim, sx0, sx1, sz0, sz1, count, frozen, payload_digest):
    if version == 3:
        magic, schema, domain, dim_digest = MAGIC, 3, DOMAIN, bytes(32)
    elif version == 4:
        magic, schema, domain = MAGIC_V4, 4, DOMAIN_V4
        try:
            dim_digest = hashlib.sha256(DIMENSIONS[dim]).digest()
        except KeyError as error:
            raise ValueError(f"unsupported dimension {dim!r}") from error
    elif version == 5:
        magic, schema, domain = MAGIC_V5, 5, DOMAIN_V5
        try:
            dim_digest = hashlib.sha256(DIMENSIONS[dim]).digest()
        except KeyError as error:
            raise ValueError(f"unsupported dimension {dim!r}") from error
    else:
        raise ValueError(f"unsupported manifest version {version}")
    header = struct.pack(FMT, magic, version, HEADER, 2, schema, 776, 42,
                         GRID_MIN, GRID_MAX, GRID_MIN, GRID_MAX, sx0, sx1, sz0, sz1,
                         count, WIDTH, 0, hashlib.sha256(domain).digest(), frozen,
                         payload_digest)
    return header[:168] + dim_digest + header[200:] if version in (4, 5) else header


def read(path):
    raw = pathlib.Path(path).read_bytes()
    if len(raw) < HEADER:
        raise ValueError(f"{path}: shorter than {HEADER}-byte v3 header")
    h = struct.unpack(FMT, raw[:HEADER])
    (magic, version, size, algorithm, schema, protocol, seed, gx0, gx1, gz0,
     gz1, sx0, sx1, sz0, sz1, count, width, reserved, domain, frozen,
     payload_digest) = h
    if magic not in (MAGIC, MAGIC_V4, MAGIC_V5):
        if magic == b"LWP26P02":
            raise ValueError(f"{path}: v2 stores raw 16-bit packet fingerprints and is rejected; regenerate from a frozen world as v3")
        raise ValueError(f"{path}: unsupported manifest magic {magic!r}")
    valid_v3 = (magic == MAGIC and version == 3 and schema == 3)
    valid_v4 = (magic == MAGIC_V4 and version == 4 and schema == 4)
    valid_v5 = (magic == MAGIC_V5 and version == 5 and schema == 5)
    if not (valid_v3 or valid_v4 or valid_v5) or (size, algorithm, protocol, seed, width, reserved) != (HEADER, 2, 776, 42, WIDTH, 0):
        raise ValueError(f"{path}: unsupported parity manifest header")
    if (gx0, gx1, gz0, gz1) != (GRID_MIN, GRID_MAX, GRID_MIN, GRID_MAX):
        raise ValueError(f"{path}: not the required {GRID_SIDE}x{GRID_SIDE} grid")
    expected = (sx1-sx0+1)*(sz1-sz0+1)
    if sx0 < gx0 or sx1 > gx1 or sz0 < gz0 or sz1 > gz1 or count != expected:
        raise ValueError(f"{path}: invalid shard bounds/count")
    expected_domain = DOMAIN if version == 3 else DOMAIN_V4 if version == 4 else DOMAIN_V5
    if domain != hashlib.sha256(expected_domain).digest():
        raise ValueError(f"{path}: semantic-record schema digest differs")
    dim = dimension(raw, h)
    if frozen == bytes(32):
        raise ValueError(f"{path}: missing frozen-world identity")
    payload = raw[HEADER:]
    if len(payload) != count*WIDTH:
        raise ValueError(f"{path}: payload size is {len(payload)}, expected {count*WIDTH}")
    if payload_digest != hashlib.sha256(payload).digest():
        raise ValueError(f"{path}: payload checksum differs")
    return h, payload, dim


def validate(paths):
    for path in paths:
        h, _, dim = read(path)
        print(f"ok {path}: dimension={dim} cx={h[11]}..{h[12]} cz={h[13]}..{h[14]} semantic_sha256={h[15]} frozen={h[19].hex()}")


def merge(out, paths):
    slots, frozen, dim, version = {}, None, None, None
    for path in paths:
        h, payload, shard_dim = read(path)
        if version is None:
            version, dim = h[1], shard_dim
        elif (h[1], shard_dim) != (version, dim):
            raise ValueError(f"{path}: manifest format or dimension differs; do not merge dimensions or schema versions")
        if frozen is None:
            frozen = h[19]
        elif frozen != h[19]:
            raise ValueError(f"{path}: frozen-world identity differs; never merge independently generated worlds")
        sx0, sx1, sz0, sz1 = h[11:15]
        record = 0
        for cz in range(sz0, sz1+1):
            for cx in range(sx0, sx1+1):
                key = (cx, cz)
                if key in slots:
                    raise ValueError(f"overlap at {key}: {path}")
                slots[key] = payload[record*WIDTH:(record+1)*WIDTH]
                record += 1
    required = GRID_COUNT
    if len(slots) != required:
        raise ValueError(f"incomplete merge: {len(slots)}/{required}; missing shards are not silently zero-filled")
    payload = bytearray()
    for cz in range(GRID_MIN, GRID_MAX + 1):
        for cx in range(GRID_MIN, GRID_MAX + 1):
            payload.extend(slots[(cx, cz)])
    header = make_header(version, dim, GRID_MIN, GRID_MAX, GRID_MIN, GRID_MAX,
                         required, frozen, hashlib.sha256(payload).digest())
    pathlib.Path(out).write_bytes(header + payload)
    print(f"merged {required} full semantic SHA-256 digests into {out}")


def accept(out, first, second):
    """Freeze a baseline only after two independent read-only exports agree."""
    first_header, first_payload, first_dim = read(first)
    second_header, second_payload, second_dim = read(second)
    if first_header[11:16] != (GRID_MIN, GRID_MAX, GRID_MIN, GRID_MAX, GRID_COUNT):
        raise ValueError(f"{first}: duplicate-read acceptance requires the complete {GRID_SIDE}x{GRID_SIDE} manifest")
    if second_header[11:16] != first_header[11:16]:
        raise ValueError("duplicate frozen-world reads cover different bounds")
    if second_header[1] != first_header[1] or second_header[18:20] != first_header[18:20] or second_dim != first_dim:
        raise ValueError("duplicate frozen-world reads have different semantic schemas, dimensions, or frozen-world identities")
    if second_payload != first_payload:
        raise ValueError("duplicate frozen-world reads differ; baseline is not accepted")
    pathlib.Path(out).write_bytes(pathlib.Path(first).read_bytes())
    print(f"accepted duplicate-read semantic baseline into {out}")


def reproducible(first, second):
    """Require independent materializations to agree without sharing a root id."""
    first_header, first_payload, first_dim = read(first)
    second_header, second_payload, second_dim = read(second)
    complete = (GRID_MIN, GRID_MAX, GRID_MIN, GRID_MAX, GRID_COUNT)
    if first_header[11:16] != complete:
        raise ValueError(f"{first}: independent-root reproducibility requires the complete {GRID_SIDE}x{GRID_SIDE} manifest")
    if second_header[11:16] != complete:
        raise ValueError(f"{second}: independent-root reproducibility requires the complete {GRID_SIDE}x{GRID_SIDE} manifest")
    if second_header[1] != first_header[1] or second_header[18] != first_header[18]:
        raise ValueError("independent materializations have different semantic schemas")
    if second_dim != first_dim:
        raise ValueError("independent materializations have different dimensions")
    if second_header[11:16] != first_header[11:16]:
        raise ValueError("independent materializations cover different geometry")
    if second_payload != first_payload:
        raise ValueError("independent materializations differ in semantic payload")
    print(f"independent materializations reproduce semantic payload: dimension={first_dim} cx={first_header[11]}..{first_header[12]} cz={first_header[13]}..{first_header[14]}")


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
    print("selftest ok: authenticated v3/v4/v5 merge, duplicate-read and independent-root controls, tamper/world/dimension/schema controls, v2 refusal")


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
