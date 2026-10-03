#!/usr/bin/env python3
"""Small synthetic controls for isolated comparison asset staging."""

import hashlib
import importlib.util
import json
import pathlib
import tempfile
import unittest
import zipfile
from unittest import mock


SCRIPT = pathlib.Path(__file__).with_name("prepare-vanilla-comparison-assets.py")
SPEC = importlib.util.spec_from_file_location("prepare_vanilla_comparison_assets", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class ComparisonAssetTests(unittest.TestCase):
    ABC_SHA256 = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"

    def fixture(self, root, release="26.3", textures=True):
        jar = root / "client.jar"
        with zipfile.ZipFile(jar, "w") as archive:
            archive.writestr("version.json", json.dumps({"id": release}))
            if textures:
                archive.writestr("assets/minecraft/textures/block/stone.png", b"abc")
                archive.writestr("assets/minecraft/textures/entity/player/wide/steve.png", b"abc")
            archive.writestr("assets/minecraft/models/block/stone.json", b"model")
            archive.writestr("assets/minecraft/font/default.json", b"font references")
            archive.writestr("data/minecraft/recipe/example.json", b"recipe")
            archive.writestr("example.class", b"bytecode")
        report = root / "blocks.json"
        report.write_text('{"minecraft:stone":{"states":[{"id":1,"default":true}]}}\n', encoding="utf-8")
        return jar, report

    def test_preserves_full_original_archive_and_inventory_without_second_jar_copy(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            jar, report = self.fixture(root)
            output = root / ".cache" / "benchmarks" / "vanilla-26.3"
            original = jar.read_bytes()
            expected_sha1 = hashlib.sha1(original).hexdigest()
            with mock.patch.object(MODULE, "BENCHMARK_ROOT", output.parent):
                manifest = MODULE.stage_assets(jar, report, "26.3", output, expected_sha1)
            self.assertEqual((output / "lodestone-resources.zip").read_bytes(), original)
            self.assertEqual((output / "generated/reports/blocks.json").read_bytes(), report.read_bytes())
            self.assertEqual(jar.read_bytes(), original)
            self.assertEqual({path.name for path in output.iterdir()}, {"lodestone-resources.zip", "generated", MODULE.MANIFEST_NAME})
            self.assertEqual(manifest["source_jar"]["sha256"], hashlib.sha256(original).hexdigest())
            self.assertTrue(manifest["source_jar"]["expected_sha1_matched"])
            inventory = manifest["texture_inventory"]
            self.assertEqual(inventory["count"], 2)
            self.assertEqual(inventory["entries"], [
                {"path": "assets/minecraft/textures/block/stone.png", "bytes": 3, "sha256": self.ABC_SHA256},
                {"path": "assets/minecraft/textures/entity/player/wide/steve.png", "bytes": 3, "sha256": self.ABC_SHA256},
            ])
            self.assertFalse(manifest["external_indexed_assets_included"])
            self.assertFalse(manifest["official_source_authenticity_verified"])
            self.assertFalse(manifest["block_report_release_verified"])
            self.assertEqual(json.loads((output / MODULE.MANIFEST_NAME).read_text(encoding="utf-8")), manifest)

    def test_wrong_release_or_expected_hash_fails_before_output_creation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            jar, report = self.fixture(root)
            output = root / "benchmarks" / "assets"
            with mock.patch.object(MODULE, "BENCHMARK_ROOT", output.parent):
                with self.assertRaisesRegex(ValueError, "release does not match"):
                    MODULE.stage_assets(jar, report, "26.2", output)
                with self.assertRaisesRegex(ValueError, "SHA-1 mismatch"):
                    MODULE.stage_assets(jar, report, "26.3", output, "0" * 40)
            self.assertFalse(output.exists())

    def test_existing_directory_is_preserved_and_unsafe_output_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            jar, report = self.fixture(root)
            isolated = root / "benchmarks"
            output = isolated / "existing"
            output.mkdir(parents=True)
            sentinel = output / "keep-me"
            sentinel.write_bytes(b"unchanged")
            with mock.patch.object(MODULE, "BENCHMARK_ROOT", isolated):
                with self.assertRaisesRegex(FileExistsError, "overwrite"):
                    MODULE.stage_assets(jar, report, "26.3", output)
                for invalid in (isolated, root / "production-assets"):
                    with self.subTest(invalid=invalid), self.assertRaisesRegex(ValueError, "isolated directory"):
                        MODULE.stage_assets(jar, report, "26.3", invalid)
                link = isolated / "linked"
                link.symlink_to(root, target_is_directory=True)
                with self.assertRaisesRegex(ValueError, "isolated directory"):
                    MODULE.stage_assets(jar, report, "26.3", link / "assets")
            self.assertEqual(sentinel.read_bytes(), b"unchanged")
            self.assertFalse((root / "assets").exists())

    def test_corrupt_archive_and_missing_textures_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            jar, report = self.fixture(root)
            output = root / "benchmarks" / "assets"
            with zipfile.ZipFile(jar) as archive:
                info = archive.getinfo("assets/minecraft/textures/block/stone.png")
                data_offset = info.header_offset + 30 + len(info.filename.encode("utf-8")) + len(info.extra)
            corrupted = bytearray(jar.read_bytes())
            corrupted[data_offset] ^= 1
            jar.write_bytes(corrupted)
            with mock.patch.object(MODULE, "BENCHMARK_ROOT", output.parent):
                with self.assertRaisesRegex(ValueError, "CRC validation"):
                    MODULE.stage_assets(jar, report, "26.3", output)
                jar, report = self.fixture(root, textures=False)
                with self.assertRaisesRegex(ValueError, "no texture PNGs"):
                    MODULE.stage_assets(jar, report, "26.3", output)
            self.assertFalse(output.exists())

    def test_invalid_report_and_insufficient_disk_space_fail_before_copy(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            jar, report = self.fixture(root)
            output = root / "benchmarks" / "assets"
            with mock.patch.object(MODULE, "BENCHMARK_ROOT", output.parent):
                report.write_text("[]", encoding="utf-8")
                with self.assertRaisesRegex(ValueError, "nonempty JSON object"):
                    MODULE.stage_assets(jar, report, "26.3", output)
                report.write_text('{"block":{}}', encoding="utf-8")
                with mock.patch.object(MODULE.shutil, "disk_usage", return_value=mock.Mock(free=0)):
                    with self.assertRaisesRegex(OSError, "insufficient free space"):
                        MODULE.stage_assets(jar, report, "26.3", output)
            self.assertFalse(output.exists())

    def test_missing_expected_digest_remains_explicitly_unauthenticated(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            jar, report = self.fixture(root)
            output = root / "benchmarks" / "assets"
            with mock.patch.object(MODULE, "BENCHMARK_ROOT", output.parent):
                manifest = MODULE.stage_assets(jar, report, "26.3", output)
            self.assertIsNone(manifest["source_jar"]["expected_sha1"])
            self.assertFalse(manifest["official_source_authenticity_verified"])


if __name__ == "__main__":
    unittest.main()
