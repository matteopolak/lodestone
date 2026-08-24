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
    @staticmethod
    def fullscreen_log(width=3024, height=1898):
        return (
            "selected hardware built-in display for fullscreen benchmark "
            "native_id=Some(1)\n"
            f"benchmark window ready framebuffer_width={width} "
            f"framebuffer_height={height} fullscreen=true\n"
            "benchmark complete"
        )

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

    def test_gpu_timestamp_samples_are_summarized_without_adding_passes(self):
        log = "\n".join(
            [
                "gpu: world_total=<no reading yet>, world=<no reading yet>",
                "gpu: world_total=0.10ms, world=0.40ms, first_person=0.20ms, hud_total=0.30ms",
                "gpu: world_total=0.20ms, world=0.60ms, first_person=0.30ms, hud_total=0.40ms",
                "gpu: world_total=0.30ms, world=0.80ms, first_person=0.40ms, hud_total=0.50ms",
            ]
        )

        summary = MODULE.summarize_gpu_log(log)
        self.assertEqual(summary["samples"], 3)
        self.assertEqual(summary["world"]["median_ms"], 0.6)
        self.assertEqual(summary["world"]["p95_ms"], 0.8)
        self.assertEqual(summary["first_person"]["median_ms"], 0.3)
        self.assertNotIn("combined", summary)

    def test_missing_complete_marker_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "completion marker"):
            MODULE.validate_run([], "log without marker", "terrain")

    def test_wrong_workload_metadata_is_rejected(self):
        rows = [
            {"segment": "showcase.stationary", "frame_interval_ms": "10"},
            {"segment": "showcase.moving", "frame_interval_ms": "11"},
        ]
        log = self.fullscreen_log()
        with self.assertRaisesRegex(ValueError, "workload metadata"):
            MODULE.validate_run(rows, log, "terrain")

    def test_incomplete_measured_segments_are_rejected(self):
        rows = [{"segment": "terrain.stationary", "frame_interval_ms": "10"}]
        log = self.fullscreen_log()
        with self.assertRaisesRegex(ValueError, "moving segment"):
            MODULE.validate_run(rows, log, "terrain")

    def test_hardware_builtin_fullscreen_framebuffer_is_parsed(self):
        rows = [
            {"segment": "terrain.stationary", "frame_interval_ms": "10"},
            {"segment": "terrain.moving", "frame_interval_ms": "11"},
        ]
        self.assertEqual(
            MODULE.validate_run(rows, self.fullscreen_log(), "terrain"),
            (3024, 1898),
        )

    def test_external_or_nonfullscreen_window_is_rejected(self):
        rows = [
            {"segment": "terrain.stationary", "frame_interval_ms": "10"},
            {"segment": "terrain.moving", "frame_interval_ms": "11"},
        ]
        external = (
            "benchmark window ready framebuffer_width=2560 "
            "framebuffer_height=1440 fullscreen=true\nbenchmark complete"
        )
        with self.assertRaisesRegex(ValueError, "hardware built-in"):
            MODULE.validate_run(rows, external, "terrain")

        windowed = self.fullscreen_log().replace("fullscreen=true", "fullscreen=false")
        with self.assertRaisesRegex(ValueError, "fullscreen"):
            MODULE.validate_run(rows, windowed, "terrain")


if __name__ == "__main__":
    unittest.main()
