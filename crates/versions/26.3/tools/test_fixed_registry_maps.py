"""Hermetic rejection controls and explicit official-report identity audits."""

import copy
import json
from pathlib import Path
import re
import sys
import unittest
import zipfile

import fixed_registry_maps as maps


def synthetic_reports():
    def registry(names):
        return {"entries": {name: {"protocol_id": raw} for raw, name in enumerate(names)}}
    return {
        "26.2": {
            "minecraft:block": registry(["minecraft:air", "minecraft:stone"]),
            "minecraft:data_component_type": registry(["minecraft:retired", "minecraft:kept"]),
            "minecraft:old_domain": registry(["minecraft:old"]),
        },
        "26.3": {
            "minecraft:block": registry(["minecraft:air", "minecraft:new", "minecraft:stone"]),
            "minecraft:data_component_type": registry(["minecraft:new", "minecraft:kept"]),
            "minecraft:new_domain": registry(["minecraft:new"]),
        },
    }


def emitted_tables(text):
    names = re.findall(r"static NAMES_\d+: \[&str; \d+\] = \[\n(.*?)\n\];", text, re.S)
    blocks = re.findall(r"    RegistryTable \{\n(.*?)\n    \},", text, re.S)
    if len(names) != len(blocks):
        raise ValueError("emitted fixed maps: name/table coverage differs")
    result = {}
    for name_rows, block in zip(names, blocks):
        registry = json.loads(re.search(r"name: (\"[^\"]+\")", block)[1])
        rows = re.findall(r"(?:from_wire|to_wire): &\[(.*?)\]", block)
        if len(rows) != 4:
            raise ValueError(f"emitted fixed maps: {registry} release coverage differs")
        result[registry] = {
            "keys": [json.loads(name) for name in re.findall(r'"[^"\n]+"', name_rows)],
            "versions": {
                version: {
                    field: [[int(value) for value in row] for row in re.findall(r"Run::new\((\d+), (\d+), (\d+)\)", rows[index * 2 + offset])]
                    for offset, field in enumerate(("wire_to_canonical", "canonical_to_wire"))
                }
                for index, version in enumerate(maps.VERSIONS)
            },
        }
    return result


def audit_emitted(text, reports):
    tables = emitted_tables(text)
    registries = set().union(*(reports[version] for version in maps.VERSIONS))
    if set(tables) != registries:
        raise ValueError("emitted fixed maps: registry coverage differs")
    for registry in sorted(registries):
        wire_names = {}
        for version in maps.VERSIONS:
            entries = reports[version].get(registry, {}).get("entries", {})
            wire_names[version] = [name for name, _ in sorted(entries.items(), key=lambda row: row[1]["protocol_id"])]
        base, latest = (wire_names[version] for version in maps.VERSIONS)
        keys = base + [name for name in latest if name not in base]
        if tables[registry]["keys"] != keys:
            raise ValueError(f"emitted fixed maps: {registry} semantic names differ")
        for version in maps.VERSIONS:
            forward = tables[registry]["versions"][version]["wire_to_canonical"]
            reverse = tables[registry]["versions"][version]["canonical_to_wire"]
            for raw, name in enumerate(wire_names[version]):
                if maps.translate_runs(forward, raw) != keys.index(name):
                    raise ValueError(f"emitted fixed maps: {registry}/{version} wrong ingress at {raw}")
            for raw, name in enumerate(keys):
                expected = wire_names[version].index(name) if name in wire_names[version] else None
                if maps.translate_runs(reverse, raw) != expected:
                    raise ValueError(f"emitted fixed maps: {registry}/{version} wrong egress at {raw}")
            if maps.translate_runs(forward, len(wire_names[version])) is not None:
                raise ValueError(f"emitted fixed maps: {registry}/{version} ingress spills out of range")
            if maps.translate_runs(reverse, len(keys)) is not None:
                raise ValueError(f"emitted fixed maps: {registry}/{version} egress spills out of range")


class HermeticTests(unittest.TestCase):
    def setUp(self):
        self.reports = synthetic_reports()
        self.hashes = {"26.2": "synthetic-base", "26.3": "synthetic-latest"}
        self.manifest = maps.build_manifest(self.reports, self.hashes)

    def reject(self, label, pattern, action):
        with self.assertRaisesRegex(ValueError, pattern) as caught:
            action()
        print(f"rejection control {label}: {caught.exception}")

    def validate(self):
        maps.validate_manifest(self.manifest, self.reports, self.hashes)

    def test_retained_prefix_shift_addition_and_removed_egress(self):
        block = self.manifest["domains"]["minecraft:block"]
        self.assertEqual(block["keys"], ["minecraft:air", "minecraft:stone", "minecraft:new"])
        self.assertEqual(block["versions"]["26.3"]["wire_to_canonical"], [0, 2, 1])
        component = self.manifest["domains"]["minecraft:data_component_type"]
        self.assertEqual(component["keys"], ["minecraft:retired", "minecraft:kept", "minecraft:new"])
        self.assertEqual(component["versions"]["26.3"]["canonical_to_wire"], [None, 1, 0])

    def test_absent_release_domains_have_no_numeric_alias(self):
        old = self.manifest["domains"]["minecraft:old_domain"]["versions"]["26.3"]
        new = self.manifest["domains"]["minecraft:new_domain"]["versions"]["26.2"]
        self.assertEqual(old, {"wire_count": 0, "wire_to_canonical": [], "canonical_to_wire": [None]})
        self.assertEqual(new, {"wire_count": 0, "wire_to_canonical": [], "canonical_to_wire": [None]})

    def test_shifted_ingress_identity_alias_is_rejected(self):
        self.manifest["domains"]["minecraft:block"]["versions"]["26.3"]["wire_to_canonical"][2] = 2
        self.reject("shifted identity", "wrong ingress at wire ID 2", self.validate)

    def test_new_only_old_egress_alias_is_rejected(self):
        self.manifest["domains"]["minecraft:block"]["versions"]["26.2"]["canonical_to_wire"][2] = 0
        self.reject("new identity in old release", "wrong egress at canonical ID 2", self.validate)

    def test_removed_component_alias_is_rejected(self):
        self.manifest["domains"]["minecraft:data_component_type"]["versions"]["26.3"]["canonical_to_wire"][0] = 0
        self.reject("removed component", "wrong egress at canonical ID 0", self.validate)

    def test_base_prefix_reordering_is_rejected(self):
        self.manifest["domains"]["minecraft:block"]["keys"][:2] = ["minecraft:stone", "minecraft:air"]
        self.reject("reordered prefix", "canonical base prefix", self.validate)

    def test_changed_addition_name_is_rejected(self):
        self.manifest["domains"]["minecraft:block"]["keys"][2] = "minecraft:wrong"
        self.reject("changed addition", "canonical additions", self.validate)

    def test_truncated_mapping_is_rejected(self):
        self.manifest["domains"]["minecraft:block"]["versions"]["26.3"]["wire_to_canonical"].pop()
        self.reject("truncated mapping", "mapping length", self.validate)

    def test_missing_registry_is_rejected(self):
        del self.manifest["domains"]["minecraft:block"]
        self.reject("missing registry", "registry coverage", self.validate)

    def test_wrong_release_provenance_is_rejected(self):
        self.manifest["source_sha256"] = {"26.2": "synthetic-latest", "26.3": "synthetic-base"}
        self.reject("wrong release", "report provenance", self.validate)

    def test_mutated_provenance_does_not_mutate_the_authenticated_inputs(self):
        self.manifest["source_sha256"]["26.2"] = "wrong-base"
        self.assertEqual(self.hashes["26.2"], "synthetic-base")
        self.reject("mutated provenance", "report provenance", self.validate)

    def test_duplicate_sparse_bool_and_negative_report_ids_are_rejected(self):
        for label, raw in (("duplicate", 0), ("sparse", 3), ("boolean", True), ("negative", -1)):
            with self.subTest(label=label):
                reports = synthetic_reports()
                reports["26.2"]["minecraft:block"]["entries"]["minecraft:stone"]["protocol_id"] = raw
                self.reject(label, "duplicate|invalid|dense", lambda: maps.build_manifest(reports, self.hashes))

    def test_missing_sound_name_is_rejected_by_shared_sound_policy(self):
        self.reports["26.2"]["minecraft:sound_event"] = {"entries": {"minecraft:old": {"protocol_id": 0}}}
        self.reports["26.3"]["minecraft:sound_event"] = {"entries": {"minecraft:new": {"protocol_id": 0}}}
        self.reject("removed canonical sound", "removed a base identity", lambda: maps.build_manifest(self.reports, self.hashes))

    def test_duplicate_json_key_is_rejected(self):
        self.reject("duplicate source name", "duplicate JSON key", lambda: json.loads('{"a": 0, "a": 1}', object_pairs_hook=maps.census.unique_object))

    def test_compact_runs_preserve_gaps_and_literal_positions(self):
        rows = maps.compact_runs([4, 5, None, 7, 8, 2])
        self.assertEqual(rows, [[0, 4, 2], [3, 7, 2], [5, 2, 1]])
        self.assertEqual([maps.translate_runs(rows, raw) for raw in range(7)], [4, 5, None, 7, 8, 2, None])
        self.assertIsNone(maps.translate_runs(rows, -1))

    def test_emitted_numeric_maps_are_audited_against_names(self):
        text = maps.render(self.manifest)
        audit_emitted(text, self.reports)
        self.assertEqual(text, maps.render(maps.build_manifest(self.reports, self.hashes)))
        broken = text.replace("from_wire: &[Run::new(0, 0, 1), Run::new(1, 2, 1)", "from_wire: &[Run::new(0, 0, 1), Run::new(1, 1, 1)", 1)
        self.assertNotEqual(broken, text)
        self.reject("emitted wire alias", "wrong ingress", lambda: audit_emitted(broken, self.reports))

    def test_bootstrap_parameter_shapes_distinguish_simple_and_complex_values(self):
        owner = maps.PARTICLE_BOOTSTRAP.removesuffix(".class")
        simple = "(Ljava/lang/String;Z)Ltest/Simple;"
        complex_shape = "(Ljava/lang/String;ZLjava/util/function/Function;Ljava/util/function/Function;)Ltest/Complex;"
        rows = [
            (0, "ldc", "leaf"), (1, "invokestatic", (owner, ("query", simple))),
            (2, "ldc", "block"), (3, "invokestatic", (owner, ("query", complex_shape))),
        ]
        expected_names = ["minecraft:leaf", "minecraft:block"]
        self.assertEqual(maps.particle_registrations(rows, expected_names), {"minecraft:leaf": True, "minecraft:block": False})
        self.reject("missing particle bootstrap row", "report name coverage", lambda: maps.particle_registrations(rows[:2], expected_names))
        self.reject("duplicate particle bootstrap row", "duplicate registration", lambda: maps.particle_registrations(rows + rows[:2], expected_names))


class OfficialReportTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.inputs = maps.load_inputs(maps.ROOT / ".cache/mc/26.2/generated/reports", maps.ROOT / ".cache/mc/26.3/generated/reports")
        cls.manifest = maps.build_manifest(*cls.inputs)
        cls.fixture = json.loads((Path(__file__).parent / "fixtures/fixed-registry-witnesses.json").read_text())
        cls.jars = {version: maps.ROOT / f".cache/mc/{version}/versions/{version}/server-{version}.jar" for version in maps.VERSIONS}
        cls.particle_flags, cls.particle_sha256 = maps.load_particle_inputs(cls.jars, cls.inputs[0])

    def test_pinned_sources_and_complete_fixed_registry_coverage(self):
        self.assertEqual(self.inputs[1], self.fixture["source_sha256"])
        self.assertEqual(len(self.manifest["domains"]), 102)
        self.assertEqual(sum(len(domain["keys"]) for domain in self.manifest["domains"].values()), 7458)
        maps.validate_manifest(self.manifest, *self.inputs)
        audit_emitted(maps.render(self.manifest), self.inputs[0])
        self.assertEqual(maps.OUTPUT.read_text(), maps.render(self.manifest))

    def test_independent_literal_names_counts_and_wire_bytes(self):
        self.assertGreater(len(self.fixture["witnesses"]), 0)
        for registry, counts in self.fixture["counts"].items():
            domain = self.manifest["domains"][registry]
            self.assertEqual([domain["versions"][version]["wire_count"] for version in maps.VERSIONS] + [len(domain["keys"])], counts)
        for row in self.fixture["witnesses"]:
            with self.subTest(name=row["name"]):
                domain = self.manifest["domains"][row["registry"]]
                self.assertEqual(domain["keys"][row["canonical"]], row["name"])
                for version, field in (("26.2", "base_wire"), ("26.3", "latest_wire")):
                    expected = row[field]
                    mapping = domain["versions"][version]
                    self.assertEqual(mapping["canonical_to_wire"][row["canonical"]], expected)
                    if expected is not None:
                        self.assertEqual(self.inputs[0][version][row["registry"]]["entries"][row["name"]]["protocol_id"], expected)
                        self.assertEqual(mapping["wire_to_canonical"][expected], row["canonical"])
                if "latest_varint_hex" in row:
                    value = row["latest_wire"]
                    octets = []
                    while value >= 128:
                        octets.append((value & 127) | 128)
                        value >>= 7
                    octets.append(value)
                    self.assertEqual(bytes(octets).hex(), row["latest_varint_hex"])

    def test_report_alias_control_fails_for_same_component_wire_id(self):
        broken = copy.deepcopy(self.manifest)
        broken["domains"]["minecraft:data_component_type"]["versions"]["26.3"]["wire_to_canonical"][40] = 40
        with self.assertRaisesRegex(ValueError, "wrong ingress at wire ID 40") as caught:
            maps.validate_manifest(broken, *self.inputs)
        print(f"official rejection control component ID 40: {caught.exception}")

    def test_selected_release_rejection_does_not_drop_retained_identity(self):
        components = self.manifest["domains"]["minecraft:data_component_type"]
        self.assertEqual(components["keys"][40], "minecraft:swing_animation")
        self.assertIsNone(components["versions"]["26.3"]["canonical_to_wire"][40])
        self.assertEqual(components["versions"]["26.2"]["canonical_to_wire"][40], 40)
        decorated = self.manifest["domains"]["minecraft:decorated_pot_pattern"]
        self.assertEqual(decorated["versions"]["26.3"]["wire_count"], 0)
        self.assertTrue(all(raw is None for raw in decorated["versions"]["26.3"]["canonical_to_wire"]))

    def test_existing_runtime_canonical_names_keep_the_report_prefix(self):
        files = {
            "minecraft:entity_type": "entity_type_enum.rs",
            "minecraft:particle_type": "particle_types.rs",
            "minecraft:data_component_type": "data_component_types.rs",
            "minecraft:attribute": "attribute_types.rs",
            "minecraft:menu": "menus.rs",
            "minecraft:sound_event": "sound_events.rs",
        }
        for registry, filename in files.items():
            text = (maps.ROOT / "crates/lodestone-data/src/generated" / filename).read_text()
            if filename == "entity_type_enum.rs":
                names = re.findall(r"^    /// `(minecraft:[^`]+)`$", text, re.M)
            else:
                names = re.findall(r'^    "(minecraft:[^"\n]+)",$', text, re.M)
            with self.subTest(registry=registry):
                self.assertGreater(len(names), 0)
                self.assertEqual(names, self.manifest["domains"][registry]["keys"][:len(names)])

    def require_particle_witnesses(self, flags):
        for name, expected in self.fixture["particle_simple_witnesses"].items():
            if flags["26.3"].get(name) != expected:
                raise ValueError(f"particle simple witness differs: {name}")

    def test_authenticated_particle_bootstraps_and_generated_total_tables(self):
        self.assertEqual(self.particle_sha256, self.fixture["particle_bootstrap_sha256"])
        self.assertEqual({version: sum(flags.values()) for version, flags in self.particle_flags.items()}, self.fixture["particle_simple_counts"])
        self.require_particle_witnesses(self.particle_flags)
        files = maps.render_data_tables(self.manifest, self.particle_flags, self.particle_sha256)
        for filename, expected in files.items():
            path = maps.ROOT / "crates/lodestone-data/src/generated" / filename
            self.assertEqual(path.read_text(), expected)
        names = self.manifest["domains"]["minecraft:particle_type"]["keys"]
        values = maps.canonical_particle_flags(self.manifest, self.particle_flags)
        self.assertEqual(len(values), 128)
        self.assertEqual(values[:125], [self.particle_flags["26.2"][name] for name in names[:125]])
        self.assertEqual(values[125:], [True, True, True])
        self.assertFalse(values[43])

    def test_descriptor_control_fails_the_independent_simple_particle_gate(self):
        with zipfile.ZipFile(self.jars["26.3"]) as jar:
            parsed = maps.Class(jar.read(maps.PARTICLE_BOOTSTRAP))
        rows = parsed.instructions(parsed.methods[("<clinit>", "()V")])
        start = next(index for index, row in enumerate(rows) if row[1] in ("ldc", "ldc_w") and row[2] == "red_poplar_leaves")
        target = next(index for index in range(start + 1, len(rows)) if rows[index][1] == "invokestatic")
        offset, opcode, (owner, (method, descriptor)) = rows[target]
        changed = "(Ljava/lang/String;ZLjava/util/function/Function;Ljava/util/function/Function;)" + descriptor.partition(")")[2]
        rows[target] = (offset, opcode, (owner, (method, changed)))
        broken = copy.deepcopy(self.particle_flags)
        names = maps.registry_names(self.inputs[0]["26.3"], "minecraft:particle_type", "26.3")
        broken["26.3"] = maps.particle_registrations(rows, names)
        self.assertFalse(broken["26.3"]["minecraft:red_poplar_leaves"])
        with self.assertRaisesRegex(ValueError, "particle simple witness differs") as caught:
            self.require_particle_witnesses(broken)
        print(f"official rejection control changed particle descriptor: {caught.exception}")

    def test_missing_and_changed_shared_particle_flags_require_explicit_policy(self):
        broken = copy.deepcopy(self.particle_flags)
        del broken["26.3"]["minecraft:red_poplar_leaves"]
        with self.assertRaisesRegex(ValueError, "invalid or incomplete flags") as caught:
            maps.canonical_particle_flags(self.manifest, broken)
        print(f"official rejection control incomplete particle flags: {caught.exception}")
        broken = copy.deepcopy(self.particle_flags)
        broken["26.3"]["minecraft:tinted_leaves"] = True
        with self.assertRaisesRegex(ValueError, "changed payload classification") as caught:
            maps.canonical_particle_flags(self.manifest, broken)
        print(f"official rejection control changed shared particle flags: {caught.exception}")


if __name__ == "__main__":
    classes = [HermeticTests]
    if "--official-reports" in sys.argv:
        sys.argv.remove("--official-reports")
        classes.append(OfficialReportTests)
    suite = unittest.TestSuite(unittest.defaultTestLoader.loadTestsFromTestCase(cls) for cls in classes)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    raise SystemExit(not result.wasSuccessful())
