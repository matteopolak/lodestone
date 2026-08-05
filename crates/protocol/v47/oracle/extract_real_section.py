#!/usr/bin/env python3
"""Extracts a real chunk section out of a real 1.8.9 server's own world save,
as the committed fixture `tests/support/real_1_8_9_section_save.txt`.

This is the generator for that fixture, kept beside it for the same reason
`lodestone-canonical/oracle-java/FlatteningOracle.java` is kept beside its dump:
a committed fixture whose producing program was thrown away is a fixture nobody
can refresh, and "regenerate it" stops being an instruction.

    python3 crates/protocol/v47/oracle/extract_real_section.py \
        .cache/mc/1.8.9/world/region \
        crates/protocol/v47/tests/support/real_1_8_9_section_save.txt

Pure standard library on purpose -- no Docker, no JDK, no running server, no pip
install. That is the point: this repo's 1.8.9 oracle is Docker-managed and
started by no script here, and a session with a broken Docker (or no local JVM)
must still be able to regenerate the evidence.

# Why the world save is a legitimate external oracle

Anvil 1.8 stores a section's block identity as a 4096-byte `Blocks` array (plus
an optional `Add` nibble array for ids past 255) and a 4096-nibble `Data` array,
both indexed YZX. That is *the same* (id, meta) pair, in *the same* order, that
`map_chunk` puts on the wire as a 16-bit little-endian `(id << 4) | meta`
composite. The real vanilla server generated and wrote these bytes; nothing in
this repo produced them.

What it is NOT authoritative for: the wire *framing* around those values (that
is `packets/chunk.rs`'s own hermetic layout tests and the live join gate), and
it cannot cover metas a flat world never contains -- see the fixture's consuming
test, which asserts that limitation rather than hoping about it.

# Choosing what to extract

`REGION`/`WANT_SECTION_Y` below pick the section. Run with `--survey` to print
the whole world's distinct (id, meta) histogram first, which is how the
"4 distinct pairs, all meta=0" property of the flat world was established.
"""

import argparse
import collections
import hashlib
import struct
import sys
import zlib
from pathlib import Path

# Which section to lift. Any populated chunk works; these are recorded so the
# fixture header and this script cannot disagree about provenance.
REGION = "r.-1.-1.mca"
WANT_SECTION_Y = 0

TAG_END, TAG_BYTE, TAG_SHORT, TAG_INT, TAG_LONG = 0, 1, 2, 3, 4
TAG_FLOAT, TAG_DOUBLE, TAG_BYTE_ARRAY, TAG_STRING = 5, 6, 7, 8
TAG_LIST, TAG_COMPOUND, TAG_INT_ARRAY, TAG_LONG_ARRAY = 9, 10, 11, 12


class Reader:
    """Big-endian cursor over the inflated NBT payload."""

    def __init__(self, buf):
        self.buf = buf
        self.pos = 0

    def take(self, n):
        out = self.buf[self.pos:self.pos + n]
        if len(out) != n:
            raise EOFError(f"want {n} bytes at {self.pos}, have {len(out)}")
        self.pos += n
        return out

    def u1(self):
        return self.take(1)[0]

    def u2(self):
        return struct.unpack(">H", self.take(2))[0]

    def i4(self):
        return struct.unpack(">i", self.take(4))[0]

    def string(self):
        return self.take(self.u2()).decode("utf-8", "replace")


def payload(r, tag):
    if tag == TAG_BYTE:
        return struct.unpack(">b", r.take(1))[0]
    if tag == TAG_SHORT:
        return struct.unpack(">h", r.take(2))[0]
    if tag == TAG_INT:
        return r.i4()
    if tag == TAG_LONG:
        return struct.unpack(">q", r.take(8))[0]
    if tag == TAG_FLOAT:
        return struct.unpack(">f", r.take(4))[0]
    if tag == TAG_DOUBLE:
        return struct.unpack(">d", r.take(8))[0]
    if tag == TAG_BYTE_ARRAY:
        return r.take(r.i4())
    if tag == TAG_STRING:
        return r.string()
    if tag == TAG_LIST:
        element = r.u1()
        count = r.i4()
        return [payload(r, element) for _ in range(max(count, 0))]
    if tag == TAG_COMPOUND:
        out = {}
        while True:
            child = r.u1()
            if child == TAG_END:
                return out
            # The name MUST be read before its payload. Written as
            # `out[r.string()] = payload(r, child)` Python evaluates the
            # right-hand side FIRST, which desyncs the whole stream and then
            # fails at EOF thousands of bytes later, pointing nowhere near the
            # cause. This cost a debugging round; do not "simplify" it back.
            key = r.string()
            out[key] = payload(r, child)
    if tag == TAG_INT_ARRAY:
        return [r.i4() for _ in range(r.i4())]
    if tag == TAG_LONG_ARRAY:
        return [struct.unpack(">q", r.take(8))[0] for _ in range(r.i4())]
    raise ValueError(f"unknown NBT tag {tag}")


def parse_nbt(raw):
    r = Reader(raw)
    tag = r.u1()
    r.string()  # root name, always empty in a chunk
    return payload(r, tag)


def chunks(path):
    """Yields every stored chunk's NBT from one region file.

    Region layout: 8 KiB header; the first 4 KiB is 1024 x (3-byte sector
    offset, 1-byte sector count). Payload at offset*4096 is a 4-byte length, a
    1-byte compression id (2 = zlib, 1 = gzip), then the compressed NBT.
    """
    data = path.read_bytes()
    for slot in range(1024):
        offset = int.from_bytes(data[slot * 4:slot * 4 + 3], "big")
        sectors = data[slot * 4 + 3]
        if offset == 0 or sectors == 0:
            continue
        base = offset * 4096
        length = int.from_bytes(data[base:base + 4], "big")
        compression = data[base + 4]
        blob = data[base + 5:base + 4 + length]
        raw = zlib.decompress(blob, 47 if compression == 1 else 15)
        yield parse_nbt(raw)


def nibble(array, index):
    byte = array[index >> 1]
    return (byte >> 4) & 0xF if (index & 1) else byte & 0xF


def sections(region_dir):
    """Yields `(chunk_x, chunk_z, section_y, cells)` for every stored section."""
    for path in sorted(Path(region_dir).glob("*.mca")):
        for nbt in chunks(path):
            level = nbt.get("Level", {})
            for section in level.get("Sections", []):
                blocks, data = section.get("Blocks"), section.get("Data")
                if blocks is None or data is None:
                    continue
                add = section.get("Add")
                cells = []
                for i in range(4096):
                    block_id = blocks[i]
                    if add is not None:
                        block_id |= nibble(add, i) << 8
                    cells.append((block_id, nibble(data, i)))
                yield path, level.get("xPos"), level.get("zPos"), section.get("Y", 0), cells


def survey(region_dir):
    histogram = collections.Counter()
    for _, _, _, _, cells in sections(region_dir):
        histogram.update(cells)
    print(f"# distinct (id, meta) pairs: {len(histogram)}")
    for (block_id, meta), count in sorted(histogram.items(), key=lambda kv: -kv[1]):
        print(f"id={block_id:4d} meta={meta:2d} count={count:9d}")
    metas = {meta for _, meta in histogram}
    if metas == {0}:
        print(
            "\n# NOTE: every pair has meta=0, so this world CANNOT exercise the meta\n"
            "# half of the composite. A gate fed only this is vacuous by input (the\n"
            "# *world* species). See tests/canonicalisation.rs's adversarial arm."
        )


def write_fixture(region_dir, out_path):
    path = Path(region_dir) / REGION
    digest = hashlib.sha256(path.read_bytes()).hexdigest()

    for _, chunk_x, chunk_z, section_y, cells in sections(region_dir):
        if section_y != WANT_SECTION_Y:
            continue
        tokens = [f"{block_id}:{meta}" for block_id, meta in cells]
        header = [
            "# Real 1.8.9 chunk section, read out of the vanilla 1.8.9 server's",
            "# OWN world save. Nothing in this repo produced these block values:",
            "# the server generated and wrote them (level-type=FLAT).",
            "#",
            f"# Source:        .cache/mc/1.8.9/world/region/{REGION}",
            f"# SHA-256:       {digest}",
            f"# Chunk:         xPos={chunk_x} zPos={chunk_z}",
            f"# Section:       Y={section_y} (world y 0..15)",
            "#",
            "# Regenerate with crates/protocol/v47/oracle/extract_real_section.py",
            "#",
            "# Anvil 1.8 stores a section's block identity as a 4096-byte `Blocks`",
            "# array (plus an optional `Add` nibble array for ids past 255) and a",
            "# 4096-nibble `Data` array, both indexed YZX -- the SAME order and the",
            "# SAME (id, meta) pair that `map_chunk` puts on the wire as the",
            "# 16-bit little-endian composite `(id << 4) | meta`. So a gate can",
            "# rebuild this section's wire bytes from real server output without a",
            "# running server (Docker/JDK are not available in every session).",
            "#",
            "# Format: 4096 whitespace-separated `<id>:<meta>` tokens in YZX order",
            "# (index = y<<8 | z<<4 | x), 16 tokens per line.",
            "",
        ]
        rows = [" ".join(tokens[i:i + 16]) for i in range(0, 4096, 16)]
        Path(out_path).write_text("\n".join(header + rows) + "\n")
        print(f"wrote {out_path}: chunk ({chunk_x},{chunk_z}) section Y={section_y}")
        print(f"distinct pairs: {sorted(set(cells))}")
        return 0

    print(f"no section Y={WANT_SECTION_Y} found in {path}", file=sys.stderr)
    return 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("region_dir", help="e.g. .cache/mc/1.8.9/world/region")
    parser.add_argument("out_path", nargs="?", help="fixture path to write")
    parser.add_argument(
        "--survey",
        action="store_true",
        help="print the whole world's (id, meta) histogram instead of writing",
    )
    args = parser.parse_args()

    if args.survey:
        survey(args.region_dir)
        return 0
    if not args.out_path:
        parser.error("out_path is required unless --survey is given")
    return write_fixture(args.region_dir, args.out_path)


if __name__ == "__main__":
    sys.exit(main())
