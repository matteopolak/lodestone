#!/usr/bin/env python3
"""Foreign census of the survival oracle's overworld entity regions.

The expected values for `crates/lodestone-server/tests/entity_nbt_vanilla_oracle.rs`.
Standard library only (`struct`, `zlib`, `gzip`) and sharing no code with the
workspace, so the Rust decoder is checked against a reader that cannot share
its misunderstandings. The oracle world changes whenever a live gate or a
player touches it, so rerun this after `just oracle-survival` or any live run:

    python3 scripts/live-oracles/entity-census.py

Writes `.cache/mc/survival/entity-census.json`.
"""
import json, os
import struct, zlib, gzip, glob, collections
def rd(b,o,t):
    if t==1: return b[o],o+1
    if t==2: return None,o+2
    if t==3: return None,o+4
    if t==4: return None,o+8
    if t==5: return None,o+4
    if t==6: return None,o+8
    if t==7: n,=struct.unpack('>i',b[o:o+4]); return None,o+4+n
    if t==8: n,=struct.unpack('>H',b[o:o+2]); return b[o+2:o+2+n].decode('utf-8','replace'),o+2+n
    if t==9:
        et=b[o]; n,=struct.unpack('>i',b[o+1:o+5]); o+=5; out=[]
        for _ in range(n): v,o=rd(b,o,et); out.append(v)
        return out,o
    if t==10:
        d={}
        while True:
            tt=b[o]; o+=1
            if tt==0: return d,o
            n,=struct.unpack('>H',b[o:o+2]); name=b[o+2:o+2+n].decode('utf-8','replace'); o+=2+n
            v,o=rd(b,o,tt); d[name]=v
    if t==11: n,=struct.unpack('>i',b[o:o+4]); return None,o+4+4*n
    if t==12: n,=struct.unpack('>i',b[o:o+4]); return None,o+4+8*n
    raise ValueError(t)
ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..', '.cache', 'mc', 'survival')
files = sorted(glob.glob(os.path.join(ROOT, 'world/dimensions/minecraft/overworld/entities/*.mca')))
chunks = 0; total = 0; ids = collections.Counter()
for p in files:
    f = open(p, 'rb').read()
    if len(f) < 8192: continue
    for i in range(1024):
        off, = struct.unpack('>I', f[i*4:i*4+4])
        if not off: continue
        s = (off >> 8) * 4096; ln, = struct.unpack('>I', f[s:s+4]); comp = f[s+4]; data = f[s+5:s+4+ln]
        raw = zlib.decompress(data) if comp == 2 else gzip.decompress(data) if comp == 1 else data
        root, _ = rd(raw, 3 + struct.unpack('>H', raw[1:3])[0], 10)
        ents = root.get('Entities') or []
        if ents: chunks += 1
        for e in ents: total += 1; ids[e.get('id')] += 1
out = {'region_files': len(files), 'chunks_with_entities': chunks, 'entities': total, 'by_id': dict(sorted(ids.items()))}
with open(os.path.join(ROOT, 'entity-census.json'), 'w') as fh: json.dump(out, fh, indent=1)
print(f"{len(files)} region files, {chunks} chunks with entities, {total} entities")
