#!/usr/bin/env python3
"""Rebuilds `fuzz/seeds/<target>/` from data this repository did not author.

Every byte a fuzz target starts from should come from outside the code that
target exercises: a decoder seeded from its own encoder's output explores only
the shapes that encoder already produces, and agrees with it by construction.
So each seed family below names one external producer:

  * `.cache/mc/26.2/generated/reports/packets.json` -- the vanilla generator's
    own packet-id report, the authority on which numeric id a packet name has
    in each connection state.
  * `crates/versions/26.2/tests/fixtures/*.hex` -- packet payloads captured off
    the wire from a real vanilla 26.2 server. Byte-for-byte what a server sent.
  * `.cache/mc/26.2/src/data/minecraft/**` -- the vanilla data pack: loot
    tables, density functions, advancement/dialog/chat-type text components.
  * `.cache/mc/26.2/generated/reports/blocks.json` -- the generator's own
    block-state table, used to spell block-state strings from its property
    names and values rather than from ours.
  * `.cache/mc/survival/world/**` -- a world save written by a real vanilla
    server: its `level.dat`, its player data and its region files.

`.cache/` is not repository state (it is a downloaded jar plus worlds a live
oracle wrote), which is why the *products* of this script are committed and
the script itself needs a populated `.cache/` to re-run. A missing source is a
hard error, never a skipped family: a seed corpus that silently shrank is
indistinguishable from one that was never generated.

Run via `just fuzz-seeds-regen`. Byte-identical output for identical inputs --
files are chosen by sorted name, never sampled randomly, so re-running does
not churn the committed set.
"""

from __future__ import annotations

import argparse
import gzip
import io
import json
import re
import struct
import sys
import zipfile
import zlib
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SEEDS = Path(__file__).resolve().parent
CACHE = REPO / ".cache" / "mc"
REPORTS = CACHE / "26.2" / "generated" / "reports"
VANILLA_DATA = CACHE / "26.2" / "src" / "data" / "minecraft"
VANILLA_ASSETS = CACHE / "26.2" / "src" / "assets" / "minecraft"
SURVIVAL_WORLD = CACHE / "survival" / "world"
V26_2_FIXTURES = REPO / "crates" / "versions" / "26.2" / "tests" / "fixtures"
PACKET_IDS_RS = REPO / "crates" / "versions" / "26.2" / "src" / "generated" / "packet_ids.rs"

# Which packet each captured fixture is a payload of. The fixtures are named
# for what they demonstrate rather than for their packet, and only three of
# them say so in a machine-readable header, so the mapping is spelled out --
# and then every entry is checked against the vanilla packet-id report, so a
# wrong or renamed packet fails loudly instead of seeding a payload under an
# id that never carries it.
FIXTURE_PACKETS = [
    (re.compile(r"^registry_data_"), "configuration", "minecraft:registry_data"),
    (re.compile(r"^update_tags_configuration$"), "configuration", "minecraft:update_tags"),
    (re.compile(r"^item_entity_metadata_"), "play", "minecraft:set_entity_data"),
    (re.compile(r"^potion_contents_complete$"), "play", "minecraft:container_set_slot"),
    (re.compile(r"^tool_component_"), "play", "minecraft:container_set_slot"),
    (re.compile(r"^command_suggestions_"), "play", "minecraft:command_suggestions"),
    (re.compile(r"^command_tree_"), "play", "minecraft:commands"),
]

# Report state name -> the `ConnectionState` ordinal both packet-decode targets
# index their `STATES` array with (byte 0 of their input, taken `% 5`).
STATE_SELECTOR = {"handshake": 0, "status": 1, "login": 2, "configuration": 3, "play": 4}

# Enough of a document to exercise a parser's structure, small enough that the
# committed corpus stays a corpus and not a data dump. libFuzzer's own default
# `-max_len` is 4096 for inputs it generates; a seed above that is still used,
# just never grown, so there is no reason to commit large ones.
MAX_SEED_BYTES = 16 * 1024

# How many block-state strings to keep per distinct property count.
SEEDS_PER_ARITY = 4


class Fatal(Exception):
    pass


def require(path: Path) -> Path:
    if not path.exists():
        raise Fatal(
            f"missing seed source: {path}\n"
            f"  `.cache/` is not repository state -- see docs/fuzzing.md for how to populate it"
        )
    return path


def write_seed(target: str, name: str, data: bytes) -> None:
    out_dir = SEEDS / target
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / name).write_bytes(data)


def read_hex_fixture(path: Path) -> bytes:
    """Same `#`-comment hex format `lodestone_fuzz::read_hex_fixture` reads."""
    tokens = []
    for line in path.read_text().splitlines():
        if line.lstrip().startswith("#"):
            continue
        tokens.extend(line.split())
    return bytes(int(tok, 16) for tok in tokens)


def load_packet_report() -> dict:
    report = json.loads(require(REPORTS / "packets.json").read_text())
    ids: dict[tuple[str, str], int] = {}
    for state, bounds in report.items():
        for name, entry in bounds.get("clientbound", {}).items():
            ids[(state, name)] = entry["protocol_id"]
    if not ids:
        raise Fatal(f"{REPORTS / 'packets.json'} declared no clientbound packets")
    return ids


def load_entry_indices(bound: str) -> dict[tuple[str, str], int]:
    """Position of each packet name in a per-state id table.

    The real-id targets spend one input byte on an index into the selected
    table rather than on a raw id, so a seed for either direction has to know
    the ordering. Only the index comes from here; packet payload bytes still
    come from captured traffic.
    """
    text = require(PACKET_IDS_RS).read_text()
    indices: dict[tuple[str, str], int] = {}
    for state in STATE_SELECTOR:
        module = "handshaking" if state == "handshake" else state
        body = module_body(text, module, PACKET_IDS_RS)
        table = module_body(body, bound, PACKET_IDS_RS)
        entries = re.search(r"static ENTRIES: &\[\(&str, i32\)\] = &\[(.*?)\];", table, re.S)
        if not entries:
            raise Fatal(f"no ENTRIES table under `{module}::{bound}` in {PACKET_IDS_RS}")
        for i, name in enumerate(re.findall(r'\("([^"]+)"', entries.group(1))):
            indices[(state, name)] = i
    return indices


def module_body(text: str, module: str, source: Path) -> str:
    """The `{ ... }` body of `pub mod <module>`, matched by counting braces.

    Brace counting rather than a `\\n    \\}` pattern: the generated file nests
    modules two deep and an empty inner module closes at a different
    indentation than a populated one, so indentation is not a reliable
    delimiter.
    """
    marker = f"pub mod {module} {{"
    start = text.find(marker)
    if start < 0:
        raise Fatal(f"no `pub mod {module}` in {source}")
    depth = 0
    for i in range(start + len(marker) - 1, len(text)):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                return text[start + len(marker) : i]
    raise Fatal(f"unbalanced braces after `pub mod {module}` in {source}")


def seed_packet_decoders() -> list[str]:
    ids = load_packet_report()
    indices = load_entry_indices("clientbound")
    serverbound_indices = load_entry_indices("serverbound")
    notes = []
    fixtures = sorted(require(V26_2_FIXTURES).glob("*.hex"))
    if not fixtures:
        raise Fatal(f"no *.hex captures under {V26_2_FIXTURES}")
    matched = 0
    for fixture in fixtures:
        stem = fixture.stem
        packet = next(((s, p) for pat, s, p in FIXTURE_PACKETS if pat.match(stem)), None)
        if packet is None:
            raise Fatal(
                f"{fixture.name} matches no FIXTURE_PACKETS pattern -- add it (with the "
                f"packet its bytes are a payload of) rather than leaving the capture unseeded"
            )
        state, name = packet
        if (state, name) not in ids:
            raise Fatal(f"{name} is not a clientbound {state} packet in the vanilla report")
        packet_id = ids[(state, name)]
        payload = read_hex_fixture(fixture)[:MAX_SEED_BYTES]
        selector = bytes([STATE_SELECTOR[state]])

        # `v26_2_clientbound_decode`: state byte, then the id as 4 LE bytes.
        write_seed(
            "v26_2_clientbound_decode",
            f"{stem}.bin",
            selector + struct.pack("<i", packet_id) + payload,
        )
        # `v26_2_serverbound_decode` shares that input layout by design, so the
        # same captures are shape-valid there. They are clientbound bytes under
        # a serverbound id, which is exactly what a hostile client can send:
        # real varint/string/NBT structure at an id that does not expect it.
        write_seed(
            "v26_2_serverbound_decode",
            f"{stem}.bin",
            selector + struct.pack("<i", packet_id) + payload,
        )
        # `v26_2_clientbound_decode_by_id`: state byte, then an index into the
        # per-state table.
        if (state, name) not in indices:
            raise Fatal(f"{name} is absent from our {state} clientbound ENTRIES table")
        write_seed(
            "v26_2_clientbound_decode_by_id",
            f"{stem}.bin",
            selector + bytes([indices[(state, name)]]) + payload,
        )
        serverbound_count = sum(1 for entry_state, _ in serverbound_indices if entry_state == state)
        if not serverbound_count:
            raise Fatal(f"our {state} serverbound ENTRIES table is empty")
        # The payload remains a real server capture; the selected id is a
        # valid serverbound arm, even when its schema differs from the
        # captured clientbound payload.
        write_seed(
            "v26_2_serverbound_decode_by_id",
            f"{stem}.bin",
            selector + bytes([matched % serverbound_count]) + payload,
        )
        matched += 1
    notes.append(f"{matched} captured 26.2 packet payloads -> 4 decode targets")
    return notes


def decompress_nbt(raw: bytes) -> bytes:
    if raw[:2] == b"\x1f\x8b":
        return gzip.decompress(raw)
    try:
        return zlib.decompress(raw)
    except zlib.error:
        return raw


def seed_nbt() -> list[str]:
    notes = []
    world = require(SURVIVAL_WORLD)
    level = decompress_nbt((world / "level.dat").read_bytes())
    write_seed("nbt_decode", "survival_level_dat.nbt", level[:MAX_SEED_BYTES])

    players = sorted((world / "players" / "data").glob("*.dat"))
    if not players:
        raise Fatal(f"no player data under {world / 'players' / 'data'}")
    write_seed(
        "nbt_decode",
        "survival_player.nbt",
        decompress_nbt(players[0].read_bytes())[:MAX_SEED_BYTES],
    )

    chunks = extract_chunk_nbt(count=3)
    for i, chunk in enumerate(chunks):
        write_seed("nbt_decode", f"survival_chunk_{i}.nbt", chunk[:MAX_SEED_BYTES])
    notes.append(f"vanilla level.dat, one player .dat and {len(chunks)} region chunks -> nbt_decode")

    # `read_network_nbt` expects an *unnamed* root compound, which is the same
    # document minus the root's two-byte name length and name -- so a real
    # on-disk document is one framing edit away from a real network one.
    if level[0] != 0x0A:
        raise Fatal("vanilla level.dat root is not TAG_Compound; the framing edit below is wrong")
    name_len = struct.unpack_from(">H", level, 1)[0]
    unnamed = level[:1] + level[3 + name_len :]
    write_seed("text_chat_nbt", "survival_level_dat_unnamed_root.nbt", unnamed[:MAX_SEED_BYTES])
    notes.append("vanilla level.dat, root name stripped -> text_chat_nbt (network NBT framing)")
    return notes


def find_region_files() -> list[Path]:
    region_dir = SURVIVAL_WORLD / "dimensions" / "minecraft" / "overworld" / "region"
    files = sorted(p for p in require(region_dir).glob("*.mca") if p.stat().st_size > 8192)
    if not files:
        raise Fatal(f"no non-empty region files under {region_dir}")
    return files


def region_slots(raw: bytes):
    """Yields `(slot, sector_offset, sector_count)` for every populated slot."""
    for slot in range(1024):
        offset_hi, offset_lo, count = struct.unpack_from(">HBB", raw, slot * 4)
        offset = (offset_hi << 8) | offset_lo
        if count and offset >= 2 and (offset + count) * 4096 <= len(raw):
            yield slot, offset, count


def extract_chunk_nbt(count: int) -> list[bytes]:
    """Decompressed chunk payloads from a real region file, smallest first."""
    raw = find_region_files()[0].read_bytes()
    out = []
    for _slot, offset, sectors in sorted(region_slots(raw), key=lambda s: s[2]):
        start = offset * 4096
        length = struct.unpack_from(">I", raw, start)[0]
        if length < 1 or start + 4 + length > len(raw):
            continue
        payload = raw[start + 5 : start + 4 + length]
        try:
            out.append(decompress_nbt(payload))
        except (zlib.error, OSError):
            continue
        if len(out) == count:
            break
    if len(out) < count:
        raise Fatal(f"only {len(out)} of {count} chunks decompressed from a real region file")
    return out


def seed_region() -> list[str]:
    """A real region file trimmed to its smallest single chunk.

    Every byte -- the header's offset/length encoding, the compression scheme
    byte, the deflate stream and the chunk NBT inside it -- is what a vanilla
    server wrote. Only the header's other 1023 slots are zeroed and the unused
    sectors dropped, because a whole region is megabytes and the parser's
    interesting arms are all reachable from one populated slot.
    """
    raw = find_region_files()[0].read_bytes()
    best = min(region_slots(raw), key=lambda s: s[2], default=None)
    if best is None:
        raise Fatal("no populated chunk slot in the region file")
    slot, offset, sectors = best
    body = raw[offset * 4096 : (offset + sectors) * 4096]

    header = bytearray(8192)
    struct.pack_into(">HBB", header, slot * 4, 0, 2, sectors)  # chunk lives at sector 2
    header[4096 + slot * 4 : 4096 + slot * 4 + 4] = raw[4096 + slot * 4 : 4096 + slot * 4 + 4]
    write_seed("anvil_region_parse", "survival_one_chunk.mca", bytes(header) + body)
    return [f"one real region chunk ({sectors} sector(s)) -> anvil_region_parse"]


def sample_json_files(root: Path, limit: int, order_by_size: bool = True) -> list[Path]:
    files = sorted(require(root).rglob("*.json"))
    if not files:
        raise Fatal(f"no *.json under {root}")
    if order_by_size:
        # Smallest and largest: the small ones are the common shapes, the large
        # ones are where nesting depth and every optional field actually appear.
        files.sort(key=lambda p: (p.stat().st_size, p.name))
        half = max(1, limit // 2)
        chosen = files[:half] + files[-(limit - half) :]
    else:
        chosen = files[:limit]
    return [p for p in chosen if p.stat().st_size <= MAX_SEED_BYTES]


def flat_name(root: Path, path: Path) -> str:
    return path.relative_to(root).as_posix().replace("/", "_")


def seed_loot_tables() -> list[str]:
    root = VANILLA_DATA / "loot_table"
    chosen = sample_json_files(root, limit=12)
    for path in chosen:
        write_seed("loot_table_json", flat_name(root, path), path.read_bytes())
    return [f"{len(chosen)} vanilla loot tables -> loot_table_json"]


def seed_density_functions() -> list[str]:
    root = VANILLA_DATA / "worldgen" / "density_function"
    chosen = sample_json_files(root, limit=12)
    for path in chosen:
        write_seed("density_function_json", flat_name(root, path), path.read_bytes())
    return [f"{len(chosen)} vanilla density functions -> density_function_json"]


def collect_text_components() -> list[tuple[str, object]]:
    """Real text components out of the vanilla data pack.

    Three producers, because they exercise different halves of the component
    grammar: advancement display text is the plain `translate`/`text` case,
    chat types carry `with` parameter lists, and dialogs carry click/hover
    events and nested `extra` runs.
    """
    found: list[tuple[str, object]] = []

    advancements = sorted((VANILLA_DATA / "advancement").rglob("*.json"))
    if not advancements:
        raise Fatal(f"no advancements under {VANILLA_DATA / 'advancement'}")
    for path in advancements:
        display = json.loads(path.read_text()).get("display")
        if not display:
            continue
        for field in ("title", "description"):
            if field in display:
                found.append((f"advancement_{path.stem}_{field}", display[field]))
        if len(found) >= 8:
            break

    for path in sorted((VANILLA_DATA / "chat_type").glob("*.json")):
        doc = json.loads(path.read_text())
        for key in ("chat", "narration"):
            if key in doc:
                found.append((f"chat_type_{path.stem}_{key}", doc[key]))

    for path in sorted((VANILLA_DATA / "dialog").glob("*.json")):
        doc = json.loads(path.read_text())
        if "title" in doc:
            found.append((f"dialog_{path.stem}_title", doc["title"]))
        for i, body in enumerate(doc.get("body", [])[:2]):
            if isinstance(body, dict) and "contents" in body:
                found.append((f"dialog_{path.stem}_body{i}", body["contents"]))

    if len(found) < 10:
        raise Fatal(f"only {len(found)} vanilla text components found; expected the data pack to carry more")
    return found


def seed_text_json() -> list[str]:
    components = collect_text_components()
    for name, value in components:
        write_seed("text_chat_json", f"{name}.json", json.dumps(value).encode())
    return [f"{len(components)} vanilla text components -> text_chat_json"]


NBT_END, NBT_BYTE, NBT_INT, NBT_DOUBLE = 0, 1, 3, 6
NBT_STRING, NBT_LIST, NBT_COMPOUND = 8, 9, 10


def nbt_encode_value(value) -> tuple[int, bytes]:
    """A minimal, independent JSON -> NBT encoder.

    Deliberately written here rather than by calling into the workspace: a
    seed produced by the encoder that pairs with the decoder under test only
    ever describes shapes those two already agree on. This one follows the NBT
    format spec directly, so a disagreement between it and our reader is a
    finding rather than a tautology.
    """
    if isinstance(value, bool):
        return NBT_BYTE, bytes([1 if value else 0])
    if isinstance(value, int):
        return NBT_INT, struct.pack(">i", value)
    if isinstance(value, float):
        return NBT_DOUBLE, struct.pack(">d", value)
    if isinstance(value, str):
        raw = value.encode("utf-8")
        return NBT_STRING, struct.pack(">H", len(raw)) + raw
    if isinstance(value, list):
        if not value:
            return NBT_LIST, bytes([NBT_END]) + struct.pack(">i", 0)
        element_types = {nbt_encode_value(v)[0] for v in value}
        if len(element_types) != 1:
            raise Fatal(f"heterogeneous NBT list in a vanilla text component: {value!r}")
        tag = element_types.pop()
        body = b"".join(nbt_encode_value(v)[1] for v in value)
        return NBT_LIST, bytes([tag]) + struct.pack(">i", len(value)) + body
    if isinstance(value, dict):
        out = bytearray()
        for key, inner in value.items():
            tag, body = nbt_encode_value(inner)
            raw_key = key.encode("utf-8")
            out.append(tag)
            out += struct.pack(">H", len(raw_key)) + raw_key
            out += body
        out.append(NBT_END)
        return NBT_COMPOUND, bytes(out)
    raise Fatal(f"unencodable JSON value in a vanilla text component: {value!r}")


def seed_text_nbt() -> list[str]:
    count = 0
    for name, value in collect_text_components():
        if not isinstance(value, (dict, list)):
            # A bare string component has no unnamed-compound network framing.
            continue
        tag, body = nbt_encode_value(value)
        write_seed("text_chat_nbt", f"{name}.nbt", bytes([tag]) + body)
        count += 1
    if count == 0:
        raise Fatal("no compound-shaped vanilla text components to encode as network NBT")
    return [f"{count} vanilla text components, NBT-encoded by this script -> text_chat_nbt"]


def seed_block_states() -> list[str]:
    """Block-state strings spelled from the vanilla block-state report.

    Property names, value spellings and which properties a block even has all
    come from the generator's own `blocks.json`, so a seed cannot inherit a
    misunderstanding from our own parser or our own state tables.
    """
    blocks = json.loads(require(REPORTS / "blocks.json").read_text())
    lines: list[tuple[int, str]] = []
    for name in sorted(blocks):
        properties = blocks[name].get("properties", {})
        if not properties:
            lines.append((0, name))
            continue
        # The default state, plus one variant per property at its last value --
        # enough to cover single-property, multi-property and boolean grammar.
        pairs = [f"{k}={v[0]}" for k, v in sorted(properties.items())]
        lines.append((len(properties), f"{name}[{','.join(pairs)}]"))
        first_key = sorted(properties)[0]
        lines.append((1, f"{name}[{first_key}={properties[first_key][-1]}]"))
    if len(lines) < 100:
        raise Fatal(f"only {len(lines)} block-state strings from blocks.json")
    # A spread over *property count* rather than the first N alphabetically:
    # the grammar arms that differ are "no brackets", "one pair" and "many
    # comma-separated pairs", and the alphabetical head of the block registry
    # is all one shape.
    by_arity: dict[int, list[str]] = {}
    for arity, line in lines:
        by_arity.setdefault(arity, []).append(line)
    chosen: list[str] = []
    for arity in sorted(by_arity):
        chosen.extend(sorted(by_arity[arity])[:SEEDS_PER_ARITY])
    # One seed per string keeps each an independently-mutable libFuzzer input;
    # the parser takes a single string, so a multi-line file would only ever
    # exercise its "this is not a block state" arm.
    for i, line in enumerate(chosen):
        write_seed("block_state_string", f"blocks_report_{i:02}.txt", line.encode())
    return [f"{len(chosen)} block-state strings from blocks.json -> block_state_string"]


def seed_resource_pack_zip() -> list[str]:
    """A real single-entry-per-file zip archive, built from vanilla's own
    language files.

    `ZipSource::from_bytes` parses a whole resource pack's central directory
    and every entry's local header; the *container* here is assembled by this
    script (no vanilla `client.jar` is checked out under `.cache/`, only the
    decompiled source tree's loose asset files), but every byte the archive
    holds -- `assets/minecraft/lang/en_us.json` and `deprecated.json`, each
    truncated to `MAX_SEED_BYTES` -- is real vanilla content, and the zip
    structure (central directory, local headers, deflate streams) is genuine
    `zipfile` output, not our own writer's.
    """
    lang = require(VANILLA_ASSETS / "lang")
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as zf:
        for name in ("en_us.json", "deprecated.json"):
            path = require(lang / name)
            zf.writestr(f"assets/minecraft/lang/{name}", path.read_bytes()[:MAX_SEED_BYTES])
        zf.writestr("assets/.mcassetsroot", b"")
    write_seed("resource_pack_zip_source", "vanilla_lang_pack.zip", buf.getvalue())
    return ["a zip of vanilla's own lang files -> resource_pack_zip_source"]


FAMILIES = [
    seed_packet_decoders,
    seed_nbt,
    seed_region,
    seed_loot_tables,
    seed_density_functions,
    seed_text_json,
    seed_text_nbt,
    seed_block_states,
    seed_resource_pack_zip,
]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="report what would be written without writing it",
    )
    args = parser.parse_args()
    if args.check:
        global write_seed
        write_seed = lambda *_a, **_k: None  # noqa: E731

    notes = []
    try:
        for family in FAMILIES:
            notes.extend(family())
    except Fatal as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    for note in notes:
        print(f"  {note}")
    total = sum(1 for p in SEEDS.rglob("*") if p.is_file() and p.suffix != ".py")
    print(f"{total} seed files under {SEEDS.relative_to(REPO)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
