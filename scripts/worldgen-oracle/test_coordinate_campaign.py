"""Hermetic orchestration controls; no JVM or terrain-parity claim."""

import contextlib
import importlib.util
import io
import os
from pathlib import Path
import sys
import subprocess
import tempfile
import time
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("coordinate_campaign", Path(__file__).with_name("coordinate-campaign.py"))
CAMPAIGN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CAMPAIGN)


def report(rect, target=None, actual="state:2"):
    divergence = None
    count = (rect[1] - rect[0] + 1) * (rect[3] - rect[2] + 1)
    if target is not None:
        frame = (target[1] - rect[2]) * (rect[1] - rect[0] + 1) + target[0] - rect[0]
        divergence = {"frame": frame, "target": list(target), "component": "terrain",
                      "cell": {"kind": "block", "x": 13, "y": -25, "z": 11},
                      "expected": "state:1", "actual": actual, "bounds": "13..13:-25..-25:11..11"}
        count = frame + 1
    return {"format_version": 1, "status": "diverged" if divergence else "agreed",
            "rectangle": list(rect), "compared": count, "protocol": 776,
            "world_seed": 42, "stream_format": 7, "header_sha256": "a" * 64,
            "oracle_jar_sha256": "b" * 64, "oracle_source_sha256": "c" * 64,
            "divergence": divergence}


class CoordinateCampaignTests(unittest.TestCase):
    def args(self, path, *extra):
        return CAMPAIGN.parser().parse_args(["--output", str(path), "--seed", "0", *extra])

    def run_campaign(self, args, evaluator):
        with contextlib.redirect_stdout(io.StringIO()):
            return CAMPAIGN.campaign(args, CAMPAIGN.settings(args), evaluator)

    def test_generator_has_literal_witness_and_small_finite_domain(self):
        self.assertEqual(CAMPAIGN.rectangle(0, 0), [-1, 0, -1, 0])
        self.assertEqual(CAMPAIGN.mix(0), 0xE220A8397B1DCDAF)
        cases = [CAMPAIGN.rectangle(0, index) for index in range(8)]
        self.assertEqual({(rect[0], rect[2]) for rect in cases}, set(CAMPAIGN.ORIGINS))
        for rect in cases:
            CAMPAIGN.validate_rectangle(rect)
        self.assertEqual(CAMPAIGN.rectangle(1, 0), [0, 1, 0, 0])
        for seed in (1, (1 << 64) - 1):
            self.assertEqual({(rect[0], rect[2]) for rect in
                              (CAMPAIGN.rectangle(seed, index) for index in range(8))}, set(CAMPAIGN.ORIGINS))

    def test_resume_matches_uninterrupted_case_order_and_rejects_settings_change(self):
        with tempfile.TemporaryDirectory(prefix="coordinate-test-") as temporary:
            output = Path(temporary) / "campaign"
            calls = []

            def evaluate(rect, config, deadline):
                calls.append(list(rect))
                return report(rect)

            self.assertEqual(self.run_campaign(self.args(output, "--run-cases", "3"), evaluate), 0)
            self.assertEqual(len(calls), 3)
            changed = self.args(output, "--resume", "--seed", "1")
            with self.assertRaisesRegex(CAMPAIGN.CampaignError, "configuration"):
                self.run_campaign(changed, evaluate)
            self.assertEqual(len(calls), 3)
            self.assertEqual(self.run_campaign(self.args(output, "--resume"), evaluate), 0)
            self.assertEqual(calls, [CAMPAIGN.rectangle(0, index) for index in range(8)])
            checkpoint = CAMPAIGN.read_json(output / "checkpoint.json")
            self.assertEqual(checkpoint["status"], "complete")
            self.assertEqual(checkpoint["evaluations"], {"generated": 8, "shrink": 0, "replay": 0})
            self.assertFalse((output / "checkpoint.pending").exists())

    def test_shrink_preserves_class_after_frame_changes_and_confirms_fresh_replay(self):
        with tempfile.TemporaryDirectory(prefix="coordinate-test-") as temporary:
            output = Path(temporary) / "campaign"
            calls = []

            def evaluate(rect, config, deadline):
                calls.append(list(rect))
                return report(rect, [0, 0])

            self.assertEqual(self.run_campaign(self.args(output), evaluate), 1)
            self.assertEqual(calls, [[-1, 0, -1, 0], [0, 0, -1, 0], [0, 0, 0, 0], [0, 0, 0, 0]])
            replay = CAMPAIGN.read_json(output / "replay.json")
            self.assertEqual(replay["report"]["divergence"]["frame"], 0)
            self.assertEqual(replay["rectangle"], [0, 0, 0, 0])
            args = CAMPAIGN.parser().parse_args(["--replay", str(output / "replay.json"), "--seed", "0"])
            self.assertEqual(self.run_campaign(args, evaluate), 0)
            self.assertEqual(len(calls), 5)

    def test_different_identity_cannot_replace_original_failure(self):
        with tempfile.TemporaryDirectory(prefix="coordinate-test-") as temporary:
            output = Path(temporary) / "campaign"
            original = CAMPAIGN.rectangle(0, 0)

            def evaluate(rect, config, deadline):
                return report(rect, [0, 0], actual="state:2" if rect == original else "state:3")

            self.assertEqual(self.run_campaign(self.args(output), evaluate), 1)
            replay = CAMPAIGN.read_json(output / "replay.json")
            self.assertEqual(replay["rectangle"], original)
            self.assertEqual(len(replay["tried"]), 3)

    def test_failure_retains_index_and_unconfirmed_finding_resumes_replay(self):
        with tempfile.TemporaryDirectory(prefix="coordinate-test-") as temporary:
            output = Path(temporary) / "campaign"

            def unavailable(rect, config, deadline):
                raise CAMPAIGN.CampaignError("controlled unavailable oracle")

            with self.assertRaisesRegex(CAMPAIGN.CampaignError, "unavailable"):
                self.run_campaign(self.args(output), unavailable)
            state = CAMPAIGN.read_json(output / "checkpoint.json")
            self.assertEqual(state["next_case"], 0)
            self.assertEqual(state["evaluations"]["generated"], 0)
            calls = []

            def replay_loses_failure(rect, config, deadline):
                calls.append(list(rect))
                return report(rect) if len(calls) == 4 else report(rect, [0, 0])

            with self.assertRaisesRegex(CAMPAIGN.CampaignError, "fresh replay"):
                self.run_campaign(self.args(output, "--resume"), replay_loses_failure)
            state = CAMPAIGN.read_json(output / "checkpoint.json")
            self.assertEqual(state["next_case"], 0)
            self.assertEqual(state["last_failure"]["phase"], "replay")
            calls.clear()

            def restored(rect, config, deadline):
                calls.append(list(rect))
                return report(rect, [0, 0])

            self.assertEqual(self.run_campaign(self.args(output, "--resume"), restored), 1)
            self.assertEqual(calls, [[0, 0, 0, 0]])

    def test_invalid_configuration_and_replay_are_rejected_before_oracle(self):
        for flag, value in (("--cases", "0"), ("--cases", "9"), ("--seed", "-1"),
                            ("--shrink-attempts", "9"), ("--total-seconds", "0")):
            with self.assertRaises(CAMPAIGN.CampaignError):
                CAMPAIGN.settings(self.args(Path("unused"), flag, value))
        rect = [0, 1, 0, 1]
        invalid = report(rect, [1, 1])
        invalid["divergence"]["frame"] = 0
        with self.assertRaises(CAMPAIGN.CampaignError):
            CAMPAIGN.validate_report(invalid, rect)
        invalid = report(rect)
        invalid["compared"] = 3
        with self.assertRaises(CAMPAIGN.CampaignError):
            CAMPAIGN.validate_report(invalid, rect)
        with tempfile.TemporaryDirectory(prefix="coordinate-test-") as temporary:
            oversized = Path(temporary) / "large.json"
            oversized.write_bytes(b" " * (CAMPAIGN.MAX_JSON_BYTES + 1))
            with self.assertRaisesRegex(CAMPAIGN.CampaignError, "64 KiB"):
                CAMPAIGN.read_json(oversized)

    def test_runner_arguments_environment_and_missing_report(self):
        config = CAMPAIGN.settings(self.args(Path("unused")))
        config["oracle_jar_sha256"] = "b" * 64
        rect = [-1, 0, -1, 0]

        def subprocess_stub(command, env, deadline):
            self.assertEqual(command[-8:], ["--dimension", "overworld", "--cx", "-1", "0", "--cz", "-1", "0"])
            self.assertNotIn("LODESTONE_ORACLE_FROZEN_WORLD_ROOT", env)
            self.assertNotIn("LODESTONE_LARGE_PARITY_STREAM_DEFER_HEIGHTMAPS", env)
            self.assertEqual(env["LODESTONE_LARGE_PARITY_STREAM_BATCH_SIZE"], "1")
            observed = report(rect)
            observed["oracle_jar_sha256"] = config["oracle_jar_sha256"] or "b" * 64
            observed["oracle_source_sha256"] = config["source_sha256"]["scripts/worldgen-oracle/LargeParityOracle.java"]
            CAMPAIGN.atomic_json(Path(env["LODESTONE_COORDINATE_REPORT"]), observed)
            return 0, b""

        with patch.dict(os.environ, {"LODESTONE_ORACLE_FROZEN_WORLD_ROOT": "unsafe",
                                    "LODESTONE_LARGE_PARITY_STREAM_DEFER_HEIGHTMAPS": "1"}):
            with patch.object(CAMPAIGN, "bounded_process", subprocess_stub):
                self.assertEqual(CAMPAIGN.evaluate(rect, config, time.monotonic() + 10)["status"], "agreed")
        with patch.object(CAMPAIGN, "bounded_process", return_value=(2, b"controlled error")):
            with self.assertRaisesRegex(CAMPAIGN.CampaignError, "without a report"):
                CAMPAIGN.evaluate(rect, config, time.monotonic() + 10)

    def test_subprocess_deadline_and_retained_log_are_bounded(self):
        with self.assertRaisesRegex(CAMPAIGN.CampaignError, "deadline"):
            CAMPAIGN.bounded_process([sys.executable, "-c", "import time; time.sleep(30)"],
                                     os.environ.copy(), time.monotonic() + 0.1)
        status, log = CAMPAIGN.bounded_process(
            [sys.executable, "-c", "import sys; sys.stdout.write('x' * 200000)"],
            os.environ.copy(), time.monotonic() + 5)
        self.assertEqual(status, 0)
        self.assertEqual(len(log), CAMPAIGN.MAX_LOG_BYTES)

    def test_termination_reaps_the_separate_child_group(self):
        with tempfile.TemporaryDirectory(prefix="coordinate-term-") as temporary:
            marker = Path(temporary) / "child-pid"
            child = f"import os,time; from pathlib import Path; Path({str(marker)!r}).write_text(str(os.getpid())); time.sleep(30)"
            program = f"""
import importlib.util, os, sys, time
spec = importlib.util.spec_from_file_location('campaign', {str(Path(CAMPAIGN.__file__))!r})
campaign = importlib.util.module_from_spec(spec)
spec.loader.exec_module(campaign)
campaign.settings = lambda _: {{}}
campaign.campaign = lambda *_: campaign.bounded_process([sys.executable, '-c', {child!r}], os.environ.copy(), time.monotonic() + 30)
sys.argv = ['campaign', '--output', 'unused']
sys.exit(campaign.main())
"""
            with subprocess.Popen([sys.executable, "-c", program], stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE) as process:
                deadline = time.monotonic() + 5
                while not marker.exists() and process.poll() is None and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue(marker.exists(), "control child did not start")
                child_pid = int(marker.read_text())
                process.terminate()
                _, error = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 2, error.decode())
                self.assertIn(b"campaign interrupted", error)
                with self.assertRaises(ProcessLookupError):
                    os.kill(child_pid, 0)


if __name__ == "__main__":
    unittest.main()
