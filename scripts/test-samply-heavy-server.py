#!/usr/bin/env python3
"""Focused controls for the bounded server Samply runner (stdlib only)."""

from __future__ import annotations

import importlib.util
import contextlib
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock


RUNNER = Path(__file__).with_name("samply-heavy-server.py")
SPEC = importlib.util.spec_from_file_location("samply_heavy_server", RUNNER)
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class SamplyHeavyServerTests(unittest.TestCase):
    def test_paths_match_samply_sidecar_spelling(self):
        paths = MODULE.paths_for(Path("/tmp/profiles"), "fixed")
        self.assertEqual(paths.capture.name, "heavy-server-entity-fixed.json.gz")
        self.assertEqual(paths.symbols.name, "heavy-server-entity-fixed.json.syms.json")

    def test_population_and_duration_bounds_reject_expansions(self):
        for arguments in (
            ["--scale", "3"],
            ["--wall-deadline-secs", "61"],
            ["--smoke", "--wall-deadline-secs", "16"],
        ):
            with self.subTest(arguments=arguments), contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit):
                    MODULE.parse_args(arguments)

    def test_emit_runs_the_real_binary_and_checks_its_scene_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            paths = MODULE.paths_for(root, "fixed")

            def fake_run(command, timeout):
                scene_path = Path(command[command.index("--emit-scene") + 1])
                scene_path.write_text(json.dumps({
                    "schema": 1,
                    "spec": {"scenario": "entity", "seed": 7, "scale": 1},
                    "scene_hash": "scene",
                }))

            with mock.patch.object(MODULE, "run_bounded", side_effect=fake_run) as run:
                scene = MODULE.emit_scene(Path("/tmp/heavy-scene-server"), paths, 7, 1, 9)
            self.assertEqual(scene["scene_hash"], "scene")
            self.assertEqual(run.call_args.args[0][0], "/tmp/heavy-scene-server")
            self.assertIn("--emit-scene", run.call_args.args[0])

    def test_runtime_control_rejects_missing_capture_sidecar_and_population(self):
        with tempfile.TemporaryDirectory() as directory:
            paths = MODULE.paths_for(Path(directory), "fixed")
            paths.runtime.write_text(json.dumps({
                "status": "complete", "scenario": "entity", "seed": 1, "scale": 1,
                "scenario_hash": "scene", "requested": {"entities_spawned": 1024},
                "consumed": {"entities_extracted": 1024},
            }) + "\n")
            record = MODULE.validate_runtime(paths, 1, 1, {"scene_hash": "scene"})
            self.assertEqual(record["status"], "complete")
            paths.runtime.write_text(paths.runtime.read_text().replace("1024", "0"))
            with self.assertRaisesRegex(RuntimeError, "population witness"):
                MODULE.validate_runtime(paths, 1, 1, {"scene_hash": "scene"})
            with self.assertRaisesRegex(RuntimeError, "capture"):
                MODULE.require_nonempty(paths.capture, "capture")
            paths.capture.write_bytes(b"capture")
            with self.assertRaisesRegex(RuntimeError, "sidecar"):
                MODULE.require_nonempty(paths.symbols, "sidecar")

    def test_capture_commands_profile_entity_ready_phase_with_two_deadlines(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            server = root / "heavy-scene-server"
            server.write_text("stub")
            args = MODULE.parse_args([
                "--server", str(server), "--output-dir", str(root / "profiles"),
                "--seed", "9", "--scale", "1", "--wall-deadline-secs", "12", "--run-id", "fixed",
            ])
            paths = MODULE.paths_for(args.output_dir, "fixed")
            scene = {"schema": 1, "spec": {"scenario": "entity", "seed": 9, "scale": 1}, "scene_hash": "scene"}
            record = {
                "status": "complete", "scenario": "entity", "seed": 9, "scale": 1,
                "scenario_hash": "scene", "requested": {"entities_spawned": 1024},
                "consumed": {"entities_extracted": 1024},
            }

            def fake_run(command, timeout):
                if command[0] == "samply":
                    paths.capture.write_bytes(b"capture")
                    paths.symbols.write_text("{}")
                    paths.runtime.write_text(json.dumps(record) + "\n")

            with mock.patch.object(MODULE.shutil, "which", return_value="/usr/bin/samply"), \
                 mock.patch.object(MODULE, "emit_scene", return_value=scene), \
                 mock.patch.object(MODULE, "run_bounded", side_effect=fake_run) as run:
                actual_paths, actual_record = MODULE.capture(args)
            self.assertEqual(actual_paths, paths)
            self.assertEqual(actual_record, record)
            samply_call = run.call_args.args[0]
            self.assertEqual(samply_call[:5], ["samply", "record", "--save-only", "--unstable-presymbolicate", "-o"])
            self.assertIn("--phase", samply_call)
            self.assertEqual(samply_call[samply_call.index("--phase") + 1], "ready")
            self.assertEqual(run.call_args.args[1], 12 + MODULE.PROFILER_GRACE_SECS)


if __name__ == "__main__":
    unittest.main()
