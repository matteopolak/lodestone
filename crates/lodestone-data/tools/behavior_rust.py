"""Numeric Rust representation for the validated semantic behavior union."""

import json

import behavior_union as behavior
import canonical_census as census
import identity_staging as identities


def boolean(value):
    return "true" if value else "false"


def f32(raw):
    return f"f32::from_bits(0x{raw:08x})"


def option(value, render=str):
    return "None" if value is None else f"Some({render(value)})"


def array(name, kind, values, render=str, width=16):
    return identities.rust_array(name, kind, [str(render(value)) for value in values], width)


def packed(values):
    result = [0] * ((len(values) + 7) // 8)
    for raw, value in enumerate(values):
        if value:
            result[raw // 8] |= 1 << (raw % 8)
    return result


def bitset(name, values):
    return array(name, "u8", packed(values), lambda value: f"0x{value:02x}")


def intern(primary, overrides):
    entries, indices = [], {}

    def lookup(value):
        if value not in indices:
            indices[value] = len(entries)
            entries.append(value)
        return indices[value]

    main = [lookup(value) for value in primary]
    changed = [(raw, lookup(value)) for raw, value in overrides]
    behavior.require(len(entries) <= 65536, "interned behavior table exceeds u16")
    return entries, main, changed


def shape(value, kind):
    boxes = [f"{kind} {{ min: [{', '.join(repr(raw) for raw in box[:3])}], "
             f"max: [{', '.join(repr(raw) for raw in box[3:])}] }}" for box in value]
    return "&[" + ", ".join(boxes) + "]"


def prototype(value):
    stack, damage, has_damage, slot, any_entity = value
    slots = {"mainhand": "MainHand", "offhand": "OffHand", "feet": "Feet", "legs": "Legs",
             "chest": "Chest", "head": "Head", "body": "Body", "saddle": "Saddle"}
    return (f"ItemPrototypeDef {{ max_stack_size: {stack}, max_damage: {option(damage)}, "
            f"equip_slot: {option(slot, lambda value: 'EquipmentSlot::' + slots[value])}, "
            f"has_damage: {boolean(has_damage)}, equippable_by_any_entity: {boolean(any_entity)} }}")


def property_variants(names, prefix):
    result, seen = [], set()
    for name in names:
        base = (prefix if name[0].isdigit() else "") + "".join(word[:1].upper() + word[1:] for word in name.split("_"))
        variant, suffix = base, 2
        while variant in seen:
            variant, suffix = base + str(suffix), suffix + 1
        seen.add(variant)
        result.append(variant)
    return result


def property_enum(name, names, prefix, constant):
    variants = property_variants(names, prefix)
    lines = ["#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]", "#[repr(u8)]", f"pub enum {name} {{"]
    lines.extend(f"    {variant} = {raw}," for raw, variant in enumerate(variants))
    lines.extend(["}", "", f"pub const {constant}_COUNT: usize = {len(names)};", "",
                  array(constant + "S", name, variants, lambda value: f"{name}::{value}", 4), f"impl {name} {{",
                  "    #[must_use]", "    pub const fn name(self) -> &'static str {", "        match self {"])
    lines.extend(f"            Self::{variant} => {json.dumps(value)}," for value, variant in zip(names, variants))
    lines.extend(["        }", "    }", "", "    #[must_use]", "    pub fn from_name(name: &str) -> Option<Self> {", "        match name {"])
    lines.extend(f"            {json.dumps(value)} => Some(Self::{variant})," for value, variant in zip(names, variants))
    lines.extend(["            _ => None,", "        }", "    }", "", "    #[must_use]", "    pub const fn from_id(id: u8) -> Option<Self> {", "        match id {"])
    lines.extend(f"            {raw} => Some(Self::{variant})," for raw, variant in enumerate(variants))
    lines.extend(["            _ => None,", "        }", "    }", "", "    pub fn all() -> impl ExactSizeIterator<Item = Self> + Clone {",
                  f"        {constant}S.iter().copied()", "    }", "}", ""])
    return "\n".join(lines)


def typed_properties(inputs):
    old = [identities.state_properties(key) for key in inputs.sources[behavior.BASE]["block_states"]]
    all_rows = [identities.state_properties(key) for key in inputs.bundle["domains"]["block_states"]["keys"]]

    def domain(index):
        base = sorted({pair[index] for row in old for pair in row})
        return base + sorted({pair[index] for row in all_rows for pair in row} - set(base))

    keys, values = domain(0), domain(1)
    behavior.require(len(keys) <= 256 and len(values) <= 256, "typed property domain exceeds u8")
    key_ids, value_ids = ({value: raw for raw, value in enumerate(names)} for names in (keys, values))
    rows = [tuple(sorted((key_ids[key], value_ids[value]) for key, value in row)) for row in all_rows]
    sets = sorted(set(rows))
    behavior.require(len(sets) <= 65536, "typed property sets exceed u16")
    set_ids = {value: raw for raw, value in enumerate(sets)}
    pairs = sorted({pair for row in rows for pair in row})
    return "\n".join([
        f"pub const MAX_PROPERTIES: usize = {max(map(len, rows))};\n",
        property_enum("PropertyKey", keys, "Key", "PROPERTY_KEY"),
        property_enum("BuiltinPropertyValue", values, "Value", "PROPERTY_VALUE"),
        array("VALID_PAIRS", "(u8, u8)", pairs, lambda value: repr(value), 8),
        "pub(super) fn is_valid_pair(key: PropertyKey, value: BuiltinPropertyValue) -> bool {\n"
        "    VALID_PAIRS.binary_search(&(key as u8, value as u8)).is_ok()\n}\n",
        array("PROPERTY_SETS", "&[(u8, u8)]", sets, lambda value: "&[" + ", ".join(map(repr, value)) + "]", 1),
        array("STATE_PROPERTY_SET_IDS", "u16", [set_ids[row] for row in rows]),
        "pub(super) fn property_set_for_state(raw: u32) -> &'static [(u8, u8)] {\n"
        "    PROPERTY_SETS[STATE_PROPERTY_SET_IDS[raw as usize] as usize]\n}\n",
    ])


def tool_tables(inputs, version):
    block_ids = {name: raw for raw, name in enumerate(inputs.bundle["domains"]["blocks"]["keys"])}
    item_ids = {name: raw for raw, name in enumerate(inputs.bundle["domains"]["items"]["keys"])}
    tags, tools = inputs.tags[version], inputs.tools[version]

    def rule(value):
        selector, speed, correct = value
        blocks = ("ToolBlocksDef::Tag(" + json.dumps(selector[1:]) + ")" if isinstance(selector, str)
                  else "ToolBlocksDef::Blocks(&[" + ", ".join(str(raw) for raw in sorted(block_ids[name] for name in selector)) + "])")
        return f"ToolRuleDef {{ blocks: {blocks}, speed: {option(speed, f32)}, correct_for_drops: {option(correct, boolean)} }}"

    def tool(name):
        (speed, damage, creative), rules = tools[name]
        return (f"({item_ids[name]}, ToolDef {{ default_mining_speed: {f32(speed)}, damage_per_block: {damage}, "
                f"can_destroy_blocks_in_creative: {boolean(creative)}, rules: &[" + ", ".join(map(rule, rules)) + "] })")

    return "\n".join([
        "use crate::tool::{ToolBlocksDef, ToolDef, ToolRuleDef};\n",
        f"pub const BLOCK_TAG_COUNT: usize = {len(tags)};\n",
        f"pub const ITEM_TOOL_COUNT: usize = {len(tools)};\n",
        array("BLOCK_TAGS", "(&str, &[u16])", sorted(tags), lambda name:
              "(" + json.dumps(name) + ", &[" + ", ".join(str(raw) for raw in sorted(block_ids[key] for key in tags[name])) + "])", 1),
        array("ITEM_TOOLS", "(u16, ToolDef)", sorted(tools, key=item_ids.__getitem__), tool, 1),
    ])


def build_rust_files(inputs):
    identities.validate_bundle(inputs.bundle, inputs.manifest, inputs.sources)
    files = identities.build_rust_files(inputs.bundle, inputs.manifest, inputs.sources)
    files.pop("manifest.json")
    source_digest = identities.digest(inputs.provenance)
    header = ("// @generated by crates/lodestone-data/tools/behavior_union.py.\n"
              f"// Identity bundle: {identities.digest(inputs.bundle)}; behavior sources: {source_digest}.\n"
              "// Existing identities retain 26.2 values; shared 26.3 changes are explicit overrides.\n\n")
    count = len(inputs.bundle["domains"]["block_states"]["keys"])
    state_count = f"pub const STATE_COUNT: u32 = {count};\n"
    versioned = []

    def override(name, kind, values, render):
        if not values:
            return
        versioned.append(array(name.upper(), f"(u32, {kind})", values,
                               lambda pair: f"({pair[0]}, {render(pair[1])})", 1))

    def indexed(name, filename, entries_name, index_name, entry_type, render, extra="", index_type="u16"):
        primary, changes = behavior.primary_and_overrides(inputs, name)
        entries, ids, changed = intern(primary, changes)
        if index_type == "u8":
            behavior.require(len(entries) <= 256, f"{name} entry index exceeds u8")
        files[filename] = header + extra + state_count + "\n" + array(entries_name, entry_type, entries, render, 1) + "\n" + array(index_name, index_type, ids)
        override(name, index_type, changed, str)
        return entries

    indexed("collision", "collision_shapes.rs", "SHAPES", "STATE_SHAPE", "&[Aabb]", lambda value: shape(value, "Aabb"),
            "use crate::collision_shapes::Aabb;\n\n")
    for name, shapes_name, indices_name in (("outline", "OUTLINE_SHAPES", "STATE_OUTLINE"),
                                             ("interaction", "INTERACTION_SHAPES", "STATE_INTERACTION")):
        primary, changes = behavior.primary_and_overrides(inputs, name)
        entries, ids, changed = intern(primary, changes)
        if name == "outline":
            files["outline_shapes.rs"] = header + "use lodestone_model::BlockAabb;\n\n" + state_count + "\n"
        files["outline_shapes.rs"] += array(shapes_name, "&[BlockAabb]", entries, lambda value: shape(value, "BlockAabb"), 1) + "\n" + array(indices_name, "u16", ids) + "\n"
        override(name, "u16", changed, str)
    indexed("light", "light_props.rs", "ENTRIES", "STATE_ENTRY", "(u8, u8)", repr, index_type="u8")
    indexed("hardness", "hardness.rs", "ENTRIES", "STATE_ENTRY", "(f32, bool)", lambda value: f"({f32(value[0])}, {boolean(value[1])})")
    for name, filename, column in (("face_occlusion", "face_occlusion.rs", "FACE_OCCLUSION"),
                                   ("shade", "shade_brightness.rs", "SHADE_OCCLUDES")):
        primary, changes = behavior.primary_and_overrides(inputs, name)
        files[filename] = header + state_count + "\n" + (bitset(column, primary) if name == "shade" else array(column, "u8", primary))
        override(name, "bool" if name == "shade" else "u8", changes, boolean if name == "shade" else str)
    files["snow_support.rs"] = header + state_count + "\n"
    for name, column in (("snow_up", "FACE_FULL_UP"), ("fluid", "HAS_FLUID_STATE"),
                         ("water_source", "IS_WATER_SOURCE_LIQUID_BLOCK"), ("snowy", "HAS_SNOWY_PROPERTY")):
        primary, changes = behavior.primary_and_overrides(inputs, name)
        files["snow_support.rs"] += bitset(column, primary) + "\n"
        override(name, "bool", changes, boolean)
    primary, changes = behavior.primary_and_overrides(inputs, "legacy_solid")
    old_motion = inputs.columns["legacy_motion"]["versions"][behavior.BASE]
    files["block_solidity.rs"] = (header + state_count + f"pub const LEGACY_MOTION_STATE_COUNT: u32 = {len(old_motion)};\n\n"
                                  + bitset("LEGACY_SOLID", primary) + "\n"
                                  + bitset("BLOCKS_MOTION", [old_motion[key] for key in inputs.sources[behavior.BASE]["block_states"]]))
    override("legacy_solid", "bool", changes, boolean)
    for name in ("generic_motion", "fluid_blocker", "ocean_floor", "motion_heightmap", "no_leaves_heightmap"):
        values = inputs.columns[name]["versions"][behavior.LATEST]
        versioned.append(bitset(name.upper(), [values[key] for key in inputs.bundle["domains"]["block_states"]["keys"]]))
    primary, changes = behavior.primary_and_overrides(inputs, "survival")
    files["block_survival.rs"] = header + state_count + "\n"
    for index, name in enumerate(("SOLID_RENDER", "STURDY_UP", "CENTER_SUPPORT_DOWN", "FIRE_FLAMMABLE")):
        files["block_survival.rs"] += bitset(name, [value[index] for value in primary]) + "\n"
    override("survival", "u8", [(raw, sum(flag << index for index, flag in enumerate(value))) for raw, value in changes], str)
    entity_names = census.read_report(behavior.ROOT / ".cache/mc/26.2/generated/reports/registries.json")[0]["minecraft:block_entity_type"]["entries"]
    entity_names = census.ordered_keys(((key, value["protocol_id"]) for key, value in entity_names.items()), "block entity types")
    entity_ids = {name: raw for raw, name in enumerate(entity_names)}
    primary, changes = behavior.primary_and_overrides(inputs, "entity")
    render_entity = lambda name: 65535 if name is None else entity_ids[name]
    files["block_entity_types.rs"] = (header + state_count + f"pub const TYPE_COUNT: u32 = {len(entity_names)};\n\n"
                                      + array("TYPE_NAMES", "&str", entity_names, json.dumps, 1) + "\n"
                                      + array("STATE_TYPE", "u16", primary, render_entity))
    override("entity", "u16", changes, lambda value: str(render_entity(value)))
    primary, changes = behavior.primary_and_overrides(inputs, "path")
    path = lambda value: "PathType::" + "".join(word.title() for word in value.split("_"))
    shard_names = []
    for start in range(0, count, 1024):
        name = f"path_types_{start // 1024:04}.rs"
        shard_names.append((name, start))
        files[name] = "[\n" + "\n".join("    " + ", ".join(map(path, primary[index:index + 8])) + ","
                                                   for index in range(start, min(start + 1024, count), 8)) + "\n]\n"
    files["path_types.rs"] = (header + "use lodestone_model::PathType;\n\n" + state_count + "\n"
                              + f"const fn copy_shard<const N: usize>(mut out: [PathType; {count}], shard: [PathType; N], offset: usize) -> [PathType; {count}] {{\n"
                              + "    let mut index = 0;\n    while index < N {\n        out[offset + index] = shard[index];\n        index += 1;\n    }\n    out\n}\n\n"
                              + f"const fn assemble() -> [PathType; {count}] {{\n    let mut out = [PathType::Open; {count}];\n"
                              + "\n".join(f'    out = copy_shard(out, include!("{name}"), {start});' for name, start in shard_names)
                              + "\n    out\n}\n\n" + f"pub static STATE_PATH_TYPE: [PathType; {count}] = assemble();\n")
    override("path", "PathType", changes, path)
    block_ids = {name: raw for raw, name in enumerate(inputs.bundle["domains"]["blocks"]["keys"])}
    variants = identities.enum_variants(inputs.bundle["domains"]["blocks"]["keys"])
    primary, changes = behavior.primary_and_overrides(inputs, "block_item")
    block = lambda value: "None" if value is None else f"Some(Block::{variants[block_ids[value]]})"
    item_count = f"pub const ITEM_COUNT: u32 = {len(primary)};\n"
    files["block_items.rs"] = header + "use crate::block::Block;\n\n" + item_count + "\n" + array("BLOCK_FOR_ITEM", "Option<Block>", primary, block, 4)
    override("block_item", "Option<u16>", changes, lambda name: option(None if name is None else block_ids[name]))
    primary, changes = behavior.primary_and_overrides(inputs, "prototype")
    files["item_prototypes.rs"] = header + "use lodestone_model::EquipmentSlot;\nuse crate::item_prototypes::ItemPrototypeDef;\n\n" + item_count + "\n" + array("ITEM_PROTOTYPES", "ItemPrototypeDef", primary, prototype, 1)
    override("prototype", "ItemPrototypeDef", changes, prototype)
    files["tools_26_3.rs"] = header + tool_tables(inputs, behavior.LATEST)
    files["tools.rs"] = header + tool_tables(inputs, behavior.BASE)
    primary, changes = behavior.primary_and_overrides(inputs, "movement")
    movement = lambda value: "(" + ", ".join([*(f32(raw) for raw in value[:4]), *(boolean(raw) for raw in value[4:])]) + ")"
    files["block_movement.rs"] = header + array("MOVEMENT", "(f32, f32, f32, f32, bool, bool)", primary, movement, 1)
    override("movement", "(f32, f32, f32, f32, bool, bool)", changes, movement)
    primary, changes = behavior.primary_and_overrides(inputs, "blast")
    entries, ids, changed = intern(primary, changes)
    blast = lambda value: f"(0x{value[0]:08x}, {value[1]}, {value[2]}, {boolean(value[3])})"
    files["block_blast.rs"] = header + f"pub const BLOCK_COUNT: u32 = {len(primary)};\n\n" + array("ENTRIES", "(u32, u8, u8, bool)", entries, blast, 1) + "\n" + array("ENTRY_BY_REGISTRY_ID", "u16", ids) + "\n" + state_count + "pub const EMPTY_RESISTANCE: u32 = 0xffffffff;\n\n"
    override("blast", "u16", changed, str)
    effective = {}
    for version in behavior.census.VERSIONS:
        values = {}
        blocks = inputs.columns["blast"]["versions"][version]
        fluid = inputs.columns["fluid"]["versions"][version]
        for key in inputs.sources[version]["block_states"]:
            name = key.split("[", 1)[0]
            raw = blocks[name][0]
            if name in ("minecraft:air", "minecraft:cave_air", "minecraft:void_air") and not fluid[key]:
                raw = 0xffffffff
            elif fluid[key]:
                value = behavior.struct.unpack(">f", raw.to_bytes(4, "big"))[0]
                raw = max(raw, 0x42c80000) if value >= 0 else 0x42c80000
            values[key] = raw
        effective[version] = values
    inputs.columns["effective_resistance"] = {"domain": "block_states", "versions": effective}
    primary, changes = behavior.primary_and_overrides(inputs, "effective_resistance")
    entries, ids, changed = intern(primary, changes)
    files["block_blast.rs"] += array("RESISTANCE_VALUES", "u32", entries, lambda value: f"0x{value:08x}") + "\n" + array("STATE_RESISTANCE_ENTRY", "u16", ids)
    registries = {version: census.read_report(behavior.ROOT / f".cache/mc/{version}/generated/reports/registries.json")[0]
                  for version in behavior.census.VERSIONS}
    sound_names = census.registry_union_names(registries, "minecraft:sound_event")
    sound_ids = {name: raw for raw, name in enumerate(sound_names)}
    ranges = inputs.sound_ranges
    files["sound_events.rs"] = header + f"pub const SOUND_EVENT_COUNT: u32 = {len(sound_names)};\n\n" + array("SOUND_EVENT_FIXED_RANGES", "(u32, f32)", ranges, repr) + "\n" + array("SOUND_EVENT_NAMES", "&str", sound_names, json.dumps, 1)
    sound = lambda value: f"({f32(value[0])}, {f32(value[1])}, " + ", ".join(str(sound_ids[name]) for name in value[2:]) + ")"
    entries = indexed("sound", "sound_types.rs", "ENTRIES", "STATE_ENTRY", "(f32, f32, u16, u16, u16, u16, u16)", sound, index_type="u8")
    files["sound_types.rs"] += f"\npub const ENTRY_COUNT: u32 = {len(entries)};\n"
    files["block_property_tables.rs"] = header + typed_properties(inputs)
    files["behavior_versions.rs"] = header + state_count + "\n" + "\n".join(versioned)
    manifest = {"schema_version": 1, "identity_bundle_sha256": identities.digest(inputs.bundle),
                "behavior_source_sha256": inputs.provenance, "columns": behavior.census_summary(inputs),
                "rust_sha256": {name: behavior.digest_bytes(content.encode()) for name, content in sorted(files.items())}}
    files["behavior_manifest.json"] = behavior.census.serialize(manifest)
    return files
