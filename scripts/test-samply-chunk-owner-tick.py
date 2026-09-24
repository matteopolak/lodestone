#!/usr/bin/env python3
"""Focused controls for the bounded chunk-owner Samply runner (stdlib only)."""

from __future__ import annotations

import contextlib
import importlib.util
import io
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


RUNNER = Path(__file__).with_name("samply-chunk-owner-tick.py")
SPEC = importlib.util.spec_from_file_location("samply_chunk_owner_tick", RUNNER)
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


def healthy_witness(ticks: int = 128) -> str:
    return (
        f"scene=chunk-owner-mixed-8 ticks={ticks} owners=8 ambient_mobs=64 phases={ticks}/{ticks}/{ticks} "
        "scheduled_block_ticks=8 scheduled_fluid_ticks=8 block_entity_batches=1024 "
        "block_entity_effects=8 entity_effect_batches=16 entity_effects=64\n"
    )


class SamplyChunkOwnerTickTests(unittest.TestCase):
    def test_paths_match_samply_sidecar_spelling(self):
        paths = MODULE.paths_for(Path("/tmp/profiles"), "fixed")
        self.assertEqual(paths.capture.name, "chunk-owner-tick-fixed.json.gz")
        self.assertEqual(paths.symbols.name, "chunk-owner-tick-fixed.json.syms.json")
        self.assertEqual(paths.witness.name, "chunk-owner-tick-fixed.witness.txt")

    def test_finite_input_bounds_reject_expansions(self):
        for arguments in (
            ["--ticks", "0"],
            ["--ticks", str(MODULE.MAX_TICKS + 1)],
            ["--wall-deadline-secs", "61"],
        ):
            with self.subTest(arguments=arguments), contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit):
                    MODULE.parse_args(arguments)

    def test_witness_requires_every_production_work_class(self):
        witnesses = MODULE.parse_witness(healthy_witness(), 128)
        self.assertEqual(witnesses["entity_effects"], 64)
        for missing in MODULE.REQUIRED_COUNTERS:
            planted = re.sub(rf"{missing}=\d+", f"{missing}=0", healthy_witness())
            with self.subTest(missing=missing):
                with self.assertRaisesRegex(RuntimeError, missing):
                    MODULE.parse_witness(planted, 128)

    def test_witness_rejects_missing_phase_and_wrong_tick_controls(self):
        with self.assertRaisesRegex(RuntimeError, "phase"):
            MODULE.parse_witness(healthy_witness().replace("phases=128/128/128 ", ""), 128)
        with self.assertRaisesRegex(RuntimeError, "wrong tick"):
            MODULE.parse_witness(healthy_witness(), 127)
        with self.assertRaisesRegex(RuntimeError, "entity batch"):
            MODULE.parse_witness(healthy_witness().replace("entity_effects=64", "entity_effects=1"), 128)
        with self.assertRaisesRegex(RuntimeError, "population envelope"):
            MODULE.parse_witness(healthy_witness().replace("owners=8", "owners=1"), 128)
        with self.assertRaisesRegex(RuntimeError, "every owner"):
            MODULE.parse_witness(healthy_witness().replace("scheduled_fluid_ticks=8", "scheduled_fluid_ticks=1"), 128)

    def test_capture_runs_direct_witness_then_samply_with_two_deadlines(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            server = root / "chunk-owner-tick-profile"
            server.write_text("stub")
            args = MODULE.parse_args([
                "--server", str(server), "--output-dir", str(root / "profiles"),
                "--ticks", "128", "--wall-deadline-secs", "12", "--run-id", "fixed",
            ])
            paths = MODULE.paths_for(args.output_dir, "fixed")

            def fake_run(command, timeout):
                if command[0] == "samply":
                    paths.capture.write_bytes(b"capture")
                    paths.symbols.write_text("{}")
                    return subprocess.CompletedProcess(command, 0, "", "")
                return subprocess.CompletedProcess(command, 0, healthy_witness(), "")

            with mock.patch.object(MODULE.shutil, "which", return_value="/usr/bin/samply"), \
                 mock.patch.object(MODULE, "require_macos_samply_setup") as readiness, \
                 mock.patch.object(MODULE, "run_bounded", side_effect=fake_run) as run:
                actual_paths, witnesses = MODULE.capture(args)
            self.assertEqual(actual_paths, paths)
            self.assertEqual(witnesses["scheduled_fluid_ticks"], 8)
            readiness.assert_called_once_with("/usr/bin/samply")
            self.assertEqual(run.call_args_list[0].args[0], [str(server), "128"])
            self.assertEqual(run.call_args_list[0].args[1], 12)
            samply_command = run.call_args_list[1].args[0]
            self.assertEqual(samply_command[:5], ["samply", "record", "--save-only", "--unstable-presymbolicate", "-o"])
            self.assertEqual(run.call_args_list[1].args[1], 12 + MODULE.PROFILER_GRACE_SECS)
            self.assertEqual(paths.witness.read_text(), healthy_witness())

    def test_macos_signature_control_requires_debugger_entitlement(self):
        unsigned = subprocess.CompletedProcess(["codesign"], 0, "", "Executable=/tmp/samply\n")
        signed = subprocess.CompletedProcess(["codesign"], 0, "", "com.apple.security.cs.debugger\n")
        with mock.patch.object(MODULE.sys, "platform", "darwin"), \
             mock.patch.object(MODULE.subprocess, "run", return_value=unsigned):
            with self.assertRaisesRegex(RuntimeError, "samply setup"):
                MODULE.require_macos_samply_setup("/tmp/samply")
        with mock.patch.object(MODULE.sys, "platform", "darwin"), \
             mock.patch.object(MODULE.subprocess, "run", return_value=signed) as run:
            MODULE.require_macos_samply_setup("/tmp/samply")
        self.assertEqual(run.call_args.args[0], ["codesign", "-d", "--entitlements", ":-", "/tmp/samply"])


if __name__ == "__main__":
    unittest.main()
