"""Small hermetic controls plus an explicitly requested official-report audit."""

import copy
import json
import sys
import unittest
from pathlib import Path

import canonical_census as census


def synthetic_sources():
    domains = {
        "blocks": ["minecraft:air", "minecraft:stone"],
        "block_states": ["minecraft:air", "minecraft:stone[wet=false]", "minecraft:stone[wet=true]"],
        "items": ["minecraft:air", "minecraft:stone"],
    }
    base = {**domains, "source_sha256": {"synthetic": "base"}}
    latest = {
        domain: [keys[0], "minecraft:new", *keys[1:]] for domain, keys in domains.items()
    }
    latest["source_sha256"] = {"synthetic": "latest"}
    return {"26.2": base, "26.3": latest}


class HermeticTests(unittest.TestCase):
    def setUp(self):
        self.sources = synthetic_sources()
        self.manifest = census.build_manifest(self.sources)

    def test_old_prefix_and_appended_identity(self):
        states = self.manifest["domains"]["block_states"]
        self.assertEqual(states["keys"], ["minecraft:air", "minecraft:stone[wet=false]", "minecraft:stone[wet=true]", "minecraft:new"])
        self.assertEqual(states["versions"]["26.3"]["wire_to_canonical"], [0, 3, 1, 2])
        self.assertEqual(states["versions"]["26.2"]["canonical_to_wire"], [0, 1, 2, None])
        self.assertEqual(census.serialize(self.manifest), census.serialize(census.build_manifest(self.sources)))

    def test_shifted_old_identity_control_is_rejected(self):
        mapping = self.manifest["domains"]["block_states"]["versions"]["26.3"]
        mapping["wire_to_canonical"][2] = 2
        with self.assertRaisesRegex(ValueError, "wrong ingress"):
            census.validate_manifest(self.manifest, self.sources)

    def test_new_only_egress_alias_control_is_rejected(self):
        self.manifest["domains"]["items"]["versions"]["26.2"]["canonical_to_wire"][-1] = 0
        with self.assertRaisesRegex(ValueError, "wrong egress"):
            census.validate_manifest(self.manifest, self.sources)

    def test_reordered_prefix_is_rejected(self):
        self.manifest["domains"]["blocks"]["keys"].reverse()
        with self.assertRaisesRegex(ValueError, "base prefix"):
            census.validate_manifest(self.manifest, self.sources)

    def test_removed_base_identity_is_rejected(self):
        self.sources["26.3"]["items"].remove("minecraft:stone")
        with self.assertRaisesRegex(ValueError, "removed a base identity"):
            census.build_manifest(self.sources)

    def test_new_states_on_existing_block_require_span_upgrade(self):
        self.sources["26.3"]["block_states"].append("minecraft:stone[wet=maybe]")
        with self.assertRaisesRegex(ValueError, "multiple canonical spans"):
            census.build_manifest(self.sources)

    def test_invalid_report_ids_are_rejected(self):
        for rows in ([('a', 0), ('b', 0)], [('a', 0), ('a', 1)], [('a', 1)], [('a', True)], []):
            with self.subTest(rows=rows), self.assertRaises(ValueError):
                census.ordered_keys(rows, "control")

    def test_duplicate_json_names_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "duplicate JSON key"):
            json.loads('{"air": 0, "air": 1}', object_pairs_hook=census.unique_object)


class OfficialReportTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.sources = {
            version: census.load_source(census.ROOT / f".cache/mc/{version}/generated/reports")
            for version in census.VERSIONS
        }
        cls.manifest = census.build_manifest(cls.sources)
        cls.fixture = json.loads((Path(__file__).parent / "fixtures/canonical-census-witnesses.json").read_text())

    def test_complete_counts_and_independent_witnesses(self):
        self.assertGreater(len(self.fixture["witnesses"]), 0)
        for domain, counts in self.fixture["counts"].items():
            self.assertEqual([len(self.sources[v][domain]) for v in census.VERSIONS], counts)
            self.assertEqual(len(self.manifest["domains"][domain]["keys"]), counts[1])
        for row in self.fixture["witnesses"]:
            with self.subTest(key=row["key"]):
                domain = self.manifest["domains"][row["domain"]]
                self.assertEqual(domain["keys"][row["canonical"]], row["key"])
                for version, field in (("26.2", "base_wire"), ("26.3", "latest_wire")):
                    wire = row[field]
                    mapping = domain["versions"][version]
                    self.assertEqual(mapping["canonical_to_wire"][row["canonical"]], wire)
                    if wire is not None:
                        self.assertEqual(self.sources[version][row["domain"]][wire], row["key"])
                        self.assertEqual(mapping["wire_to_canonical"][wire], row["canonical"])

    def test_all_mappings_and_repeated_generation(self):
        census.validate_manifest(self.manifest, self.sources)
        self.assertEqual(census.serialize(self.manifest), census.serialize(census.build_manifest(self.sources)))

    def test_official_shifted_old_negative_control(self):
        broken = copy.deepcopy(self.manifest)
        broken["domains"]["block_states"]["versions"]["26.3"]["wire_to_canonical"][89] = 89
        with self.assertRaisesRegex(ValueError, "wrong ingress at wire ID 89"):
            census.validate_manifest(broken, self.sources)

    def test_official_new_only_negative_control(self):
        broken = copy.deepcopy(self.manifest)
        broken["domains"]["items"]["versions"]["26.2"]["canonical_to_wire"][1537] = 72
        with self.assertRaisesRegex(ValueError, "wrong egress at canonical ID 1537"):
            census.validate_manifest(broken, self.sources)


if __name__ == "__main__":
    classes = [HermeticTests]
    if "--official-reports" in sys.argv:
        sys.argv.remove("--official-reports")
        classes.append(OfficialReportTests)
    suite = unittest.TestSuite(unittest.defaultTestLoader.loadTestsFromTestCase(cls) for cls in classes)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    raise SystemExit(not result.wasSuccessful())
