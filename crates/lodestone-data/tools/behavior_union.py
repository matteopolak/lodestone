#!/usr/bin/env python3
"""Join captured behavior to the append-only identity census before Rust emission."""

import argparse
import ast
from dataclasses import dataclass, field
import hashlib
import json
import math
from pathlib import Path
import re
import struct
import tempfile

import canonical_census as census
import identity_staging as identities


ROOT = census.ROOT
GENERATED = ROOT / "crates/lodestone-data/src/generated"
SUPPORT = ROOT / "crates/lodestone-data/tests/support"
CAPTURES = ROOT / ".cache/mc/26.3/behavior-oracles"
SCALARS = ROOT / ".cache/mc/light-properties-capture.y2mj5O"
COLLISION_PREFIX = Path(__file__).parent / "fixtures/collision-26-2-prefix.json"
SOUND_RANGES_PREFIX = Path(__file__).parent / "fixtures/sound-ranges-26-2-prefix.json"
LATEST = census.VERSIONS[1]
BASE = census.VERSIONS[0]


def require(condition, message):
    if not condition:
        raise ValueError(f"behavior union: {message}")


def digest_bytes(data):
    return hashlib.sha256(data).hexdigest()


def rows(path):
    return [line.split() for line in path.read_text().splitlines()
            if line.strip() and not line.startswith("#")]


def flag(value):
    require(value in ("0", "1"), f"invalid flag {value!r}")
    return value == "1"


def bits(value):
    raw = int(value, 16)
    require(0 <= raw < 2**32, f"invalid f32 bits {value!r}")
    require(math.isfinite(struct.unpack(">f", raw.to_bytes(4, "big"))[0]),
            f"non-finite f32 {value!r}")
    return raw


def properties(value):
    if value == "-":
        return {}
    pairs = [pair.split("=", 1) for pair in value.split(",")]
    require(all(len(pair) == 2 for pair in pairs), "invalid property signature")
    require(len(dict(pairs)) == len(pairs) and pairs == sorted(pairs),
            "duplicate or unsorted property signature")
    return dict(pairs)


def state_identity(source, raw, name, signature=None):
    require(0 <= raw < len(source["block_states"]), f"state ID out of range: {raw}")
    expected = source["block_states"][raw]
    actual = census.state_key(name, properties(signature)) if signature is not None else expected
    require(name == expected.split("[", 1)[0] and actual == expected,
            f"state ID/name/properties mismatch: {raw} {name} {signature}")
    return expected


def named_identity(source, domain, raw, name):
    require(0 <= raw < len(source[domain]) and source[domain][raw] == name,
            f"{domain} ID/name mismatch: {raw} {name}")
    return name


def total(values, expected, label):
    require(set(values) == set(expected),
            f"{label} coverage differs: missing {len(set(expected) - set(values))}, "
            f"extra {len(set(values) - set(expected))}")
    return values


def insert(values, key, value, label):
    require(key not in values, f"duplicate {label}: {key}")
    values[key] = value


@dataclass
class Inputs:
    sources: dict
    bundle: dict
    manifest: dict
    columns: dict = field(default_factory=dict)
    provenance: dict = field(default_factory=dict)
    tags: dict = field(default_factory=dict)
    tools: dict = field(default_factory=dict)
    sound_ranges: list[tuple[int, float]] = field(default_factory=list)

    def read(self, path):
        data = path.read_bytes()
        self.provenance[str(path.relative_to(ROOT))] = digest_bytes(data)
        return [line.split() for line in data.decode().splitlines()
                if line.strip() and not line.startswith("#")]

    def add(self, name, version, values, domain="block_states"):
        total(values, self.sources[version][domain], f"{name}/{version}")
        self.columns.setdefault(name, {"domain": domain, "versions": {}})["versions"][version] = values

    def authenticate(self, directory, filenames, metadata_name="manifest.json"):
        metadata, metadata_digest = census.read_report(directory / metadata_name)
        self.provenance[str((directory / metadata_name).relative_to(ROOT))] = metadata_digest
        release = metadata.get("actual_release", metadata.get("jar_identity", {}))
        require(not release or (release["id"], release["protocol_version"]) == (LATEST, 777),
                f"wrong-release capture manifest: {directory.name}")
        reported = metadata.get("blocks_report_sha256")
        if reported is not None:
            require(reported == self.sources[LATEST]["source_sha256"]["blocks.json"],
                    f"capture report hash differs: {directory.name}")
        reported = metadata.get("registries_report_sha256")
        if reported is not None:
            require(reported == self.sources[LATEST]["source_sha256"]["registries.json"],
                    f"capture registry hash differs: {directory.name}")
        reported = metadata.get("server_classes_sha256")
        if reported is not None:
            path = ROOT / ".cache/mc/26.3/versions/26.3/server-26.3.jar"
            key = str(path.relative_to(ROOT))
            if key not in self.provenance:
                self.provenance[key] = digest_bytes(path.read_bytes())
            require(reported == self.provenance[key], f"capture jar hash differs: {directory.name}")
        for filename in filenames:
            expected = metadata.get("raw_sha256")
            if "captures" in metadata:
                capture = metadata["captures"][filename.split(".", 1)[0]]
                expected = capture["semantic_join_sha256"] if ".semantic." in filename else capture["raw_sha256"]
            require(expected is not None, f"missing capture digest: {filename}")
            require(digest_bytes((directory / filename).read_bytes()) == expected,
                    f"capture digest differs: {filename}")


def compact_flags(records, source, labels):
    header = next((row for row in records if row[0] == "C"), None)
    require(header is not None and int(header[1]) == len(source["block_states"]), "compact wrong-release header")
    if len(header) >= 3:
        require(int(header[2]) == len(source["blocks"]), "compact block census")
    boundaries = [(raw, key.split("[", 1)[0]) for raw, key in enumerate(source["block_states"])
                  if raw == 0 or key.split("[", 1)[0] != source["block_states"][raw - 1].split("[", 1)[0]]
    actual = [(int(row[1]), row[2]) for row in records if row[0] == "B"]
    require(actual == boundaries, "compact capture block boundaries differ")
    result = {label: [] for label in labels}
    for row in records:
        if row[0] == "P" and row[1] in result:
            require(len(row) == 4 and int(row[2]) == len(result[row[1]]), "compact flag gap or duplicate")
            require(set(row[3]) <= {"0", "1"}, "compact flag domain")
            result[row[1]].extend(character == "1" for character in row[3])
    require(all(len(column) == len(source["block_states"]) for column in result.values()),
            "compact flag capture is incomplete")
    return {label: dict(zip(source["block_states"], values)) for label, values in result.items()}


def double_boxes(tokens, count, lossless=False):
    require(len(tokens) == 6 * count, "box coordinate cardinality")
    coords = []
    for token in tokens:
        raw = int(token, 16)
        require(0 <= raw < 2**64, "double coordinate width")
        value = struct.unpack(">d", raw.to_bytes(8, "big"))[0]
        narrowed = struct.unpack(">f", struct.pack(">f", value))[0]
        require(math.isfinite(value) and math.isfinite(narrowed), "non-finite coordinate")
        require(not lossless or narrowed == value, "collision coordinate is not lossless f32")
        coords.append(narrowed)
    boxes = tuple(tuple(coords[start:start + 6]) for start in range(0, len(coords), 6))
    require(all(all(box[axis] <= box[axis + 3] for axis in range(3)) for box in boxes), "inverted box")
    return boxes


def outline_columns(records, source):
    require(next((row for row in records if row[0] == "C"), None) == ["C", str(len(source["block_states"]))],
            "outline wrong-release header")
    shapes = {"O": [], "X": []}
    indices = {"O": [], "X": []}
    boundaries = [(int(row[1]), row[2]) for row in records if row[0] == "B"]
    expected = [(index, key.split("[", 1)[0]) for index, key in enumerate(source["block_states"])
                if index == 0 or key.split("[", 1)[0] != source["block_states"][index - 1].split("[", 1)[0]]
    require(boundaries == expected, "outline block boundaries differ")
    for row in records:
        if row[0] == "S":
            label = row[1]
            require(label in shapes and int(row[2]) == len(shapes[label]), "outline shape ordering")
            shapes[label].append(double_boxes(row[4:], int(row[3])))
        elif row[0] == "P":
            label = row[1]
            require(label in indices and int(row[2]) == len(indices[label]), "outline state ordering")
            indices[label].extend(map(int, row[3:]))
    result = {}
    for label in shapes:
        require(len(indices[label]) == len(source["block_states"]), "outline total coverage")
        require(all(0 <= value < len(shapes[label]) for value in indices[label]), "outline shape range")
        result[label] = dict(zip(source["block_states"], [shapes[label][value] for value in indices[label]]))
    return result


def sound_columns(records, source):
    header = next((row for row in records if row[0] == "C"), None)
    require(header is not None and list(map(int, header[1:3])) == [len(source["block_states"]), len(source["blocks"])],
            "sound wrong-release header")
    names, entries, states = {}, [], []
    for row in records:
        if row[0] == "N":
            insert(names, int(row[1]), row[2], "sound name")
        elif row[0] == "T":
            require(int(row[1]) == len(entries), "sound entry ordering")
            sound_names = tuple(names[int(raw)] for raw in row[4:])
            require(len(sound_names) == 5, "sound entry columns")
            entries.append((bits(row[2]), bits(row[3]), *sound_names))
        elif row[0] == "R":
            require(len(row) == 3, "sound run columns")
            entry = int(row[1])
            require(0 <= entry < len(entries) and int(row[2]) > 0, "sound entry range")
            states.extend([entries[entry]] * int(row[2]))
    require(len(states) == len(source["block_states"]), "sound total coverage")
    return dict(zip(source["block_states"], states))


def rust_array_body(text, name):
    match = re.search(r"pub (?:static|const) " + re.escape(name) + r":.*?= \[", text)
    require(match is not None, f"missing retained Rust array: {name}")
    start = match.end()
    depth = 1
    for end in range(start, len(text)):
        depth += (text[end] == "[") - (text[end] == "]")
        if depth == 0:
            return re.sub(r"//[^\n]*", "", text[start:end])
    raise ValueError(f"behavior union: unterminated retained array: {name}")


def collision_snapshot(text, source):
    body = rust_array_body(text, "SHAPES")
    shape_parts = []
    cursor = 0
    while (start := body.find("&[", cursor)) >= 0:
        depth = 1
        for end in range(start + 2, len(body)):
            depth += (body[end] == "[") - (body[end] == "]")
            if depth == 0:
                shape_parts.append(body[start + 2:end])
                cursor = end + 1
                break
        else:
            raise ValueError("behavior union: unclosed retained shape")
    shapes = []
    for part in shape_parts:
        boxes = []
        for lower, upper in re.findall(r"Aabb\s*\{\s*min:\s*\[([^]]*)\],\s*max:\s*\[([^]]*)\],?\s*\}", part):
            box = tuple(map(float, (lower + "," + upper).split(",")))
            require(len(box) == 6 and all(math.isfinite(value) for value in box), "retained collision box")
            boxes.append(box)
        require(part.strip() == "" or boxes, "unparsed retained collision shape")
        shapes.append(tuple(boxes))
    ids = ast.literal_eval("[" + rust_array_body(text, "STATE_SHAPE") + "]")
    require(len(ids) >= len(source["block_states"]), "retained collision prefix incomplete")
    require(all(type(raw) is int and 0 <= raw < len(shapes) for raw in ids), "retained collision range")
    require(len(ids) == len(source["block_states"]), "freeze requires an unextended 26.2 collision source")
    return {"schema_version": 1, "authority": "retained-26.2-production-table",
            "source_sha256": digest_bytes(text.encode()), "state_keys_sha256": identities.digest(source["block_states"]),
            "shapes": shapes, "state_shape": ids}


def retained_collision(inputs):
    snapshot, digest = census.read_report(COLLISION_PREFIX)
    inputs.provenance[str(COLLISION_PREFIX.relative_to(ROOT))] = digest
    require(snapshot["schema_version"] == 1 and snapshot["authority"] == "retained-26.2-production-table",
            "retained collision snapshot schema/authority")
    source = inputs.sources[BASE]
    require(snapshot["state_keys_sha256"] == identities.digest(source["block_states"]), "retained collision identities changed")
    require(len(snapshot["source_sha256"]) == 64, "retained collision source digest")
    shapes = [tuple(tuple(box) for box in shape) for shape in snapshot["shapes"]]
    ids = snapshot["state_shape"]
    require(len(ids) == len(source["block_states"]) and all(type(raw) is int and 0 <= raw < len(shapes) for raw in ids),
            "retained collision snapshot census/range")
    result = dict(zip(source["block_states"], [shapes[raw] for raw in ids]))
    inputs.provenance["retained-26.2-collision-source"] = snapshot["source_sha256"]
    inputs.add("collision", BASE, result)


def load_base(inputs):
    source = inputs.sources[BASE]
    retained_collision(inputs)
    ranges, digest = census.read_report(SOUND_RANGES_PREFIX)
    require(ranges["schema_version"] == 1 and ranges["authority"] == "retained-26.2-production-column"
            and ranges["column"] == "SOUND_EVENT_FIXED_RANGES" and ranges["sound_event_count"] == 1968,
            "retained sound range schema/authority")
    inputs.sound_ranges = [tuple(row) for row in ranges["entries"]]
    require(all(type(raw) is int and 0 <= raw < ranges["sound_event_count"] and math.isfinite(value)
                for raw, value in inputs.sound_ranges)
            and len({raw for raw, _ in inputs.sound_ranges}) == len(inputs.sound_ranges),
            "invalid retained sound ranges")
    inputs.provenance[str(SOUND_RANGES_PREFIX.relative_to(ROOT))] = digest
    outlines = outline_columns(inputs.read(SUPPORT / "outline_shape_jvm.txt"), source)
    inputs.add("outline", BASE, outlines["O"])
    inputs.add("interaction", BASE, outlines["X"])
    inputs.add("sound", BASE, sound_columns(inputs.read(SUPPORT / "sound_types_jvm.txt"), source))
    for filename, labels in (("face_occlusion_jvm.txt", "DUNSWE"), ("snow_support_jvm.txt", "ULWYD"),
                             ("shade_brightness_jvm.txt", "SF"), ("block_physics_jvm.txt", "LM")):
        values = compact_flags(inputs.read(SUPPORT / filename), source, labels)
        if filename.startswith("face_"):
            inputs.add("face_occlusion", BASE, {key: sum(values[label][key] << index for index, label in enumerate(labels))
                                                 for key in source["block_states"]})
        else:
            names = {"snow_support_jvm.txt": ("snow_up", "fluid", "water_source", "snowy", "default"),
                     "shade_brightness_jvm.txt": ("shade", "full_collision"),
                     "block_physics_jvm.txt": ("legacy_solid", "legacy_motion")}[filename]
            for label, name in zip(labels, names):
                inputs.add(name, BASE, values[label])
    for filename, column, offset, parse in (
        ("hardness_jvm.txt", "hardness", 0, lambda row: (bits(row[2]), flag(row[3]))),
        ("block_entity_types_jvm.txt", "entity", 0, lambda row: None if row[3] == "-" else row[3]),
        ("block_survival_jvm.txt", "survival", 1, lambda row: tuple(map(flag, row[2:]))),
    ):
        values = {}
        for row in inputs.read(SUPPORT / filename):
            if row[0] == "C":
                continue
            raw = int(row[offset])
            name = source["block_states"][raw].split("[", 1)[0] if column == "survival" else row[offset + 1]
            key = state_identity(source, raw, name)
            insert(values, key, parse(row), column)
        inputs.add(column, BASE, values)
    paths = {}
    for row in inputs.read(ROOT / "crates/lodestone-data/oracle-java/pathtype_java.txt"):
        key = state_identity(source, int(row[0]), row[1])
        insert(paths, key, row[2], "path")
    inputs.add("path", BASE, paths)
    blast = {}
    for row in inputs.read(SUPPORT / "blast_fire_jvm.txt"):
        key = named_identity(source, "blocks", int(row[0]), row[1])
        insert(blast, key, (bits(row[2]), int(row[3]), int(row[4]), flag(row[5])), "blast")
    inputs.add("blast", BASE, blast, "blocks")
    physics = {}
    for row in inputs.read(SUPPORT / "block_physics_jvm.txt"):
        if row[0] == "K":
            insert(physics, row[1], (*map(bits, row[2:6]), *map(flag, row[6:8])), "movement")
    inputs.add("movement", BASE, physics, "blocks")
    load_item_rows(inputs, inputs.read(SUPPORT / "item_prototype_jvm.txt"), BASE)
    item_blocks = {}
    for row in inputs.read(SUPPORT / "block_items_jvm.txt"):
        key = named_identity(source, "items", int(row[0]), row[1])
        insert(item_blocks, key, None if row[2] == "-" else row[2], "block item")
    inputs.add("block_item", BASE, item_blocks, "items")
    load_tools(inputs, inputs.read(SUPPORT / "tool_jvm.txt"), BASE)


def load_item_rows(inputs, records, version):
    values = {}
    for row in records:
        if row[0] != "P":
            continue
        key = named_identity(inputs.sources[version], "items", int(row[1]), row[2])
        require(len(row) in (8, 9), "prototype columns")
        value = (int(row[3]), None if row[4] == "-" else int(row[4]), flag(row[5]),
                 None if row[6] == "-" else row[6], row[7] == "-")
        require(1 <= value[0] <= 99 and (value[1] is None or 0 < value[1] <= 65535), "prototype widths")
        require(value[3] in (None, "mainhand", "offhand", "feet", "legs", "chest", "head", "body", "saddle"),
                f"unknown equipment slot: {value[3]}")
        if len(row) == 9:
            flag(row[8])
        insert(values, key, value, "prototype")
    inputs.add("prototype", version, values, "items")


def block_members(inputs, value, version):
    result = []
    for token in value.split(",") if value else []:
        if version == LATEST:
            raw, _, name = token.partition(":")
            named_identity(inputs.sources[version], "blocks", int(raw), name)
        else:
            name = token
        require(name in inputs.sources[version]["blocks"], f"unknown tool member {name}")
        result.append(name)
    require(len(result) == len(set(result)), "duplicate tool block member")
    return tuple(sorted(result))


def load_tools(inputs, records, version):
    tags, tools = {}, {}
    current = None
    for row in records:
        if row[0] == "T" and (version == BASE or row[1] == "block"):
            name = row[1] if version == BASE else row[2]
            members = row[2:] if version == BASE else row[4:]
            insert(tags, name, block_members(inputs, ",".join(members), version), "tool tag")
        elif row[0] == "I":
            offset = 1 if version == BASE else 2
            current = row[offset]
            require(current in inputs.sources[version]["items"], "tool item identity")
            header = (bits(row[offset + 1]), int(row[offset + 2]), flag(row[offset + 3]))
            insert(tools, current, [header, [], int(row[offset + 4])], "tool")
        elif row[0] == "R":
            offset = 1 if version == BASE else 4
            if version == LATEST:
                require(row[2] == current and int(row[3]) == len(tools[current][1]), "tool rule owner/order")
            selector, speed, correct = row[offset:offset + 3]
            require(selector.startswith(("#", "=")), "tool rule selector")
            selected = selector if selector.startswith("#") else block_members(inputs, selector[1:], version)
            tools[current][1].append((selected, None if speed == "-" else bits(speed),
                                     None if correct == "-" else flag(correct)))
    for name, (_, rules, count) in tools.items():
        require(len(rules) == count, f"tool rule count: {name}")
        require(all(not isinstance(selector, str) or selector[1:] in tags for selector, _, _ in rules),
                f"unbound tool tag: {name}")
    inputs.tags[version] = tags
    inputs.tools[version] = {name: (header, tuple(rules)) for name, (header, rules, _) in tools.items()}


def load_latest(inputs):
    source = inputs.sources[LATEST]
    directory = CAPTURES / "collision-block-entity.r3Qp07"
    inputs.authenticate(directory, ["collision-block-entity.raw"])
    shapes, collision, entity = [], {}, {}
    records = inputs.read(directory / "collision-block-entity.raw")
    require(records[0][:3] == ["C", str(len(source["block_states"])), str(len(source["blocks"]))]
            and records[-1] == ["Z", *records[0][1:]], "collision wrong-release or incomplete trailer")
    for row in records:
        if row[0] == "Q":
            require(int(row[1]) == len(shapes), "collision shape order")
            shapes.append(double_boxes(row[3:], int(row[2]), lossless=True))
        elif row[0] == "S":
            key = state_identity(source, int(row[1]), row[2], row[3])
            require(len(row) == 9 and 0 <= int(row[5]) < len(shapes), "collision columns/range")
            insert(collision, key, shapes[int(row[5])], "collision state")
            insert(entity, key, None if row[8] == "-" else row[8], "entity state")
    inputs.add("collision", LATEST, collision)
    inputs.add("entity", LATEST, entity)
    directory = CAPTURES / "capture-batch2.yC9KcR"
    inputs.authenticate(directory, ["OutlineShapeOracle.raw", "SoundTypeOracle.raw"])
    outlines = outline_columns(inputs.read(directory / "OutlineShapeOracle.raw"), source)
    inputs.add("outline", LATEST, outlines["O"])
    inputs.add("interaction", LATEST, outlines["X"])
    inputs.add("sound", LATEST, sound_columns(inputs.read(directory / "SoundTypeOracle.raw"), source))
    directory = CAPTURES / "capture.jWRsrL"
    inputs.authenticate(directory, ["FaceOcclusionOracle.raw", "SnowSupportOracle.raw", "ShadeBrightnessOracle.raw"])
    for filename, labels, names in (
        ("FaceOcclusionOracle.raw", "DUNSWE", ()),
        ("SnowSupportOracle.raw", "ULWYD", ("snow_up", "fluid", "water_source", "snowy", "default")),
        ("ShadeBrightnessOracle.raw", "SF", ("shade", "full_collision")),
    ):
        values = compact_flags(inputs.read(directory / filename), source, labels)
        if not names:
            inputs.add("face_occlusion", LATEST, {key: sum(values[label][key] << index for index, label in enumerate(labels))
                                                   for key in source["block_states"]})
        for label, name in zip(labels, names):
            inputs.add(name, LATEST, values[label])
    directory = CAPTURES / "physics-semantics.nY3xZV"
    inputs.authenticate(directory, ["physics-semantics.raw"])
    names = ("legacy_solid", "geometry_solid", "generic_motion", "heightmap_tag", "no_leaves_tag", "fluid_blocker",
             "ocean_floor", "motion_heightmap", "no_leaves_heightmap", "fluid")
    columns, movement = {name: {} for name in names}, {}
    records = inputs.read(directory / "physics-semantics.raw")
    require(records[0] == ["C", str(len(source["block_states"])), str(len(source["blocks"]))]
            and records[-1] == ["Z", *records[0][1:]], "physics wrong-release or incomplete trailer")
    for row in records:
        if row[0] in ("K", "S"):
            key = state_identity(source, int(row[1]), row[2], row[3])
            if row[0] == "K":
                insert(movement, row[2], (*map(bits, row[4:8]), *map(flag, row[8:10])), "movement")
            else:
                require(len(row) == 14, "physics state columns")
                for name, value in zip(names, row[4:]):
                    insert(columns[name], key, flag(value), name)
    inputs.add("movement", LATEST, movement, "blocks")
    for name, values in columns.items():
        if name == "fluid":
            require(values == inputs.columns[name]["versions"][LATEST], "independent fluid captures disagree")
        inputs.add(name, LATEST, values)
    directory = CAPTURES / "survival-blast-path.bfFtUe"
    inputs.authenticate(directory, ["survival-blast-path.raw"])
    blast, survival, paths = {}, {}, {}
    records = inputs.read(directory / "survival-blast-path.raw")
    require(records[0][:3] == ["C", str(len(source["block_states"])), str(len(source["blocks"]))]
            and records[-1] == ["Z", *records[0][1:]], "survival wrong-release or incomplete trailer")
    for row in records:
        if row[0] == "B":
            key = named_identity(source, "blocks", int(row[1]), row[3])
            state_identity(source, int(row[2]), row[3], row[4])
            insert(blast, key, (bits(row[5]), int(row[6]), int(row[7]), flag(row[8])), "blast")
        elif row[0] == "S":
            key = state_identity(source, int(row[1]), row[2], row[3])
            require(len(row) == 12, "survival/path columns")
            insert(survival, key, tuple(map(flag, row[4:8])), "survival")
            insert(paths, key, row[11], "path")
    inputs.add("blast", LATEST, blast, "blocks")
    inputs.add("survival", LATEST, survival)
    inputs.add("path", LATEST, paths)
    directory = CAPTURES / "item-tools.nGvP0t"
    inputs.authenticate(directory, ["item-tools.from-reports.tsv"], "from-reports.manifest.json")
    records = inputs.read(directory / "item-tools.from-reports.tsv")
    load_item_rows(inputs, records, LATEST)
    load_tools(inputs, records, LATEST)
    for version in census.VERSIONS:
        values = {}
        for row in inputs.read(SCALARS / f"{version}-light-properties.raw"):
            if row[0] in ("C", "E"):
                continue
            key = state_identity(inputs.sources[version], int(row[0]), row[1], row[4])
            value = tuple(map(int, row[2:4]))
            require(all(0 <= raw <= 15 for raw in value), "light level domain")
            insert(values, key, value, "light")
        inputs.add("light", version, values)
    for filename, column, domain, parse in (
        ("26.3-hardness.raw", "hardness", "block_states", lambda row: (bits(row[2]), flag(row[3]))),
        ("26.3-block-item.raw", "block_item", "items", lambda row: None if row[2] == "-" else row[2]),
    ):
        values = {}
        for row in inputs.read(SCALARS / filename):
            key = (state_identity(source, int(row[0]), row[1]) if domain == "block_states"
                   else named_identity(source, domain, int(row[0]), row[1]))
            insert(values, key, parse(row), column)
        inputs.add(column, LATEST, values, domain)


def load_inputs(base_reports=None, latest_reports=None):
    sources = {BASE: census.load_source(base_reports or ROOT / ".cache/mc/26.2/generated/reports"),
               LATEST: census.load_source(latest_reports or ROOT / ".cache/mc/26.3/generated/reports")}
    manifest = census.build_manifest(sources)
    bundle = identities.build_bundle(manifest, sources, "union")
    inputs = Inputs(sources, bundle, manifest)
    load_base(inputs)
    load_latest(inputs)
    return inputs


def primary_and_overrides(inputs, name):
    column = inputs.columns[name]
    keys = inputs.bundle["domains"][column["domain"]]["keys"]
    versions = column["versions"]
    require(BASE in versions and LATEST in versions, f"{name} requires both release inputs")
    old, latest = versions[BASE], versions[LATEST]
    for version, values in ((BASE, old), (LATEST, latest)):
        total(values, inputs.sources[version][column["domain"]], f"{name}/{version}")
    primary = [old[key] if key in old else latest[key] for key in keys]
    overrides = [(raw, latest[key]) for raw, key in enumerate(keys) if key in old and old[key] != latest[key]]
    return primary, overrides


def census_summary(inputs):
    result = {}
    for name, column in sorted(inputs.columns.items()):
        versions = column["versions"]
        shared = set(versions.get(BASE, {})) & set(versions.get(LATEST, {}))
        differences = [key for key in sorted(shared) if versions[BASE][key] != versions[LATEST][key]]
        result[name] = {"domain": column["domain"], "counts": {version: len(values) for version, values in versions.items()},
                        "shared_changes": len(differences), "witnesses": differences[:6]}
    return result


def check_runtime(files):
    for name, content in files.items():
        require((GENERATED / name).read_bytes() == content.encode(), f"runtime file differs: {name}")


def install_runtime(files):
    prepared = []
    try:
        for name, content in files.items():
            require(re.fullmatch(r"[a-z0-9_]+\.(rs|json)", name) is not None, "unexpected runtime filename")
            with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=GENERATED,
                                             prefix=".behavior-union-", delete=False) as output:
                output.write(content)
                prepared.append((Path(output.name), GENERATED / name))
        for temporary, destination in prepared:
            temporary.replace(destination)
    finally:
        for temporary, _ in prepared:
            temporary.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-reports", type=Path)
    parser.add_argument("--latest-reports", type=Path)
    parser.add_argument("--freeze-base-collision", action="store_true",
                        help="freeze the existing unextended collision table once as an immutable input")
    destination = parser.add_mutually_exclusive_group()
    destination.add_argument("--runtime-check", action="store_true", help="check all adopted runtime outputs without writing")
    destination.add_argument("--runtime-install", action="store_true", help="install the complete validated runtime group")
    args = parser.parse_args()
    if args.freeze_base_collision:
        source = census.load_source(args.base_reports or ROOT / ".cache/mc/26.2/generated/reports")
        snapshot = collision_snapshot((GENERATED / "collision_shapes.rs").read_text(), source)
        with COLLISION_PREFIX.open("x", encoding="utf-8") as output:
            output.write(census.serialize(snapshot))
        print(f"retained collision prefix: {len(snapshot['state_shape'])}; source_sha256: {snapshot['source_sha256']}")
        return
    inputs = load_inputs(args.base_reports, args.latest_reports)
    if args.runtime_check or args.runtime_install:
        import behavior_rust
        files = behavior_rust.build_rust_files(inputs)
        if args.runtime_check:
            check_runtime(files)
        else:
            install_runtime(files)
        print(f"runtime files: {len(files)}; bytes: {sum(len(content.encode()) for content in files.values())}")
    print(json.dumps(census_summary(inputs), sort_keys=True, indent=2))


if __name__ == "__main__":
    main()
