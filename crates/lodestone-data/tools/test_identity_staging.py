"""Identity-only staging controls and independent official-report constants."""

import copy
import json
import sys
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
    classes = [HermeticTests]
    if "--official-reports" in sys.argv:
        sys.argv.remove("--official-reports")
        classes.append(OfficialReportTests)
    suite = unittest.TestSuite(unittest.defaultTestLoader.loadTestsFromTestCase(cls) for cls in classes)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    raise SystemExit(not result.wasSuccessful())
