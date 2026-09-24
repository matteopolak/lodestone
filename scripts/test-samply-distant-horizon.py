#!/usr/bin/env python3
"""Hermetic controls for the bounded distant-horizon Samply wrapper."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


RUNNER = Path(__file__).with_name("samply-distant-horizon.py")
SPEC = importlib.util.spec_from_file_location("samply_distant_horizon", RUNNER)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)

HEALTHY = "\n".join(
    (
        "horizon-profile phase=far-columns requested=256 shaped=256 full=0 solid_blocks=751 store_entries=256 store_evictions=0",
        "horizon-profile phase=horizon candidates=219 tiles_updated=18 tiles_skipped=201 cells_written=73728 atlas_cpu_bytes=2654208 atlas_gpu_bytes=2654208",
    )
)


class DistantHorizonSamplyTests(unittest.TestCase):
    def test_valid_witness_exposes_both_consumed_phases(self) -> None:
        far, horizon = MODULE.parse_witness(HEALTHY)
        self.assertEqual(far["shaped"], 256)
        self.assertEqual(horizon["cells_written"], 18 * 64 * 64)

    def test_detector_control_rejects_full_column_fallback(self) -> None:
        planted = HEALTHY.replace("shaped=256 full=0", "shaped=255 full=1")
        with self.assertRaisesRegex(RuntimeError, "full columns"):
            MODULE.parse_witness(planted)

    def test_rejects_noop_horizon_phase(self) -> None:
        planted = HEALTHY.replace("tiles_updated=18", "tiles_updated=0")
        with self.assertRaisesRegex(RuntimeError, "zero horizon tiles_updated"):
            MODULE.parse_witness(planted)

    def test_rejects_incoherent_written_cells(self) -> None:
        planted = HEALTHY.replace("cells_written=73728", "cells_written=73727")
        with self.assertRaisesRegex(RuntimeError, "does not match"):
            MODULE.parse_witness(planted)

    def test_capture_paths_are_stable_and_distinct(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = MODULE.paths_for(Path(temporary), "fixed")
            self.assertEqual(paths.capture.name, "distant-horizon-fixed.json.gz")
            self.assertEqual(paths.symbols.name, "distant-horizon-fixed.json.syms.json")
            self.assertEqual(paths.witness.name, "distant-horizon-fixed.witness.txt")
            self.assertEqual(len({paths.capture, paths.symbols, paths.witness}), 3)

    def test_capture_preflights_then_profiles_the_same_finite_binary(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / "horizon-profile"
            binary.write_text("stub")
            capture = root / "profiles/distant-horizon-fixed.json.gz"
            args = MODULE.parse_args(["--binary", str(binary), "--capture", str(capture)])
            paths = MODULE.paths_for_capture(capture)

            def fake_run(command, timeout):
                if command[0] == "samply":
                    paths.capture.write_bytes(b"capture")
                    paths.symbols.write_text("{}")
                    return subprocess.CompletedProcess(command, 0, "", "")
                return subprocess.CompletedProcess(command, 0, HEALTHY, "")

            with mock.patch.object(MODULE.shutil, "which", return_value="/usr/bin/samply"), \
                 mock.patch.object(MODULE, "require_macos_samply_setup"), \
                 mock.patch.object(MODULE, "run_bounded", side_effect=fake_run) as run:
                actual_paths, witnesses = MODULE.capture(args)
            self.assertEqual(actual_paths, paths)
            self.assertEqual(witnesses[0]["shaped"], 256)
            self.assertEqual(run.call_args_list[0].args, ([str(binary)], 60))
            self.assertEqual(run.call_args_list[1].args[0][-2:], ["--", str(binary)])
            self.assertEqual(run.call_args_list[1].args[1], 60 + MODULE.PROFILER_GRACE_SECS)
            self.assertEqual(paths.witness.read_text(), HEALTHY)

    def test_detector_control_stops_before_samply_on_full_column_fallback(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / "horizon-profile"
            binary.write_text("stub")
            capture = root / "profiles/distant-horizon-fixed.json.gz"
            args = MODULE.parse_args(["--binary", str(binary), "--capture", str(capture)])
            paths = MODULE.paths_for_capture(capture)
            planted = HEALTHY.replace("shaped=256 full=0", "shaped=255 full=1")

            with mock.patch.object(MODULE.shutil, "which", return_value="/usr/bin/samply"), \
                 mock.patch.object(MODULE, "require_macos_samply_setup"), \
                 mock.patch.object(
                     MODULE,
                     "run_bounded",
                     return_value=subprocess.CompletedProcess([str(binary)], 0, planted, ""),
                 ) as run:
                with self.assertRaisesRegex(RuntimeError, "full columns"):
                    MODULE.capture(args)
            self.assertEqual(run.call_count, 1, "the planted detector must stop before Samply")
            self.assertFalse(paths.capture.exists())
            self.assertFalse(paths.symbols.exists())
            self.assertFalse(paths.witness.exists())

    def test_capture_removes_partial_artifacts_after_failed_sidecar_check(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / "horizon-profile"
            binary.write_text("stub")
            capture = root / "profiles/distant-horizon-fixed.json.gz"
            args = MODULE.parse_args(["--binary", str(binary), "--capture", str(capture)])
            paths = MODULE.paths_for_capture(capture)

            def fake_run(command, timeout):
                if command[0] == "samply":
                    paths.capture.write_bytes(b"partial capture")
                    return subprocess.CompletedProcess(command, 0, "", "")
                return subprocess.CompletedProcess(command, 0, HEALTHY, "")

            with mock.patch.object(MODULE.shutil, "which", return_value="/usr/bin/samply"), \
                 mock.patch.object(MODULE, "require_macos_samply_setup"), \
                 mock.patch.object(MODULE, "run_bounded", side_effect=fake_run):
                with self.assertRaisesRegex(RuntimeError, "sidecar"):
                    MODULE.capture(args)
            self.assertFalse(paths.capture.exists())
            self.assertFalse(paths.symbols.exists())
            self.assertFalse(paths.witness.exists())

    def test_capture_path_rejects_non_samply_extension(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "must end"):
            MODULE.paths_for_capture(Path("profile.json"))


if __name__ == "__main__":
    unittest.main()
