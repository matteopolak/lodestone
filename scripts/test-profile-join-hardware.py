#!/usr/bin/env python3
"""Small command-shape tests for profile-join-hardware.py."""

from __future__ import annotations

import subprocess
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts/profile-join-hardware.py"


class ProfileJoinHardwareTests(unittest.TestCase):
    def run_dry(self, workload: str) -> str:
        result = subprocess.run(
            [sys.executable, str(SCRIPT), workload, "--radius", "0",
             "--run-id", f"test-{workload}", "--dry-run"],
            cwd=ROOT, text=True, stdout=subprocess.PIPE, check=True,
        )
        return result.stdout

    def test_integrated_uses_join_profile_and_counter_template(self) -> None:
        output = self.run_dry("integrated")
        self.assertIn("--template CPU Counters", output)
        self.assertIn("/release/join_profile 4242 0 240", output)
        self.assertIn("process=join_profile", output)

    def test_client_keeps_single_threaded_fixture_bounded(self) -> None:
        output = self.run_dry("client")
        self.assertIn("--test client_join_mesh_profile", output)
        self.assertIn("--test-threads=1", output)
        self.assertIn("process=client_join_mesh_profile", output)


if __name__ == "__main__":
    unittest.main()
