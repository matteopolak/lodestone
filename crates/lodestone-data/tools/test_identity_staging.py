"""Identity-only staging controls and independent official-report constants."""

import ast
import copy
import hashlib
import json
import re
import sys
import tempfile
import unittest
from pathlib import Path

import canonical_census as census
import identity_staging as staging
from test_canonical_census import synthetic_sources


def identity_sources():
    sources = synthetic_sources()
    for version, source in sources.items():
        source["block_identities"] = {
            name: {
                "states": [key for key in source["block_states"] if key.split("[", 1)[0] == name],
                "default_state": name,
            }
            for name in source["blocks"]
        }
        value = "true" if version == "26.2" else "false"
        source["block_identities"]["minecraft:stone"]["default_state"] = f"minecraft:stone[wet={value}]"
    return sources


def rust_values(source, name):
    match = re.search(rf"pub static {name}: .*? = \[(.*?)\n\s*\];", source, re.DOTALL)
    if match is None:
        raise AssertionError(f"missing Rust column: {name}")
    values = re.sub(r"Some\((\d+)\)", r"\1", match[1]).replace("&", "")
    return ast.literal_eval("[" + values + "]")


class RustEmitterTests(unittest.TestCase):
    def setUp(self):
        self.sources = identity_sources()
        self.sources["26.3"]["block_identities"]["minecraft:stone"]["default_state"] = "minecraft:stone[wet=true]"
        self.manifest = census.build_manifest(self.sources)
        self.bundle = staging.build_bundle(self.manifest, self.sources)
        self.files = staging.build_rust_files(self.bundle, self.manifest, self.sources)

    def test_canonical_enums_keep_appended_ids_and_name_permutations(self):
        for name in ("block", "item"):
            source = self.files[f"{name}_enum.rs"]
            self.assertEqual(re.findall(r"^    ([A-Z]\w*) = (\d+),$", source, re.MULTILINE),
                             [("Air", "0"), ("Stone", "1"), ("New", "2")])
            self.assertEqual(rust_values(source, "REGISTRY_IDS_BY_NAME"), [0, 2, 1])
        self.assertEqual(rust_values(self.files["block_enum.rs"], "DEFAULT_STATE"), [0, 2, 3])
        self.assertEqual(rust_values(self.files["block_registry.rs"], "STATE_BLOCK"), [0, 1, 1, 2])
        self.assertEqual(rust_values(self.files["block_registry.rs"], "BLOCK_STATE_SPANS"), [(0, 1), (1, 2), (3, 1)])

    def test_state_rows_use_alphabetical_blocks_and_sorted_property_sets(self):
        source = self.files["block_states.rs"]
        self.assertEqual(rust_values(source, "PROPERTY_SETS"), [[], [("wet", "false")], [("wet", "true")]])
        self.assertEqual(rust_values(source, "STATES"), [(0, 0), (2, 1), (2, 2), (1, 0)])

    def test_versioned_defaults_and_wire_columns_preserve_missing_values(self):
        source = self.files["identity_versions.rs"]
        base, latest = source.split("pub mod v26_3 {")
        self.assertEqual(rust_values(base, "BLOCK_DEFAULT_STATES"), [0, 2, None])
        self.assertEqual(rust_values(latest, "BLOCK_DEFAULT_STATES"), [0, 2, 3])
        self.assertEqual(rust_values(base, "BLOCK_STATE_CANONICAL_TO_WIRE"), [0, 1, 2, None])
        self.assertEqual(rust_values(latest, "BLOCK_STATE_CANONICAL_TO_WIRE"), [0, 2, 3, 1])
        self.assertEqual(rust_values(latest, "BLOCK_STATE_WIRE_TO_CANONICAL"), [0, 3, 1, 2])
        self.assertEqual(rust_values(base, "ITEM_CANONICAL_TO_WIRE"), [0, 1, None])
        self.assertEqual(rust_values(latest, "BLOCK_WIRE_TO_CANONICAL"), [0, 2, 1])

    def test_emitter_rejects_valid_range_wrong_semantics_and_egress_alias(self):
        for domain, column, index, value, message in (
            ("blocks", "default_states", 1, 1, "default state differs"),
            ("items", "canonical_to_wire", 2, 0, "egress differs"),
        ):
            broken = copy.deepcopy(self.bundle)
            broken["domains"][domain]["versions"]["26.2"][column][index] = value
            with self.subTest(column=column), self.assertRaisesRegex(ValueError, message):
                staging.build_rust_files(broken, self.manifest, self.sources)

    def test_shared_default_conflicts_are_rejected_in_both_scopes(self):
        self.sources["26.3"]["block_identities"]["minecraft:stone"]["default_state"] = "minecraft:stone[wet=false]"
        for scope in staging.SCOPES:
            bundle = staging.build_bundle(self.manifest, self.sources, scope)
            with self.subTest(scope=scope), self.assertRaisesRegex(ValueError, "minecraft:stone.*conflicting semantic defaults"):
                staging.build_rust_files(bundle, self.manifest, self.sources)

    def test_runtime_check_uses_the_requested_identity_scope_and_detects_mutation(self):
        base = staging.build_bundle(self.manifest, self.sources, "base")
        files = staging.build_rust_files(base, self.manifest, self.sources)
        with tempfile.TemporaryDirectory(dir="/tmp") as temporary:
            directory = Path(temporary) / "runtime"
            staging.write_rust_files(directory, files)
            staging.check_runtime_files(directory, files, "base")
            with self.assertRaisesRegex(ValueError, "runtime block file differs"):
                staging.check_runtime_files(directory, self.files, "union")
            path = directory / "block_enum.rs"
            original = path.read_bytes()
            changed = original.replace(b"    0, 2,", b"    0, 1,", 1)
            self.assertNotEqual(original, changed)
            path.write_bytes(changed)
            with self.assertRaisesRegex(ValueError, "runtime block file differs.*block_enum.rs"):
                staging.check_runtime_files(directory, files, "base")
            union = Path(temporary) / "union"
            staging.write_rust_files(union, self.files)
            staging.check_runtime_files(union, self.files, "union")
            (union / "items.rs").write_text("deliberately truncated\n")
            with self.assertRaisesRegex(ValueError, "runtime block file differs.*items.rs"):
                staging.check_runtime_files(union, self.files, "union")

    def test_variant_spelling_controls_reject_collision_reserved_and_invalid_names(self):
        self.assertEqual(staging.enum_variants(["minecraft:oak_log", "minecraft:cut_copper"]), ["OakLog", "CutCopper"])
        for names, message in (
            (["minecraft:oak_log", "minecraft:oak__log"], "collision"),
            (["minecraft:self"], "invalid enum variant"),
            (["minecraft:1stone"], "unsupported built-in name"),
            (["plugin:stone"], "unsupported built-in name"),
            (["minecraft:__"], "invalid enum variant"),
        ):
            with self.subTest(names=names), self.assertRaisesRegex(ValueError, message):
                staging.enum_variants(names)

    def test_property_spelling_controls_reject_ambiguous_or_unsorted_values(self):
        self.assertEqual(staging.state_properties("minecraft:stone[axis=y,wet=true]"), (("axis", "y"), ("wet", "true")))
        for key in ("minecraft:stone[wet=true,axis=y]", "minecraft:stone[wet=true,wet=true]",
                    "minecraft:stone[wet=a=b]", "minecraft:stone[]", "minecraft:stone[wet=true"):
            with self.subTest(key=key), self.assertRaises(ValueError):
                staging.state_properties(key)

    def test_representation_limit_accepts_boundary_and_rejects_overflow(self):
        for limit in (65535, 65536, 4294967295):
            staging.require_width(limit, limit, "control")
            with self.subTest(limit=limit), self.assertRaisesRegex(ValueError, "representation limit"):
                staging.require_width(limit + 1, limit, "control")

    def test_byte_determinism_and_provenance_cover_every_rust_file(self):
        original = copy.deepcopy(self.bundle)
        self.assertEqual(self.files, staging.build_rust_files(self.bundle, self.manifest, self.sources))
        self.assertEqual(self.bundle, original)
        manifest = json.loads(self.files["manifest.json"])
        self.assertEqual(manifest["bundle_sha256"], staging.digest(self.bundle))
        self.assertEqual(manifest["file_sha256"], {
            name: hashlib.sha256(content.encode()).hexdigest()
            for name, content in self.files.items() if name.endswith(".rs")
        })
        self.assertEqual(manifest["counts"], {"blocks": 3, "block_states": 4, "items": 3})

    def test_base_projection_emits_only_base_version_and_ids(self):
        bundle = staging.build_bundle(self.manifest, self.sources, "base")
        files = staging.build_rust_files(bundle, self.manifest, self.sources)
        self.assertNotIn("pub mod v26_3", files["identity_versions.rs"])
        self.assertNotIn("New =", files["block_enum.rs"])
        self.assertEqual(rust_values(files["identity_versions.rs"], "BLOCK_DEFAULT_STATES"), [0, 2])
        self.assertEqual(rust_values(files["block_enum.rs"], "DEFAULT_STATE"), [0, 2])

    def test_private_write_refuses_overwrite_and_check_detects_modified_column(self):
        with tempfile.TemporaryDirectory(dir="/tmp") as temporary:
            destination = Path(temporary) / "rust"
            staging.write_rust_files(destination, self.files)
            staging.check_rust_files(destination, self.files)
            with self.assertRaises(FileExistsError):
                staging.write_rust_files(destination, self.files)
            path = destination / "identity_versions.rs"
            original = path.read_bytes()
            changed = original.replace(b"Some(2)", b"Some(1)", 1)
            self.assertNotEqual(changed, original)
            path.write_bytes(changed)
            with self.assertRaisesRegex(ValueError, "staged file differs.*identity_versions.rs"):
                staging.check_rust_files(destination, self.files)

    def test_private_write_refuses_runtime_source_destination(self):
        with self.assertRaisesRegex(ValueError, "private directory"):
            staging.write_rust_files(census.ROOT / "crates/lodestone-data/src/generated/staged", self.files)


    def test_private_write_rejects_symlink_escape_to_runtime_source(self):
        with tempfile.TemporaryDirectory(dir="/tmp") as temporary:
            link = Path(temporary) / "linked"
            link.symlink_to(census.ROOT / "crates/lodestone-data/src/generated", target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "private directory"):
                staging.write_rust_files(link / "staged", self.files)

    def test_check_rejects_extra_or_missing_file(self):
        with tempfile.TemporaryDirectory(dir="/tmp") as temporary:
            destination = Path(temporary) / "rust"
            staging.write_rust_files(destination, self.files)
            extra = destination / "extra.txt"
            extra.touch()
            with self.assertRaisesRegex(ValueError, "staged file set differs"):
                staging.check_rust_files(destination, self.files)
            extra.unlink()
            (destination / "items.rs").unlink()
            with self.assertRaisesRegex(ValueError, "staged file set differs"):
                staging.check_rust_files(destination, self.files)


class HermeticTests(unittest.TestCase):
    def setUp(self):
        self.sources = identity_sources()
        self.manifest = census.build_manifest(self.sources)
        self.bundle = staging.build_bundle(self.manifest, self.sources)

    def test_base_projection_has_only_base_identity_columns(self):
        bundle = staging.build_bundle(self.manifest, self.sources, "base")
        for name in census.DOMAINS:
            self.assertEqual(bundle["domains"][name]["keys"], self.sources["26.2"][name])
            self.assertEqual(list(bundle["domains"][name]["versions"]), ["26.2"])
        self.assertEqual(bundle["domains"]["blocks"]["state_spans"], [[0, 1], [1, 2]])
        self.assertEqual(bundle["domains"]["block_states"]["block_ids"], [0, 1, 1])

    def test_union_defaults_use_semantic_keys_and_keep_version_differences(self):
        blocks = self.bundle["domains"]["blocks"]
        self.assertEqual(blocks["state_spans"], [[0, 1], [1, 2], [3, 1]])
        self.assertEqual(blocks["versions"]["26.2"]["default_states"], [0, 2, None])
        self.assertEqual(blocks["versions"]["26.3"]["default_states"], [0, 1, 3])
        self.assertEqual(self.bundle["domains"]["block_states"]["block_ids"], [0, 1, 1, 2])

    def test_generation_is_deterministic_and_does_not_mutate_census(self):
        original = copy.deepcopy(self.manifest)
        self.assertEqual(census.serialize(self.bundle), census.serialize(staging.build_bundle(self.manifest, self.sources)))
        self.bundle["domains"]["items"]["keys"].reverse()
        self.assertEqual(self.manifest, original)

    def test_wrong_semantic_default_within_valid_span_is_rejected(self):
        self.bundle["domains"]["blocks"]["versions"]["26.2"]["default_states"][1] = 1
        with self.assertRaisesRegex(ValueError, "26.2/minecraft:stone default state differs"):
            staging.validate_bundle(self.bundle, self.manifest, self.sources)

    def test_wrong_semantic_span_within_valid_bounds_is_rejected(self):
        self.bundle["domains"]["blocks"]["state_spans"][1] = [1, 1]
        with self.assertRaisesRegex(ValueError, "minecraft:stone semantic state span differs"):
            staging.validate_bundle(self.bundle, self.manifest, self.sources)

    def test_wrong_owner_is_rejected(self):
        self.bundle["domains"]["block_states"]["block_ids"][2] = 0
        with self.assertRaisesRegex(ValueError, "owner differs"):
            staging.validate_bundle(self.bundle, self.manifest, self.sources)

    def test_invalid_integer_columns_are_rejected(self):
        for value in (True, -1, 1.0):
            with self.subTest(value=value):
                broken = copy.deepcopy(self.bundle)
                broken["domains"]["blocks"]["state_spans"][1][1] = value
                with self.assertRaisesRegex(ValueError, "invalid state span"):
                    staging.validate_bundle(broken, self.manifest, self.sources)

    def test_report_defaults_are_explicit_and_unique(self):
        for states in (
            [], [{"id": 0}], [{"id": 0, "default": 1}],
            [{"id": 0, "default": True}, {"id": 1, "default": True}],
        ):
            with self.subTest(states=states), self.assertRaises(ValueError):
                census.block_identities({"minecraft:stone": {"states": states}})

    def test_report_properties_are_sorted_without_changing_default(self):
        rows = census.block_identities({"minecraft:stone": {"states": [
            {"id": 7, "properties": {"wet": "true", "axis": "y"}, "default": True},
        ]}})
        expected = "minecraft:stone[axis=y,wet=true]"
        self.assertEqual(rows["minecraft:stone"], {"states": [expected], "default_state": expected})


class OfficialReportTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.sources = {
            version: census.load_source(census.ROOT / f".cache/mc/{version}/generated/reports")
            for version in census.VERSIONS
        }
        cls.manifest = census.build_manifest(cls.sources)
        cls.base = staging.build_bundle(cls.manifest, cls.sources, "base")
        cls.union = staging.build_bundle(cls.manifest, cls.sources, "union")
        cls.fixture = json.loads((Path(__file__).parent / "fixtures/identity-staging-witnesses.json").read_text())
        cls.mapping_fixture = json.loads((Path(__file__).parent / "fixtures/canonical-census-witnesses.json").read_text())
        cls.base_rust = staging.build_rust_files(cls.base, cls.manifest, cls.sources)
        cls.union_rust = staging.build_rust_files(cls.union, cls.manifest, cls.sources)

    def test_emitted_base_columns_match_independently_captured_hashes(self):
        files = self.base_rust
        names = rust_values(files["block_registry.rs"], "BLOCK_REGISTRY_NAMES")
        by_name = rust_values(files["block_enum.rs"], "REGISTRY_IDS_BY_NAME")
        properties = rust_values(files["block_states.rs"], "PROPERTY_SETS")
        state_keys = []
        for block_index, property_index in rust_values(files["block_states.rs"], "STATES"):
            name = names[by_name[block_index]]
            pairs = properties[property_index]
            suffix = ",".join(f"{key}={value}" for key, value in pairs)
            state_keys.append(f"{name}[{suffix}]" if suffix else name)
        columns = {
            "block_keys": names,
            "item_keys": rust_values(files["items.rs"], "ITEM_NAMES"),
            "state_keys": state_keys,
            "state_spans": [list(row) for row in rust_values(files["block_registry.rs"], "BLOCK_STATE_SPANS")],
            "default_states": rust_values(files["block_enum.rs"], "DEFAULT_STATE"),
            "block_ids": rust_values(files["block_registry.rs"], "STATE_BLOCK"),
        }
        self.assertEqual({name: staging.digest(values) for name, values in columns.items()}, self.fixture["column_sha256"])

    def test_emitted_union_witnesses_resolve_through_canonical_name_permutation(self):
        files = self.union_rust
        names = rust_values(files["block_registry.rs"], "BLOCK_REGISTRY_NAMES")
        self.assertEqual(names[1196], "minecraft:poplar_planks")
        self.assertEqual(rust_values(files["block_registry.rs"], "BLOCK_STATE_SPANS")[1196], (32366, 1))
        self.assertIn("PoplarPlanks = 1196,", files["block_enum.rs"])
        by_name = rust_values(files["block_enum.rs"], "REGISTRY_IDS_BY_NAME")
        block_index, property_index = rust_values(files["block_states.rs"], "STATES")[32366]
        self.assertEqual(names[by_name[block_index]], "minecraft:poplar_planks")
        self.assertEqual(rust_values(files["block_states.rs"], "PROPERTY_SETS")[property_index], [])
        base, latest = files["identity_versions.rs"].split("pub mod v26_3 {")
        self.assertIsNone(rust_values(base, "BLOCK_DEFAULT_STATES")[1196])
        self.assertEqual(rust_values(latest, "BLOCK_DEFAULT_STATES")[1196], 32366)
        self.assertEqual(rust_values(latest, "BLOCK_STATE_CANONICAL_TO_WIRE")[32366], 27)

    def test_canonical_defaults_preserve_base_and_use_the_introducing_release(self):
        base = rust_values(self.base_rust["block_enum.rs"], "DEFAULT_STATE")
        union = rust_values(self.union_rust["block_enum.rs"], "DEFAULT_STATE")
        self.assertEqual(union[:1196], base)
        self.assertEqual(staging.digest(base), self.fixture["column_sha256"]["default_states"])
        self.assertEqual(union[1196], 32366)
        for row in self.fixture["blocks"]:
            with self.subTest(block=row["key"]):
                self.assertEqual(base[row["id"]], row["default_state"])

    def test_adopted_union_identity_files_match_the_report_only_emitter(self):
        directory = census.ROOT / "crates/lodestone-data/src/generated"
        staging.check_runtime_files(directory, self.union_rust, "union")
        with tempfile.TemporaryDirectory(dir="/tmp") as temporary:
            destination = Path(temporary) / "control"
            staging.write_rust_files(destination, self.base_rust)
            path = destination / "block_enum.rs"
            original = path.read_bytes()
            changed = original.replace(b"    0, 1, 2, 3, 4, 5, 6, 7, 9,", b"    0, 1, 2, 3, 4, 5, 6, 7, 8,", 1)
            self.assertNotEqual(original, changed)
            path.write_bytes(changed)
            with self.assertRaisesRegex(ValueError, "runtime block file differs.*block_enum.rs"):
                staging.check_runtime_files(destination, self.base_rust, "base")

    def test_shared_official_default_conflict_fails_both_rust_scopes(self):
        sources = copy.deepcopy(self.sources)
        sources["26.3"]["block_identities"]["minecraft:oak_log"]["default_state"] = "minecraft:oak_log[axis=x]"
        for scope in staging.SCOPES:
            bundle = staging.build_bundle(self.manifest, sources, scope)
            with self.subTest(scope=scope), self.assertRaisesRegex(ValueError, "minecraft:oak_log.*conflicting semantic defaults"):
                staging.build_rust_files(bundle, self.manifest, sources)

    def test_emitted_mapping_columns_match_independent_wire_witnesses(self):
        base, latest = self.union_rust["identity_versions.rs"].split("pub mod v26_3 {")
        columns = {}
        for version, source in (("26.2", base), ("26.3", latest)):
            for domain, prefix in (("blocks", "BLOCK"), ("block_states", "BLOCK_STATE"), ("items", "ITEM")):
                columns[version, domain, "egress"] = rust_values(source, f"{prefix}_CANONICAL_TO_WIRE")
                columns[version, domain, "ingress"] = rust_values(source, f"{prefix}_WIRE_TO_CANONICAL")
        for row in self.mapping_fixture["witnesses"]:
            for version, wire in (("26.2", row["base_wire"]), ("26.3", row["latest_wire"])):
                with self.subTest(key=row["key"], version=version):
                    self.assertEqual(columns[version, row["domain"], "egress"][row["canonical"]], wire)
                    if wire is not None:
                        self.assertEqual(columns[version, row["domain"], "ingress"][wire], row["canonical"])

    def test_complete_base_columns_match_independently_captured_report_hashes(self):
        domains = self.base["domains"]
        self.assertEqual(self.sources["26.2"]["source_sha256"], self.fixture["source_sha256"])
        for name, count in self.fixture["counts"].items():
            self.assertEqual(len(domains[name]["keys"]), count)
        columns = {
            "block_keys": domains["blocks"]["keys"],
            "item_keys": domains["items"]["keys"],
            "state_keys": domains["block_states"]["keys"],
            "state_spans": domains["blocks"]["state_spans"],
            "default_states": domains["blocks"]["versions"]["26.2"]["default_states"],
            "block_ids": domains["block_states"]["block_ids"],
        }
        self.assertEqual({name: staging.digest(values) for name, values in columns.items()}, self.fixture["column_sha256"])

    def test_base_default_span_and_item_witnesses(self):
        domains = self.base["domains"]
        blocks = domains["blocks"]
        for row in self.fixture["blocks"]:
            with self.subTest(key=row["key"]):
                self.assertEqual(blocks["keys"][row["id"]], row["key"])
                self.assertEqual(blocks["state_spans"][row["id"]], [row["state_start"], row["state_count"]])
                default = blocks["versions"]["26.2"]["default_states"][row["id"]]
                self.assertEqual(default, row["default_state"])
                self.assertEqual(domains["block_states"]["keys"][default], row["default_key"])
        for row in self.fixture["items"]:
            self.assertEqual(domains["items"]["keys"][row["id"]], row["key"])

    def test_union_preserves_base_and_uses_canonical_defaults(self):
        for name in census.DOMAINS:
            base_keys = self.base["domains"][name]["keys"]
            self.assertEqual(self.union["domains"][name]["keys"][:len(base_keys)], base_keys)
        blocks = self.union["domains"]["blocks"]
        self.assertEqual(blocks["keys"][1196], "minecraft:poplar_planks")
        self.assertEqual(blocks["state_spans"][1196], [32366, 1])
        self.assertIsNone(blocks["versions"]["26.2"]["default_states"][1196])
        self.assertEqual(blocks["versions"]["26.3"]["default_states"][1196], 32366)
        self.assertEqual(blocks["versions"]["26.3"]["default_states"][35], 86)

    def test_official_semantic_default_negative_control(self):
        broken = copy.deepcopy(self.base)
        broken["domains"]["blocks"]["versions"]["26.2"]["default_states"][49] = 136
        with self.assertRaisesRegex(ValueError, "26.2/minecraft:oak_log default state differs"):
            staging.validate_bundle(broken, self.manifest, self.sources)

    def test_official_semantic_span_negative_control(self):
        broken = copy.deepcopy(self.base)
        broken["domains"]["blocks"]["state_spans"][49] = [136, 2]
        with self.assertRaisesRegex(ValueError, "minecraft:oak_log semantic state span differs"):
            staging.validate_bundle(broken, self.manifest, self.sources)

    def test_repeat_generation_for_both_scopes(self):
        for scope, bundle in (("base", self.base), ("union", self.union)):
            self.assertEqual(census.serialize(bundle), census.serialize(staging.build_bundle(self.manifest, self.sources, scope)))


if __name__ == "__main__":
    classes = [HermeticTests, RustEmitterTests]
    if "--official-reports" in sys.argv:
        sys.argv.remove("--official-reports")
        classes.append(OfficialReportTests)
    suite = unittest.TestSuite(unittest.defaultTestLoader.loadTestsFromTestCase(cls) for cls in classes)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    raise SystemExit(not result.wasSuccessful())
