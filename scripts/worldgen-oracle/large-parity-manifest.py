#!/usr/bin/env python3
"""Validate and deterministically merge packet-payload v2 shard manifests."""
import argparse, hashlib, pathlib, struct, sys, tempfile

MAGIC = b"LWP26P02"; HEADER = 160; WIDTH = 2; DOMAIN = b"lodestone.worldgen.large-parity.manifest/v2"
FMT = ">8sHHHHIqiiiiiiiiQ32s32s28x"

def read(path):
    raw = pathlib.Path(path).read_bytes()
    if len(raw) < HEADER: raise ValueError(f"{path}: shorter than {HEADER}-byte header")
    h = struct.unpack(FMT, raw[:HEADER])
    magic, version, size, algo, schema, protocol, seed, gx0, gx1, gz0, gz1, sx0, sx1, sz0, sz1, count, domain, payload_digest = h
    if magic != MAGIC or version != 2 or size != HEADER or algo != 1 or schema != 2 or protocol != 776 or seed != 42: raise ValueError(f"{path}: unsupported header {h[:7]}")
    if (gx0,gx1,gz0,gz1) != (-500,500,-500,500): raise ValueError(f"{path}: not the required 1001x1001 grid")
    expected = (sx1-sx0+1)*(sz1-sz0+1)
    if sx0 < gx0 or sx1 > gx1 or sz0 < gz0 or sz1 > gz1 or count != expected: raise ValueError(f"{path}: invalid shard bounds/count")
    if domain != hashlib.sha256(DOMAIN).digest(): raise ValueError(f"{path}: schema domain digest differs")
    payload = raw[HEADER:]
    if len(raw) != HEADER + count*WIDTH: raise ValueError(f"{path}: payload size is {len(raw)-HEADER}, expected {count*WIDTH}")
    if payload_digest != hashlib.sha256(payload).digest(): raise ValueError(f"{path}: payload checksum differs (corrupt fingerprint bytes)")
    return h, payload

def validate(paths):
    for p in paths:
        h, _ = read(p); print(f"ok {p}: cx={h[11]}..{h[12]} cz={h[13]}..{h[14]} fingerprints={h[15]}")

def merge(out, paths):
    slots = {}
    for path in paths:
        h, payload = read(path); sx0,sx1,sz0,sz1 = h[11:15]; n = 0
        for cz in range(sz0, sz1+1):
            for cx in range(sx0, sx1+1):
                key = (cx,cz)
                if key in slots: raise ValueError(f"overlap at {key}: {path}")
                slots[key] = payload[n*WIDTH:(n+1)*WIDTH]; n += 1
    required = 1001*1001
    if len(slots) != required: raise ValueError(f"incomplete merge: {len(slots)}/{required}; missing shards are not silently zero-filled")
    payload = bytearray()
    for cz in range(-500,501):
        for cx in range(-500,501): payload.extend(slots[(cx,cz)])
    header = struct.pack(FMT, MAGIC,2,HEADER,1,2,776,42,-500,500,-500,500,-500,500,-500,500,required,hashlib.sha256(DOMAIN).digest(),hashlib.sha256(payload).digest())
    with open(out, "wb") as f:
        f.write(header)
        f.write(payload)
    print(f"merged {required} fingerprints into {out}")

def selftest():
    """Exercise successful merge and a persisted one-bit corruption rejection.
    The payloads are synthetic fingerprints, never generated chunks."""
    def make(path, sx0, sx1, word):
        count = (sx1-sx0+1)*1001; payload = struct.pack(">H", word) * count
        header = struct.pack(FMT, MAGIC,2,HEADER,1,2,776,42,-500,500,-500,500,sx0,sx1,-500,500,count,hashlib.sha256(DOMAIN).digest(),hashlib.sha256(payload).digest())
        pathlib.Path(path).write_bytes(header + payload)
    with tempfile.TemporaryDirectory(prefix="large-parity-") as d:
        left,right,out = (pathlib.Path(d)/n for n in ("left.lwp","right.lwp","full.lwp"))
        make(left,-500,0,0x1111); make(right,1,500,0x2222); merge(out,[left,right]); h,payload=read(out)
        assert h[15] == 1_002_001 and payload[:WIDTH] == struct.pack(">H",0x1111) and payload[501*WIDTH:502*WIDTH] == struct.pack(">H",0x2222)
        bad = pathlib.Path(d)/"bad.lwp"; bad.write_bytes(left.read_bytes()); raw=bytearray(bad.read_bytes()); raw[HEADER+3] ^= 1; bad.write_bytes(raw)
        try: read(bad)
        except ValueError: pass
        else: raise AssertionError("one-bit payload corruption was accepted")
    print("selftest ok: deterministic full merge and one-bit corruption detection")

def main():
    p=argparse.ArgumentParser(); s=p.add_subparsers(required=True); v=s.add_parser("validate"); v.add_argument("paths", nargs="+"); m=s.add_parser("merge"); m.add_argument("--out", required=True); m.add_argument("paths", nargs="+"); s.add_parser("selftest"); a=p.parse_args()
    try:
        if a.__dict__.get("out"): merge(a.out,a.paths)
        elif a.__dict__.get("paths"): validate(a.paths)
        else: selftest()
    except ValueError as e: sys.exit(f"large-parity manifest error: {e}")
if __name__ == "__main__": main()
