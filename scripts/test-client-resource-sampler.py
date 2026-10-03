#!/usr/bin/env python3
"""Synthetic controls for process lifetime, CPU intervals and RSS attribution."""

import importlib.util
import ctypes
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location("resources", Path(__file__).with_name("client-resource-sampler.py"))
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def process(pid, parent=0, birth="Mon Oct 2 10:00:00 2026", cpu=0, rss=4096):
    return {"pid": pid, "ppid": parent, "birth": birth, "cpu_seconds": cpu, "rss_bytes": rss}


def collector(snapshots, timestamps, **options):
    return MODULE.ProcessTreeSampler(
        [10], "Synthetic PID association only; no game measurements",
        snapshot=mock.Mock(side_effect=[(rows, 0) for rows in snapshots]),
        clock=mock.Mock(side_effect=timestamps), **options,
    )


class ResourceTests(unittest.TestCase):
    def test_retired_counter_abi_and_independent_interval_arithmetic(self):
        self.assertEqual(ctypes.sizeof(MODULE.RusageV4), 296)
        self.assertEqual(MODULE.RusageV4.instructions.offset, 248)
        self.assertEqual(MODULE.RusageV4.cycles.offset, 256)
        reader = mock.Mock(side_effect=[
            {"start_abstime": 7, "instructions": 100, "cycles": 200},
            {"start_abstime": 8, "instructions": 300, "cycles": 600},
            {"start_abstime": 7, "instructions": 201, "cycles": 402},
            {"start_abstime": 8, "instructions": 603, "cycles": 1206},
        ])
        rows = {10: process(10), 11: process(11, 10)}
        sampler = collector([rows, {key: dict(row) for key, row in rows.items()}],
                            [0, 0, 0, 1, 1, 1], retired_reader=reader)
        self.assertIsNone(sampler.take_sample()["retired_counters"]["observed_instruction_delta"])
        sample = sampler.take_sample()["retired_counters"]
        self.assertEqual(sample["observed_instruction_delta"], 404)
        self.assertEqual(sample["observed_cycle_delta"], 808)
        self.assertTrue(sample["complete"])
        self.assertEqual(sampler.report()["retired_counters"]["observed_instructions"], 404)
        self.assertEqual(sampler.report()["retired_counters"]["complete_intervals"], 1)

    def test_retired_lifetime_changes_missing_and_backwards_counters_are_gaps(self):
        reader = mock.Mock(side_effect=[
            {"start_abstime": 7, "instructions": 100, "cycles": 200},
            {"start_abstime": 9, "instructions": 300, "cycles": 600},
            OSError("unsupported"),
            {"start_abstime": 9, "instructions": 500, "cycles": 1000},
            {"start_abstime": 9, "instructions": 400, "cycles": 800},
        ])
        sampler = collector([{10: process(10)} for _ in range(5)],
                            [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4, 4], retired_reader=reader)
        for _ in range(5):
            sample = sampler.take_sample()["retired_counters"]
            self.assertIsNone(sample["observed_instruction_delta"])
            self.assertIsNone(sample["observed_cycle_delta"])
            self.assertFalse(sample["complete"])
        self.assertEqual(sampler.report()["retired_counters"]["error_observations"], 1)

    def test_retired_collection_is_opt_in_and_zero_counters_are_unavailable(self):
        sampler = collector([{10: process(10)}], [0, 0, 0])
        with mock.patch.object(MODULE, "mac_retired_counters") as reader:
            self.assertNotIn("retired_counters", sampler.take_sample())
            reader.assert_not_called()
        def unavailable(pid, flavor, pointer):
            return 0
        with mock.patch.object(MODULE, "mac_rusage_function", return_value=unavailable):
            with self.assertRaisesRegex(OSError, "unavailable"):
                MODULE.mac_retired_counters(10)

    def test_ps_shape_cpu_clock_and_rss_units_without_arguments(self):
        rows, rejected = MODULE.parse_snapshot(
            "10 1 Mon Oct 2 10:00:00 2026 01:02.50 8\n"
            "11 10 Mon Oct 2 10:00:01 2026 - -\n"
        )
        self.assertEqual(rejected, 0)
        self.assertEqual(rows[10]["cpu_seconds"], 62.5)
        self.assertEqual(rows[10]["rss_bytes"], 8192)
        self.assertIsNone(rows[11]["cpu_seconds"])
        self.assertIsNone(rows[11]["rss_bytes"])
        self.assertEqual(MODULE.parse_cpu_time("2-03:04:05.50"), 183845.5)
        self.assertNotIn("pcpu", MODULE.PS_COMMAND[-1])
        self.assertNotIn("args", MODULE.PS_COMMAND[-1])
        with self.assertRaisesRegex(ValueError, "65536"):
            MODULE.ProcessTreeSampler([10], "synthetic", max_samples=10000, max_processes=1024)

    def test_monotonic_midpoint_cpu_arithmetic_can_exceed_one_core(self):
        sampler = collector([
            {10: process(10, cpu=20), 11: process(11, 10, cpu=2)},
            {10: process(10, cpu=23), 11: process(11, 10, cpu=2.5)},
        ], [100.0, 100.2, 100.3, 102.0, 102.2, 102.3])
        first = sampler.take_sample()
        self.assertAlmostEqual(first["monotonic_seconds"], 100.1)
        self.assertIsNone(first["cpu_observed_interval_percent"])
        second = sampler.take_sample()
        self.assertAlmostEqual(second["interval_seconds"], 2)
        self.assertAlmostEqual(second["cpu_observed_delta_seconds"], 3.5)
        self.assertAlmostEqual(second["cpu_observed_interval_percent"], 175)
        self.assertAlmostEqual(second["sampler_overhead_seconds"], 0.3)
        self.assertTrue(second["cpu_complete"])
        self.assertEqual(sampler.report()["summary"]["peak_observed_interval_cpu_percent"], 175)
        self.assertEqual(sampler.report()["summary"]["mean_observed_interval_cpu_percent"], 175)

    def test_pid_reuse_excludes_root_and_does_not_subtract_child_lifetimes(self):
        sampler = collector([
            {10: process(10, cpu=9), 11: process(11, 10, cpu=10)},
            {10: process(10, cpu=10), 11: process(11, 10, birth="new child", cpu=0.2)},
            {10: process(10, birth="new root", cpu=500), 11: process(11, 10, cpu=500)},
        ], [0, 0, 0, 1, 1, 1, 2, 2, 2])
        sampler.take_sample()
        second = sampler.take_sample()
        self.assertEqual(second["cpu_observed_delta_seconds"], 1)
        self.assertFalse(second["cpu_complete"])
        self.assertEqual(second["reused_process_identities"], ["11@new child"])
        third = sampler.take_sample()
        self.assertEqual(third["reused_root_pids"], [10])
        self.assertIsNone(third["rss_bytes"])
        self.assertIsNone(third["cpu_observed_delta_seconds"])
        self.assertEqual(third["processes"], [])

    def test_missing_metrics_and_departing_child_are_not_reported_as_zero(self):
        sampler = collector([
            {10: process(10, cpu=1), 11: process(11, 10, cpu=2)},
            {10: process(10, cpu=None, rss=None)}, {},
        ], [0, 0, 0, 1, 1, 1, 2, 2, 2])
        sampler.take_sample()
        second = sampler.take_sample()
        self.assertIsNone(second["rss_bytes"])
        self.assertIsNone(second["rss_observed_bytes"])
        self.assertIsNone(second["cpu_observed_interval_percent"])
        self.assertEqual(len(second["missing_or_exited_identities"]), 1)
        third = sampler.take_sample()
        self.assertEqual(third["unavailable_root_pids"], [10])
        self.assertIsNone(third["cpu_observed_cumulative_seconds"])

    def test_shared_rss_sum_bounds_and_gpu_estimate_remain_distinct(self):
        snapshot = {10: process(10, rss=8192), 11: process(11, 10, rss=8192)}
        sampler = collector([snapshot], [0, 0, 0], max_samples=1)
        sample = sampler.take_sample({"bytes": 1234, "source": "synthetic application allocation counter"})
        self.assertEqual(sample["rss_bytes"], 16384)
        self.assertIn("shared pages", sampler.report()["rss_note"])
        self.assertIsNone(sample["gpu_memory"]["dedicated_vram_bytes"])
        self.assertIsNone(sample["gpu_memory"]["resident_gpu_bytes"])
        self.assertEqual(sample["application_gpu_allocation_estimate"]["bytes"], 1234)
        self.assertIsNone(sampler.take_sample())
        self.assertTrue(sampler.report()["summary"]["truncated"])
        bounded = collector([snapshot], [0, 0, 0], max_processes=1)
        partial = bounded.take_sample()
        self.assertTrue(partial["process_limit_truncated"])
        self.assertIsNone(partial["rss_bytes"])
        self.assertEqual(partial["rss_observed_bytes"], 8192)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "synthetic-resources.json"
            sampler.write_report(path)
            self.assertEqual(len(json.loads(path.read_text())["samples"]), 1)
            with self.assertRaises(FileExistsError):
                sampler.write_report(path)


if __name__ == "__main__":
    unittest.main()
