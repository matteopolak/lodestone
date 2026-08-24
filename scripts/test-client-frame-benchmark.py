#!/usr/bin/env python3
"""Unit tests for the live client benchmark runner and summarizer."""

import importlib.util
import pathlib
import sys
import unittest


RUNNER = pathlib.Path(__file__).with_name("client-frame-benchmark.py")
SPEC = importlib.util.spec_from_file_location("client_frame_benchmark", RUNNER)
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class SummaryTests(unittest.TestCase):
    def test_percentiles_budget_misses_and_nonempty_phase_means(self):
        summary = MODULE.summarize_rows(
            [
                {
                    "frame_interval_ms": "8.0",
                    "segment": "terrain.stationary",
                    "setup": "1.0",
                },
                {
                    "frame_interval_ms": "17.0",
                    "segment": "terrain.stationary",
                    "setup": "",
                },
                {
                    "frame_interval_ms": "34.0",
                    "segment": "terrain.stationary",
                    "setup": "3.0",
                },
            ]
        )
        self.assertEqual(summary["frames"], 3)
        self.assertEqual(summary["p50_ms"], 17.0)
        self.assertEqual(summary["p95_ms"], 34.0)
        self.assertEqual(summary["p99_ms"], 34.0)
        self.assertEqual(summary["over_16_67"], 2)
        self.assertEqual(summary["over_33_3"], 1)
        self.assertEqual(summary["phases_ms"]["setup"], 2.0)

    def test_nearest_rank_percentile_uses_the_observed_tail(self):
        self.assertEqual(MODULE.nearest_rank([1.0, 2.0, 8.0, 9.0], 0.95), 9.0)
        self.assertEqual(MODULE.nearest_rank([1.0, 2.0, 8.0, 9.0], 0.50), 2.0)

    def test_missing_complete_marker_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "completion marker"):
            MODULE.validate_run([], "log without marker", "terrain")

    def test_wrong_workload_metadata_is_rejected(self):
        rows = [
            {"segment": "showcase.stationary", "frame_interval_ms": "10"},
            {"segment": "showcase.moving", "frame_interval_ms": "11"},
        ]
        log = "benchmark window ready framebuffer_width=2560 framebuffer_height=1440\nbenchmark complete"
        with self.assertRaisesRegex(ValueError, "workload metadata"):
            MODULE.validate_run(rows, log, "terrain")

    def test_incomplete_measured_segments_are_rejected(self):
        rows = [{"segment": "terrain.stationary", "frame_interval_ms": "10"}]
        log = "benchmark window ready framebuffer_width=2560 framebuffer_height=1440\nbenchmark complete"
        with self.assertRaisesRegex(ValueError, "moving segment"):
            MODULE.validate_run(rows, log, "terrain")


if __name__ == "__main__":
    unittest.main()
