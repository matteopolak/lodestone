"""Independent captured witnesses and exhaustive semantic union controls."""

import ast
import copy
import hashlib
import json
import re
import unittest

import behavior_union as behavior
import behavior_rust as rust
import canonical_census as census


def numeric_column(text, name):
    return ast.literal_eval("[" + behavior.rust_array_body(text, name) + "]")


def read_bit(column, raw):
    return bool(column[raw // 8] & (1 << (raw % 8)))


class JoinControls(unittest.TestCase):
    def setUp(self):
        self.source = {"block_states": ["minecraft:air", "minecraft:oak_log[axis=x]", "minecraft:oak_log[axis=y]"],
                       "blocks": ["minecraft:air", "minecraft:oak_log"]}

    def test_shifted_identity_property_duplicate_and_truncation_controls(self):
        self.assertEqual(behavior.state_identity(self.source, 2, "minecraft:oak_log", "axis=y"), self.source["block_states"][2])
        for raw, name, properties in ((1, "minecraft:oak_log", "axis=y"), (2, "minecraft:air", "axis=y"),
                                       (2, "minecraft:oak_log", "axis=x"), (3, "minecraft:oak_log", "axis=y")):
            with self.subTest(raw=raw, name=name, properties=properties), self.assertRaises(ValueError) as error:
                behavior.state_identity(self.source, raw, name, properties)
            print(f"REJECTED identity control: {error.exception}")
        with self.assertRaisesRegex(ValueError, "duplicate") as error:
            behavior.insert({"minecraft:air": 0}, "minecraft:air", 1, "state")
        print(f"REJECTED duplicate control: {error.exception}")
        with self.assertRaisesRegex(ValueError, "missing 1") as error:
            behavior.total({key: 0 for key in self.source["block_states"][:-1]}, self.source["block_states"], "states")
        print(f"REJECTED truncation control: {error.exception}")

    def test_wrong_release_and_wrong_motion_column_are_rejected(self):
        records = [["C", "3", "2"], ["B", "0", "minecraft:air"], ["B", "1", "minecraft:oak_log"], ["P", "L", "0", "011"]]
        self.assertEqual(list(behavior.compact_flags(records, self.source, "L")["L"].values()), [False, True, True])
        wrong = copy.deepcopy(records)
        wrong[0][1] = "4"
        with self.assertRaisesRegex(ValueError, "wrong-release") as error:
            behavior.compact_flags(wrong, self.source, "L")
        print(f"REJECTED wrong-release control: {error.exception}")
        with self.assertRaisesRegex(ValueError, "incomplete") as error:
            behavior.compact_flags(records, self.source, "M")
        print(f"REJECTED sibling-predicate control: {error.exception}")

    def test_report_registry_union_preserves_order_and_rejects_removed_or_sparse_ids(self):
        registry = "minecraft:sound_event"
        reports = {
            behavior.BASE: {registry: {"entries": {"minecraft:a": {"protocol_id": 0}, "minecraft:b": {"protocol_id": 1}}}},
            behavior.LATEST: {registry: {"entries": {"minecraft:c": {"protocol_id": 0}, "minecraft:b": {"protocol_id": 1}, "minecraft:a": {"protocol_id": 2}}}},
        }
        self.assertEqual(census.registry_union_names(reports, registry), ["minecraft:a", "minecraft:b", "minecraft:c"])
        removed = copy.deepcopy(reports)
        del removed[behavior.LATEST][registry]["entries"]["minecraft:a"]
        with self.assertRaisesRegex(ValueError, "removed") as error:
            census.registry_union_names(removed, registry)
        print(f"REJECTED removed registry-identity control: {error.exception}")
        sparse = copy.deepcopy(reports)
        sparse[behavior.LATEST][registry]["entries"]["minecraft:a"]["protocol_id"] = 3
        with self.assertRaisesRegex(ValueError, "dense") as error:
            census.registry_union_names(sparse, registry)
        print(f"REJECTED sparse registry-identity control: {error.exception}")


class CapturedUnionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.inputs = behavior.load_inputs()
        cls.files = rust.build_rust_files(cls.inputs)

    def test_exact_shared_changes_use_independently_read_values(self):
        blast, changes = behavior.primary_and_overrides(self.inputs, "blast")
        self.assertEqual(len(changes), 3)
        for name in ("minecraft:quartz_slab", "minecraft:red_sandstone_slab", "minecraft:cut_red_sandstone_slab"):
            raw = self.inputs.bundle["domains"]["blocks"]["keys"].index(name)
            self.assertEqual(blast[raw], (0x40c00000, 0, 0, False))
            self.assertEqual(dict(changes)[raw], (0x3f4ccccd, 0, 0, False))
        solid, changes = behavior.primary_and_overrides(self.inputs, "legacy_solid")
        self.assertEqual(changes, [(9015, True), (9018, True)])
        self.assertEqual([solid[raw] for raw in (9015, 9018)], [False, False])
        outline, changes = behavior.primary_and_overrides(self.inputs, "outline")
        self.assertEqual(len(changes), 32)
        self.assertEqual(outline[11131], ((0.25, 0.0, 0.25, 0.75, 0.5, 0.75),))
        self.assertEqual(dict(changes)[11131], ((0.25, 0.0, 0.25, 0.75, 0.53125, 0.75),))
        latest = self.inputs.columns["no_leaves_heightmap"]["versions"][behavior.LATEST]
        key = "minecraft:oak_leaves[distance=1,persistent=true,waterlogged=true]"
        self.assertTrue(latest[key])
        self.assertFalse(self.inputs.columns["no_leaves_tag"]["versions"][behavior.LATEST][key])
        with self.assertRaisesRegex(ValueError, "requires both release inputs") as error:
            behavior.primary_and_overrides(self.inputs, "legacy_motion")
        print(f"REJECTED invented latest legacy-motion control: {error.exception}")

    def test_every_primary_semantic_prefix_is_retained_and_latest_tail_is_total(self):
        for name, column in self.inputs.columns.items():
            if set(column["versions"]) != set(census.VERSIONS):
                continue
            primary, overrides = behavior.primary_and_overrides(self.inputs, name)
            keys = self.inputs.bundle["domains"][column["domain"]]["keys"]
            base_count = len(self.inputs.sources[behavior.BASE][column["domain"]])
            self.assertEqual(primary[:base_count], [column["versions"][behavior.BASE][key] for key in keys[:base_count]], name)
            latest = dict(overrides)
            self.assertEqual([latest.get(raw, primary[raw]) for raw in range(len(keys))],
                             [column["versions"][behavior.LATEST][key] for key in keys], name)

    def test_latest_prototypes_match_authenticated_component_reports_and_literal_new_items(self):
        directory = behavior.ROOT / ".cache/mc/26.3/generated/reports/minecraft/components/item"
        metadata = json.loads((behavior.CAPTURES / "item-tools.nGvP0t/from-reports.manifest.json").read_text())
        hashes = metadata["component_report_file_hashes"]
        latest = self.inputs.columns["prototype"]["versions"][behavior.LATEST]
        self.assertEqual((len(hashes), len(latest)), (1658, 1658))
        for name, captured in latest.items():
            filename = name.removeprefix("minecraft:") + ".json"
            raw = (directory / filename).read_bytes()
            self.assertEqual(hashlib.sha256(raw).hexdigest(), hashes[filename], name)
            components = json.loads(raw)["components"]
            equip = components.get("minecraft:equippable", {})
            expected = (components["minecraft:max_stack_size"], components.get("minecraft:max_damage"),
                        "minecraft:damage" in components, equip.get("slot"), "allowed_entities" not in equip)
            self.assertEqual(captured, expected, name)
        self.assertEqual(latest["minecraft:poplar_boat"], (1, None, False, None, True))
        self.assertEqual(latest["minecraft:poplar_planks"], (64, None, False, None, True))
        old = self.inputs.columns["prototype"]["versions"][behavior.BASE]
        self.assertNotIn("minecraft:poplar_boat", old)
        self.assertEqual(len(old), 1537)
        self.assertEqual(behavior.primary_and_overrides(self.inputs, "prototype")[1], [],
                         "shared prototype changes require release-selected overrides before adoption")

    def test_emitted_bitsets_and_total_numeric_columns_match_capture_values(self):
        files = self.files
        count = 35723
        for filename, column in (("collision_shapes.rs", "STATE_SHAPE"), ("outline_shapes.rs", "STATE_OUTLINE"),
                                 ("outline_shapes.rs", "STATE_INTERACTION"), ("light_props.rs", "STATE_ENTRY"),
                                 ("hardness.rs", "STATE_ENTRY"), ("sound_types.rs", "STATE_ENTRY"),
                                 ("block_entity_types.rs", "STATE_TYPE"), ("block_property_tables.rs", "STATE_PROPERTY_SET_IDS"),
                                 ("block_blast.rs", "STATE_RESISTANCE_ENTRY")):
            self.assertEqual(len(numeric_column(files[filename], column)), count, f"{filename}/{column}")
        self.assertEqual(numeric_column(files["block_entity_types.rs"], "STATE_TYPE")[0], 65535)
        self.assertEqual(numeric_column(files["block_entity_types.rs"], "STATE_TYPE")[self.inputs.sources[behavior.BASE]["block_states"].index("minecraft:furnace[facing=north,lit=false]")], 0)
        keys = self.inputs.bundle["domains"]["block_states"]["keys"]
        for name in ("generic_motion", "fluid_blocker", "ocean_floor", "motion_heightmap", "no_leaves_heightmap"):
            column = numeric_column(files["behavior_versions.rs"], name.upper())
            self.assertEqual([read_bit(column, raw) for raw in range(count)],
                             [self.inputs.columns[name]["versions"][behavior.LATEST][key] for key in keys], name)
        legacy = numeric_column(files["block_solidity.rs"], "BLOCKS_MOTION")
        self.assertEqual(len(legacy), 4046)
        self.assertIn("LEGACY_MOTION_STATE_COUNT: u32 = 32366", files["block_solidity.rs"])
        paths = sum(len(re.findall(r"PathType::\w+", text)) for name, text in files.items() if re.fullmatch(r"path_types_\d{4}\.rs", name))
        self.assertEqual(paths, count)

    def test_retained_collision_source_is_explicit_and_fully_preserved(self):
        snapshot, _ = census.read_report(behavior.COLLISION_PREFIX)
        self.assertEqual(snapshot["authority"], "retained-26.2-production-table")
        self.assertEqual(snapshot["source_sha256"], "ab570eac4a8e15b5a82c0e4987a1740a345be5fadd0df568d22c9e4350e34b1c")
        indices = numeric_column(self.files["collision_shapes.rs"], "STATE_SHAPE")
        self.assertEqual(indices[:32366], snapshot["state_shape"])
        self.assertEqual(numeric_column(self.files["sound_events.rs"], "SOUND_EVENT_FIXED_RANGES"), [])
        self.assertIn(str(behavior.SOUND_RANGES_PREFIX.relative_to(behavior.ROOT)), self.inputs.provenance)

    def test_tool_tags_and_typed_property_prefix_are_versioned(self):
        self.assertEqual(self.inputs.tools[behavior.BASE], self.inputs.tools[behavior.LATEST])
        old, latest = self.inputs.tags[behavior.BASE], self.inputs.tags[behavior.LATEST]
        self.assertEqual((len(old), len(latest)), (265, 300))
        self.assertNotIn("minecraft:poplar_planks", old["minecraft:mineable/axe"])
        self.assertIn("minecraft:poplar_planks", latest["minecraft:mineable/axe"])
        self.assertEqual(sum(old[name] != latest[name] for name in set(old) & set(latest)), 51)
        for name, prefix in (("PropertyKey", "Key"), ("BuiltinPropertyValue", "Value")):
            index = 0 if prefix == "Key" else 1
            expected = sorted({pair[index] for key in self.inputs.sources[behavior.BASE]["block_states"]
                               for pair in behavior.identities.state_properties(key)})
            variants = rust.property_variants(expected, prefix)
            body = self.files["block_property_tables.rs"].split(f"pub enum {name} {{", 1)[1].split("}", 1)[0]
            emitted = re.findall(r"(\w+) = (\d+)", body)
            self.assertEqual(emitted[:len(expected)], [(variant, str(raw)) for raw, variant in enumerate(variants)])

    def test_missing_capture_row_cannot_be_replaced_by_an_old_shared_row(self):
        inputs = copy.copy(self.inputs)
        inputs.columns = copy.deepcopy(self.inputs.columns)
        del inputs.columns["light"]["versions"][behavior.LATEST]["minecraft:stone"]
        with self.assertRaisesRegex(ValueError, "coverage differs: missing 1") as error:
            rust.build_rust_files(inputs)
        print(f"REJECTED missing latest-row control: {error.exception}")

    def test_manifest_covers_every_output_and_generation_is_deterministic(self):
        manifest = json.loads(self.files["behavior_manifest.json"])
        names = set(self.files) - {"behavior_manifest.json"}
        self.assertEqual(set(manifest["rust_sha256"]), names)
        for name in names:
            self.assertEqual(manifest["rust_sha256"][name], hashlib.sha256(self.files[name].encode()).hexdigest())
        self.assertEqual(rust.build_rust_files(self.inputs), self.files)


if __name__ == "__main__":
    unittest.main()
