#!/usr/bin/env python3
"""Unit tests for the large client-benchmark world installer."""

import importlib.util
import pathlib
import tempfile
import unittest
import zipfile


INSTALLER = pathlib.Path(__file__).with_name("install-client-benchmark-world.py")
SPEC = importlib.util.spec_from_file_location("install_client_benchmark_world", INSTALLER)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class WorldInstallerTests(unittest.TestCase):
    def test_archive_hash_must_match_the_pinned_download(self):
        with tempfile.TemporaryDirectory() as temp_name:
            archive = pathlib.Path(temp_name) / "world.zip"
            archive.write_bytes(b"changed archive")

            with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
                MODULE.verify_archive_hash(archive, "0" * 64)

    def test_safe_extract_rejects_parent_traversal(self):
        with tempfile.TemporaryDirectory() as temp_name:
            temp = pathlib.Path(temp_name)
            archive = temp / "bad.zip"
            with zipfile.ZipFile(archive, "w") as handle:
                handle.writestr("../escaped.txt", "no")

            with self.assertRaisesRegex(ValueError, "unsafe ZIP member"):
                MODULE.safe_extract(archive, temp / "extract")
            self.assertFalse((temp / "escaped.txt").exists())

    def test_find_world_root_accepts_one_nested_java_world(self):
        with tempfile.TemporaryDirectory() as temp_name:
            root = pathlib.Path(temp_name)
            world = root / "download" / "Hermitcraft Season 10"
            (world / "region").mkdir(parents=True)
            (world / "level.dat").write_bytes(b"level")
            (world / "region" / "r.0.0.mca").write_bytes(b"region")

            self.assertEqual(MODULE.find_world_root(root), world)

    def test_find_world_root_rejects_archive_without_region_data(self):
        with tempfile.TemporaryDirectory() as temp_name:
            root = pathlib.Path(temp_name)
            (root / "level.dat").write_bytes(b"level")

            with self.assertRaisesRegex(ValueError, "one Java world root"):
                MODULE.find_world_root(root)

    def test_install_is_idempotent_after_complete_marker(self):
        with tempfile.TemporaryDirectory() as temp_name:
            root = pathlib.Path(temp_name)
            source = root / "source"
            destination = root / "installed"
            server_jar = root / "server.jar"
            (source / "region").mkdir(parents=True)
            (source / "level.dat").write_bytes(b"level")
            (source / "region" / "r.0.0.mca").write_bytes(b"region")
            server_jar.write_bytes(b"jar")

            self.assertTrue(
                MODULE.install_world_root(source, destination, server_jar, "abc123")
            )
            sentinel = destination / "keep-me"
            sentinel.write_text("unchanged", encoding="utf-8")
            self.assertFalse(
                MODULE.install_world_root(source, destination, server_jar, "abc123")
            )
            self.assertEqual(sentinel.read_text(encoding="utf-8"), "unchanged")


if __name__ == "__main__":
    unittest.main()
