"""Hermetic contract tests for the bounded tick-corpus recorder."""

from __future__ import annotations

import importlib.util
import pathlib
import sys
import unittest


SCRIPT = pathlib.Path(__file__).parents[1] / "capture-differential-ticks.py"
SPEC = importlib.util.spec_from_file_location("capture_differential_ticks", SCRIPT)
assert SPEC and SPEC.loader
RECORDER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = RECORDER
SPEC.loader.exec_module(RECORDER)


class ForceLoadContractTests(unittest.TestCase):
    def test_capture_owns_exact_action_and_probe_chunks(self) -> None:
        plan = {
            "region": [
                {"pos": [-1, 200, -1], "candidates": ["minecraft:air"]},
                {"pos": [17, 200, 33], "candidates": ["minecraft:stone"]},
            ],
            "steps": [
                {"action": {"pos": [16, 200, 32]}},
                {"action": {"pos": [17, 200, 33]}},
            ],
        }
        self.assertEqual(
            RECORDER.chunks_to_force_load(plan),
            [(-16, -16), (16, 32)],
        )

    def test_force_load_cleanup_is_reverse_and_scoped_to_added_chunks(self) -> None:
        class Rcon:
            def __init__(self):
                self.commands = []

            def command(self, command):
                self.commands.append(command)
                return ""

        plan = {
            "region": [{"pos": [0, 200, 0], "candidates": ["minecraft:air"]}],
            "steps": [{"action": {"pos": [32, 200, 0]}}],
        }
        rcon = Rcon()
        loaded = RECORDER.force_load(rcon, plan)
        RECORDER.release_force_loads(rcon, loaded)
        self.assertEqual(
            rcon.commands,
            [
                "forceload add 0 0",
                "forceload add 32 0",
                "forceload remove 32 0",
                "forceload remove 0 0",
            ],
        )


if __name__ == "__main__":
    unittest.main()
