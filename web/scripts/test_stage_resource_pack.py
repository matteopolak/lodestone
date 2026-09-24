#!/usr/bin/env python3
"""Controls for deterministic browser resource-pack staging."""

import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import stage_resource_pack


class ResourcePackStagingTest(unittest.TestCase):
    def make_source(self, path: Path) -> None:
        with zipfile.ZipFile(path, "w") as archive:
            archive.writestr("assets/minecraft/textures/block/stone.png", b"texture")
            archive.writestr("assets/minecraft/models/block/stone.json", b"model")
            archive.writestr("data/example/recipe/stone.json", b"recipe")
            archive.writestr("data/example/tags/item/stone.json", b"tag")
            archive.writestr("data/example/advancement/hidden.json", b"unused")
            archive.writestr("com/example/Server.class", b"class")
            archive.writestr("pack.png", b"icon")

    def test_keeps_the_complete_loader_surface_and_excludes_non_resources(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source.jar"
            self.make_source(source)
            first = stage_resource_pack.stage(source, root / "first")
            with zipfile.ZipFile(root / "first/client.jar") as archive:
                self.assertEqual(
                    archive.namelist(),
                    [
                        "assets/minecraft/models/block/stone.json",
                        "assets/minecraft/textures/block/stone.png",
                        "data/example/recipe/stone.json",
                        "data/example/tags/item/stone.json",
                        "pack.png",
                    ],
                )
            self.assertEqual(first["entries"], 5)

    def test_output_is_deterministic(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source.jar"
            self.make_source(source)
            stage_resource_pack.stage(source, root / "first")
            stage_resource_pack.stage(source, root / "second")
            self.assertEqual(
                (root / "first/client.jar").read_bytes(),
                (root / "second/client.jar").read_bytes(),
            )
            self.assertEqual(
                (root / "first/client.jar.manifest.json").read_bytes(),
                (root / "second/client.jar.manifest.json").read_bytes(),
            )

    def test_missing_or_corrupt_source_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaises(ValueError):
                stage_resource_pack.stage(root / "missing.jar", root / "out")
            corrupt = root / "corrupt.jar"
            corrupt.write_bytes(b"not a zip")
            with self.assertRaises(ValueError):
                stage_resource_pack.stage(corrupt, root / "out")


if __name__ == "__main__":
    unittest.main()
