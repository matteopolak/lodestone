#!/usr/bin/env python3
"""Validate and deterministically merge frozen-world semantic v3 shard manifests."""
import argparse, hashlib, pathlib, struct, sys, tempfile

MAGIC = b"LWP26P03"
HEADER = 256
WIDTH = 32
DOMAIN = b"lodestone.worldgen.large-parity.manifest/v3/semantic"
GRID_MIN = -250
GRID_MAX = 250
GRID_SIDE = GRID_MAX - GRID_MIN + 1
GRID_COUNT = GRID_SIDE * GRID_SIDE
# magic, version, header, digest algorithm, schema, protocol, seed, global/shard
# bounds, record count, per-record digest width, reserved, schema/world/payload SHA-256
FMT = ">8sHHHHIqiiiiiiiiQHH32s32s32s88x"


def read(path):
    raw = pathlib.Path(path).read_bytes()
    if len(raw) < HEADER:
        raise ValueError(f"{path}: shorter than {HEADER}-byte v3 header")
    h = struct.unpack(FMT, raw[:HEADER])
    (magic, version, size, algorithm, schema, protocol, seed, gx0, gx1, gz0,
     gz1, sx0, sx1, sz0, sz1, count, width, reserved, domain, frozen,
     payload_digest) = h
    if magic != MAGIC:
        if magic == b"LWP26P02":
            raise ValueError(f"{path}: v2 stores raw 16-bit packet fingerprints and is rejected; regenerate from a frozen world as v3")
        raise ValueError(f"{path}: unsupported manifest magic {magic!r}")
    if (version, size, algorithm, schema, protocol, seed, width, reserved) != (3, HEADER, 2, 3, 776, 42, WIDTH, 0):
        raise ValueError(f"{path}: unsupported v3 header")
    if (gx0, gx1, gz0, gz1) != (GRID_MIN, GRID_MAX, GRID_MIN, GRID_MAX):
        raise ValueError(f"{path}: not the required {GRID_SIDE}x{GRID_SIDE} grid")
    expected = (sx1-sx0+1)*(sz1-sz0+1)
    if sx0 < gx0 or sx1 > gx1 or sz0 < gz0 or sz1 > gz1 or count != expected:
        raise ValueError(f"{path}: invalid shard bounds/count")
    if domain != hashlib.sha256(DOMAIN).digest():
        raise ValueError(f"{path}: semantic-record schema digest differs")
    if frozen == bytes(32):
        raise ValueError(f"{path}: missing frozen-world identity")
    payload = raw[HEADER:]
    if len(payload) != count*WIDTH:
        raise ValueError(f"{path}: payload size is {len(payload)}, expected {count*WIDTH}")
    if payload_digest != hashlib.sha256(payload).digest():
        raise ValueError(f"{path}: payload checksum differs")
    return h, payload


def validate(paths):
    for path in paths:
        h, _ = read(path)
        print(f"ok {path}: cx={h[11]}..{h[12]} cz={h[13]}..{h[14]} semantic_sha256={h[15]} frozen={h[19].hex()}")


def merge(out, paths):
    slots, frozen = {}, None
    for path in paths:
        h, payload = read(path)
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
    header = struct.pack(FMT, MAGIC, 3, HEADER, 2, 3, 776, 42,
                         GRID_MIN, GRID_MAX, GRID_MIN, GRID_MAX, GRID_MIN, GRID_MAX, GRID_MIN, GRID_MAX,
                         required, WIDTH, 0, hashlib.sha256(DOMAIN).digest(),
                         frozen, hashlib.sha256(payload).digest())
    pathlib.Path(out).write_bytes(header + payload)
    print(f"merged {required} full semantic SHA-256 digests into {out}")


def accept(out, first, second):
    """Freeze a baseline only after two independent read-only exports agree."""
    first_header, first_payload = read(first)
    second_header, second_payload = read(second)
    if first_header[11:16] != (GRID_MIN, GRID_MAX, GRID_MIN, GRID_MAX, GRID_COUNT):
        raise ValueError(f"{first}: duplicate-read acceptance requires the complete {GRID_SIDE}x{GRID_SIDE} manifest")
    if second_header[11:16] != first_header[11:16]:
        raise ValueError("duplicate frozen-world reads cover different bounds")
    if second_header[18:20] != first_header[18:20]:
        raise ValueError("duplicate frozen-world reads have different semantic schemas or frozen-world identities")
    if second_payload != first_payload:
        raise ValueError("duplicate frozen-world reads differ; baseline is not accepted")
    pathlib.Path(out).write_bytes(pathlib.Path(first).read_bytes())
    print(f"accepted duplicate-read semantic baseline into {out}")


def selftest():
    """Exercise a full merge, payload tampering, incompatible worlds, and v2 refusal."""
    def make(path, sx0, sx1, byte, frozen):
        count = (sx1-sx0+1)*GRID_SIDE
        payload = bytes([byte])*WIDTH*count
        header = struct.pack(FMT, MAGIC, 3, HEADER, 2, 3, 776, 42,
                             GRID_MIN, GRID_MAX, GRID_MIN, GRID_MAX, sx0, sx1, GRID_MIN, GRID_MAX,
                             count, WIDTH, 0, hashlib.sha256(DOMAIN).digest(),
                             frozen, hashlib.sha256(payload).digest())
        pathlib.Path(path).write_bytes(header + payload)
    with tempfile.TemporaryDirectory(prefix="large-parity-v3-") as directory:
        directory = pathlib.Path(directory)
        left, right, out = directory/"left.lwp", directory/"right.lwp", directory/"full.lwp"
        frozen = hashlib.sha256(b"frozen world control").digest()
        make(left, GRID_MIN, 0, 0x11, frozen); make(right, 1, GRID_MAX, 0x22, frozen)
        merge(out, [left, right]); h, payload = read(out)
        left_width = 0 - GRID_MIN + 1
        assert h[15] == GRID_COUNT and payload[:WIDTH] == bytes([0x11])*WIDTH and payload[left_width*WIDTH:(left_width+1)*WIDTH] == bytes([0x22])*WIDTH
        duplicate = directory/"duplicate.lwp"; accepted = directory/"accepted.lwp"; duplicate.write_bytes(out.read_bytes()); accept(accepted, out, duplicate); assert accepted.read_bytes() == out.read_bytes()
        raw = bytearray(duplicate.read_bytes()); raw[HEADER+3] ^= 1; duplicate.write_bytes(raw)
        try: accept(accepted, out, duplicate)
        except ValueError: pass
        else: raise AssertionError("mismatched duplicate frozen-world read was accepted")
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
    print("selftest ok: authenticated v3 merge, duplicate-read acceptance, tamper/world controls, v2 refusal")


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(required=True)
    validate_parser = sub.add_parser("validate"); validate_parser.add_argument("paths", nargs="+")
    merge_parser = sub.add_parser("merge"); merge_parser.add_argument("--out", required=True); merge_parser.add_argument("paths", nargs="+")
    accept_parser = sub.add_parser("accept"); accept_parser.add_argument("--out", required=True); accept_parser.add_argument("first"); accept_parser.add_argument("second")
    sub.add_parser("selftest")
    args = parser.parse_args()
    try:
        if args.__dict__.get("first"):
            accept(args.out, args.first, args.second)
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
