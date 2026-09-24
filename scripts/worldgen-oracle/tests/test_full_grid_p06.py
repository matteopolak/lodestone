#!/usr/bin/env python3
"""Command and refusal controls for full-grid-p06.py.

These tests never invoke the Java oracle or Cargo.  The CLI plan test uses
``--dry-run``; the remaining controls exercise the coordinator's pure checks.
"""

from __future__ import annotations

import importlib.util
import io
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "full-grid-p06.py"
SPEC = importlib.util.spec_from_file_location("full_grid_p06", SCRIPT)
assert SPEC and SPEC.loader
DRIVER = importlib.util.module_from_spec(SPEC)
sys.modules["full_grid_p06"] = DRIVER
SPEC.loader.exec_module(DRIVER)


class FullGridP06Controls(unittest.TestCase):
    def test_command_construction_is_p06_and_end_is_serial_two_row(self):
        overworld = DRIVER.shard_commands("overworld", "read-a")
        nether = DRIVER.shard_commands("nether", "read-b")
        end = DRIVER.shard_commands("end", "read-a")
        self.assertEqual(len(overworld), 32)
        self.assertEqual(len(nether), 32)
        self.assertEqual(len(end), 501)
        for command in (*overworld, *nether, *end):
            self.assertIn("--raw-packet", command)
            self.assertIn("--resume", command)
            self.assertIn("--dimension", command)
            self.assertIn("/oracle-out/", " ".join(command))
        self.assertEqual(end[0][end[0].index("--cz") + 1 : end[0].index("--cz") + 3], ["-500", "-499"])
        self.assertEqual(end[-1][end[-1].index("--cz") + 1 : end[-1].index("--cz") + 3], ["500", "500"])

    def test_dry_run_prints_all_dimensions_without_starting_oracle(self):
        with tempfile.TemporaryDirectory(prefix="p06-driver-test-") as temporary:
            base = Path(temporary)
            output = io.StringIO()
            with redirect_stdout(output):
                result = DRIVER.main(
                    [
                        "--overworld-root",
                        str(base / "overworld"),
                        "--nether-root",
                        str(base / "nether"),
                        "--end-root",
                        str(base / "end"),
                        "--output-root",
                        str(base / "output"),
                        "--min-free-bytes",
                        "1",
                        "--min-ram-bytes",
                        "1400000000",
                        "--rust-command",
                        "fake-rust --manifest",
                        "--dry-run",
                    ]
                )
            plan = output.getvalue()
            self.assertEqual(result, 0)
            self.assertEqual(plan.count("--mode materialize"), 3)
            self.assertEqual(plan.count("# P06 Rust comparison:"), 3)
            self.assertIn("--dimension overworld", plan)
            self.assertIn("--dimension nether", plan)
            self.assertIn("--dimension end", plan)
            self.assertNotIn(".lodestone-full-grid-p06.json", plan)

    def test_paths_inside_repository_are_rejected(self):
        with self.assertRaisesRegex(DRIVER.DriverError, "outside the repository"):
            DRIVER.absolute_outside_repo(str(DRIVER.repo_root() / "oracle-output"), "--output-root", DRIVER.repo_root())

    def test_duplicate_or_legacy_world_roots_are_rejected(self):
        with tempfile.TemporaryDirectory(prefix="p06-driver-test-") as temporary:
            root = Path(temporary)
            self.assertEqual(
                DRIVER.main(
                    [
                        "--overworld-root",
                        str(root / "world"),
                        "--nether-root",
                        str(root / "world"),
                        "--end-root",
                        str(root / "end"),
                        "--output-root",
                        str(root / "output"),
                        "--min-free-bytes",
                        "1",
                        "--min-ram-bytes",
                        "1107296256",
                        "--dry-run",
                    ]
                ),
                2,
            )
            legacy = root / "legacy"
            legacy.mkdir()
            (legacy / "lodestone-large-parity-v4-nether.materialize").write_text("old\n", encoding="ascii")
            with self.assertRaisesRegex(DRIVER.DriverError, "legacy provenance"):
                DRIVER.ensure_world_root(legacy, "nether")

    def test_resource_preflight_rejects_ram_and_oversized_batch(self):
        dimensions = [DRIVER.Dimension("overworld", Path("/private/tmp/ow"))]
        with self.assertRaisesRegex(DRIVER.DriverError, "batch-size"):
            DRIVER.preflight_resources(dimensions, Path("/private/tmp/out"), 2049, 1, 2 * DRIVER.GIB, ram_bytes=8 * DRIVER.GIB)
        with self.assertRaisesRegex(DRIVER.DriverError, "RAM preflight"):
            DRIVER.preflight_resources(dimensions, Path("/private/tmp/out"), 256, 1, 4 * DRIVER.GIB, ram_bytes=1)

    def test_incompatible_existing_output_is_never_overwritten(self):
        with tempfile.TemporaryDirectory(prefix="p06-driver-test-") as temporary:
            output = Path(temporary) / "output"
            output.mkdir()
            (output / "foreign.lwp").write_bytes(b"not a P06 manifest")
            dimensions = [DRIVER.Dimension(name, Path(temporary) / name) for name in DRIVER.DIMENSION_NAMES]
            with self.assertRaisesRegex(DRIVER.DriverError, "unknown shards"):
                DRIVER.load_or_create_state(output, dimensions, 256, 4, dry_run=False)


if __name__ == "__main__":
    unittest.main()
