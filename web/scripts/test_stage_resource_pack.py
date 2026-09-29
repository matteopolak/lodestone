#!/usr/bin/env python3
"""Controls for deterministic browser resource-pack staging."""

from __future__ import annotations

import hashlib
import json
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import stage_resource_pack
import stage_resource_pack_parts


class ResourcePackStagingTest(unittest.TestCase):
    def make_source(self, path: Path) -> None:
        with zipfile.ZipFile(path, "w") as archive:
            archive.writestr("assets/minecraft/textures/block/stone.png", b"texture")
            archive.writestr("assets/minecraft/textures/block/base_only.png", b"forbidden")
            archive.writestr("assets/minecraft/textures/entity/player/wide/steve.png", b"default skin")
            archive.writestr("assets/minecraft/models/block/stone.json", b"model")
            archive.writestr("data/example/recipe/stone.json", b"recipe")
            archive.writestr("data/example/tags/item/stone.json", b"tag")
            archive.writestr("data/example/advancement/hidden.json", b"unused")
            archive.writestr("com/example/Server.class", b"class")
            archive.writestr("pack.png", b"icon")
            archive.writestr("version.json", b'{"id":"26.2","pack_version":{"resource_major":88}}')

    def make_visual(self, path: Path, metadata: dict | None = None) -> None:
        with zipfile.ZipFile(path, "w") as archive:
            archive.writestr("assets/minecraft/textures/block/stone.png", b"whimscape")
            archive.writestr("assets/minecraft/textures/block/new.png", b"new art")
            archive.writestr("assets/minecraft/models/block/stone.json", b"new model")
            archive.writestr("pack.mcmeta", json.dumps(metadata or {
                "pack": {"min_format": 84, "max_format": 98, "description": "old"}
            }))

    def test_keeps_the_complete_loader_surface_and_excludes_non_resources(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source.jar"
            visual = root / "visual.zip"
            self.make_source(source)
            self.make_visual(visual)
            first = stage_resource_pack.stage(source, visual, root / "first")
            with zipfile.ZipFile(root / "first/lodestone-resources.zip") as archive:
                self.assertEqual(
                    archive.namelist(),
                    [
                        "assets/minecraft/models/block/stone.json",
                        "assets/minecraft/textures/block/new.png",
                        "assets/minecraft/textures/block/stone.png",
                        "assets/minecraft/textures/entity/player/wide/steve.png",
                        "data/example/recipe/stone.json",
                        "data/example/tags/item/stone.json",
                        "pack.mcmeta",
                        "version.json",
                    ],
                )
                self.assertEqual(archive.read("assets/minecraft/textures/block/stone.png"), b"whimscape")
                self.assertNotIn("assets/minecraft/textures/block/base_only.png", archive.namelist())
                self.assertEqual(
                    archive.read("assets/minecraft/textures/entity/player/wide/steve.png"),
                    b"default skin",
                )
                self.assertEqual(archive.read("assets/minecraft/models/block/stone.json"), b"new model")
                pack = json.loads(archive.read("pack.mcmeta"))["pack"]
                self.assertIn("kavast", pack["description"])
                self.assertEqual(pack["pack_format"], 88)
                self.assertEqual(pack["supported_formats"], [88, 88])
                self.assertEqual((pack["min_format"], pack["max_format"]), (88, 88))
            self.assertEqual(first["entries"], 8)

    def test_applies_only_host_compatible_overlays_in_declared_order(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, visual = root / "source.jar", root / "visual.zip"
            self.make_source(source)
            self.make_visual(visual, {
                "pack": {"min_format": 84, "max_format": 98},
                "overlays": {"entries": [
                    {"directory": "first", "min_format": 84, "max_format": 88},
                    {"directory": "second", "min_format": 88, "max_format": 88},
                    {"directory": "future", "min_format": 89, "max_format": 98},
                ]},
            })
            with zipfile.ZipFile(visual, "a") as archive:
                archive.writestr("first/assets/minecraft/textures/block/stone.png", b"first")
                archive.writestr("second/assets/minecraft/textures/block/stone.png", b"second")
                archive.writestr("future/assets/minecraft/textures/block/stone.png", b"future")
                archive.writestr("future/assets/minecraft/textures/block/future.png", b"future")
            stage_resource_pack.stage(source, visual, root / "out")
            with zipfile.ZipFile(root / "out/lodestone-resources.zip") as archive:
                self.assertEqual(archive.read("assets/minecraft/textures/block/stone.png"), b"second")
                self.assertNotIn("assets/minecraft/textures/block/future.png", archive.namelist())
                self.assertNotIn("overlays", json.loads(archive.read("pack.mcmeta")))
                self.assertFalse(any(name.startswith(("first/", "second/", "future/"))
                                     for name in archive.namelist()))

    def test_rejects_incompatible_and_malformed_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source.jar"
            self.make_source(source)
            for index, (metadata, message) in enumerate([
                ({"pack": {"min_format": 89, "max_format": 98}}, "exclude host format 88"),
                ({"pack": {"min_format": 98, "max_format": 84}}, "range is reversed"),
                ({"pack": {"min_format": True, "max_format": 98}}, "unsigned 32-bit"),
                ({"pack": {"description": "no formats"}}, "unsigned 32-bit"),
                ({"pack": [], "description": "no object"}, "no pack object"),
            ]):
                with self.subTest(metadata=metadata):
                    visual = root / f"visual-{index}.zip"
                    self.make_visual(visual, metadata)
                    with self.assertRaisesRegex(ValueError, message):
                        stage_resource_pack.stage(source, visual, root / "out")
                    self.assertFalse((root / "out/lodestone-resources.zip").exists())

    def test_output_is_deterministic(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source.jar"
            visual = root / "visual.zip"
            self.make_source(source)
            self.make_visual(visual)
            stage_resource_pack.stage(source, visual, root / "first")
            stage_resource_pack.stage(source, visual, root / "second")
            self.assertEqual(
                (root / "first/lodestone-resources.zip").read_bytes(),
                (root / "second/lodestone-resources.zip").read_bytes(),
            )
            self.assertEqual(
                (root / "first/lodestone-resources.zip.manifest.json").read_bytes(),
                (root / "second/lodestone-resources.zip.manifest.json").read_bytes(),
            )

    def test_missing_or_corrupt_source_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaises(ValueError):
                stage_resource_pack.stage(root / "missing.jar", root / "missing.zip", root / "out")
            corrupt = root / "corrupt.jar"
            corrupt.write_bytes(b"not a zip")
            visual = root / "visual.zip"
            self.make_visual(visual)
            with self.assertRaises(ValueError):
                stage_resource_pack.stage(corrupt, visual, root / "out")
            empty_visual = root / "empty_visual.zip"
            with zipfile.ZipFile(empty_visual, "w") as archive:
                archive.writestr("pack.mcmeta", b'{"pack":{"description":"empty"}}')
            source = root / "source.jar"
            self.make_source(source)
            with self.assertRaisesRegex(ValueError, "no textures"):
                stage_resource_pack.stage(source, empty_visual, root / "out")


class ResourcePackPartsTest(unittest.TestCase):
    def test_parts_reassemble_the_exact_archive_with_deterministic_names(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "lodestone-resources.zip"
            payload = b"resource archive bytes"
            source.write_bytes(payload)
            first = stage_resource_pack_parts.stage(source, root / "first", 7)
            second = stage_resource_pack_parts.stage(source, root / "second", 7)
            self.assertEqual(first, second)
            self.assertEqual(first["asset"], "lodestone-resources.zip")
            self.assertEqual(first["total_bytes"], len(payload))
            self.assertEqual(first["sha256"], hashlib.sha256(payload).hexdigest())
            self.assertEqual([part["bytes"] for part in first["parts"]], [7, 7, 7, 1])
            self.assertEqual(b"".join((root / "first" / part["name"]).read_bytes()
                                      for part in first["parts"]), payload)
            for part in first["parts"]:
                data = (root / "first" / part["name"]).read_bytes()
                self.assertEqual(part["sha256"], hashlib.sha256(data).hexdigest())
                self.assertIn(part["sha256"], part["name"])
                self.assertNotIn("/", part["name"])

    def test_invalid_part_sizes_and_empty_archives_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "lodestone-resources.zip"
            source.write_bytes(b"")
            for part_bytes in (0, -1, stage_resource_pack_parts.PART_BYTES + 1):
                with self.assertRaisesRegex(ValueError, "part size"):
                    stage_resource_pack_parts.stage(source, root / "out", part_bytes)
            with self.assertRaisesRegex(ValueError, "empty"):
                stage_resource_pack_parts.stage(source, root / "out", 7)


if __name__ == "__main__":
    unittest.main()
