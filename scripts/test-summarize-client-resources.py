#!/usr/bin/env python3
"""Independent arithmetic controls for selected resource windows."""

import importlib.util
from pathlib import Path
import unittest


SPEC = importlib.util.spec_from_file_location("interval", Path(__file__).with_name("summarize-client-resources.py"))
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def report():
    timestamps = [0, 1, 2, 4, 5, 6]
    deltas = [None, 30, 10, 3, 2, 40]
    samples = []
    for index, (timestamp, delta) in enumerate(zip(timestamps, deltas)):
        duration = timestamp - timestamps[index - 1] if index else None
        samples.append({
            "monotonic_seconds": timestamp, "interval_seconds": duration,
            "cpu_observed_delta_seconds": delta,
            "cpu_observed_interval_percent": delta / duration * 100 if duration else None,
            "cpu_complete": bool(index), "rss_bytes": 100 + index,
            "rss_observed_bytes": 100 + index, "sampler_overhead_seconds": 0.01,
            "snapshot_error": None,
            "retired_counters": {
                "observed_instruction_delta": delta * 101 if delta is not None else None,
                "observed_cycle_delta": delta * 43 if delta is not None else None,
                "complete": bool(index), "errors": {},
            },
        })
    return {
        "association": "synthetic arithmetic, not a game run", "root_pids": [7],
        "root_births": {"7": "synthetic"}, "samples": samples,
        "summary": {"truncated": False}, "rss_note": "shared pages",
        "cpu_note": "100 percent is one core", "gpu_memory": {"resident_gpu_bytes": None},
        "retired_counters": {"enabled": True, "method": "injected", "note": "CPU only"},
    }


class IntervalTests(unittest.TestCase):
    def test_cut_boundaries_exclude_both_startup_and_following_work(self):
        result = MODULE.summarize_interval(report(), 1.25, 5.75)
        self.assertEqual(result["summary"]["point_samples"], 3)
        self.assertEqual(result["summary"]["whole_intervals"], 2)
        self.assertEqual(result["summary"]["cpu_observed_delta_seconds"], 5)
        self.assertAlmostEqual(result["summary"]["mean_observed_interval_cpu_percent"], 500 / 3)
        self.assertEqual(result["summary"]["peak_observed_interval_cpu_percent"], 200)
        self.assertEqual(result["summary"]["peak_complete_rss_bytes"], 104)
        self.assertEqual(result["retired_counters"]["observed_instructions"], 505)
        self.assertEqual(result["retired_counters"]["observed_cycles"], 215)
        self.assertEqual(result["interval"]["covered_seconds"], 3)
        self.assertAlmostEqual(result["interval"]["coverage_fraction"], 2 / 3)
        self.assertIsNone(result["gpu_memory"]["resident_gpu_bytes"])

    def test_exact_bounds_keep_whole_intervals_but_not_previous_cpu(self):
        result = MODULE.summarize_interval(report(), 2, 5)
        self.assertEqual(result["summary"]["cpu_observed_delta_seconds"], 5)
        self.assertEqual(result["interval"]["coverage_fraction"], 1)

    def test_missing_work_is_not_fabricated_as_zero_or_complete(self):
        source = report()
        sample = source["samples"][3]
        sample["cpu_observed_delta_seconds"] = None
        sample["cpu_observed_interval_percent"] = None
        sample["cpu_complete"] = False
        sample["retired_counters"] = {
            "observed_instruction_delta": None, "observed_cycle_delta": None,
            "complete": False, "errors": {"7": "unavailable"},
        }
        result = MODULE.summarize_interval(source, 2, 5)
        self.assertEqual(result["summary"]["mean_observed_interval_cpu_percent"], 200)
        self.assertEqual(result["summary"]["cpu_observed_span_seconds"], 1)
        self.assertEqual(result["summary"]["complete_cpu_intervals"], 1)
        self.assertEqual(result["retired_counters"]["error_observations"], 1)

    def test_no_extrapolation_or_clock_mismatch(self):
        for start, end in [(-1, 2), (2, 7), (2, 2), (2.2, 3.9), (float("nan"), 5)]:
            with self.subTest(start=start, end=end), self.assertRaises(ValueError):
                MODULE.summarize_interval(report(), start, end)
        source = report()
        source["samples"][3]["interval_seconds"] = 20
        with self.assertRaisesRegex(ValueError, "consecutive"):
            MODULE.summarize_interval(source, 2, 5)


if __name__ == "__main__":
    unittest.main()
