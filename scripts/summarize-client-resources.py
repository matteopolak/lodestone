#!/usr/bin/env python3
"""Select observed resource intervals without estimating partial boundary work."""

import argparse
import hashlib
import json
import math
from pathlib import Path
import sys


def summarize_interval(report, start, end):
    if not (math.isfinite(start) and math.isfinite(end) and start < end):
        raise ValueError("Expected finite monotonic start < end")
    samples = report["samples"]
    timestamps = [sample["monotonic_seconds"] for sample in samples]
    if not timestamps or any(not math.isfinite(value) for value in timestamps):
        raise ValueError("Resource report has no finite sample timestamps")
    if any(left >= right for left, right in zip(timestamps, timestamps[1:])):
        raise ValueError("Resource timestamps must increase strictly")
    if start < timestamps[0] or end > timestamps[-1]:
        raise ValueError("Requested interval is outside the resource capture")
    points = [sample for sample in samples if start <= sample["monotonic_seconds"] <= end]
    intervals = []
    for previous, sample in zip(samples, samples[1:]):
        if previous["monotonic_seconds"] < start or sample["monotonic_seconds"] > end:
            continue
        duration = sample["monotonic_seconds"] - previous["monotonic_seconds"]
        if not math.isclose(duration, sample["interval_seconds"], rel_tol=1e-9, abs_tol=1e-6):
            raise ValueError("Sample interval does not match consecutive timestamps")
        intervals.append(sample)
    if not intervals:
        raise ValueError("Requested interval contains no whole observed sampling interval")

    def maximum(field):
        values = [sample[field] for sample in points if sample[field] is not None]
        return max(values) if values else None

    cpu = [sample for sample in intervals if sample["cpu_observed_delta_seconds"] is not None]
    cpu_seconds = sum(sample["cpu_observed_delta_seconds"] for sample in cpu)
    cpu_span = sum(sample["interval_seconds"] for sample in cpu)
    retired = [sample["retired_counters"] for sample in intervals if "retired_counters" in sample]

    def counter_sum(field):
        values = [sample[field] for sample in retired if sample[field] is not None]
        return sum(values) if values else None

    covered = sum(sample["interval_seconds"] for sample in intervals)
    return {
        "schema": 1,
        "association": report["association"],
        "root_pids": report["root_pids"],
        "root_births": report["root_births"],
        "interval": {
            "clock": "collector monotonic snapshot midpoints",
            "requested_start_seconds": start,
            "requested_end_seconds": end,
            "requested_duration_seconds": end - start,
            "observed_start_seconds": intervals[0]["monotonic_seconds"] - intervals[0]["interval_seconds"],
            "observed_end_seconds": intervals[-1]["monotonic_seconds"],
            "covered_seconds": covered,
            "coverage_fraction": covered / (end - start),
            "note": "Only whole consecutive sample intervals are included; boundary work is not prorated.",
        },
        "summary": {
            "point_samples": len(points),
            "whole_intervals": len(intervals),
            "complete_cpu_intervals": sum(sample["cpu_complete"] for sample in intervals),
            "cpu_observed_delta_seconds": cpu_seconds if cpu else None,
            "cpu_observed_span_seconds": cpu_span,
            "mean_observed_interval_cpu_percent": cpu_seconds / cpu_span * 100 if cpu_span else None,
            "peak_observed_interval_cpu_percent": max(
                (sample["cpu_observed_interval_percent"] for sample in cpu), default=None),
            "peak_complete_rss_bytes": maximum("rss_bytes"),
            "peak_observed_rss_bytes": maximum("rss_observed_bytes"),
            "truncated": report["summary"]["truncated"],
            "snapshot_errors": sum(sample["snapshot_error"] is not None for sample in points),
            "sampler_overhead_seconds": sum(sample["sampler_overhead_seconds"] for sample in points),
        },
        "retired_counters": {
            "enabled": report["retired_counters"]["enabled"],
            "method": report["retired_counters"]["method"],
            "observed_instructions": counter_sum("observed_instruction_delta"),
            "observed_cycles": counter_sum("observed_cycle_delta"),
            "complete_intervals": sum(sample["complete"] for sample in retired),
            "error_observations": sum(len(sample["errors"]) for sample in retired),
            "note": report["retired_counters"]["note"],
        },
        "rss_note": report["rss_note"],
        "cpu_note": report["cpu_note"],
        "gpu_memory": report["gpu_memory"],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--start", type=float, required=True, help="Collector monotonic seconds, not elapsed video time")
    parser.add_argument("--end", type=float, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        raw = args.report.read_bytes()
        summary = summarize_interval(json.loads(raw), args.start, args.end)
        summary["source"] = {"path": str(args.report.resolve()), "sha256": hashlib.sha256(raw).hexdigest()}
        with args.output.open("x", encoding="utf-8") as destination:
            json.dump(summary, destination, indent=2, allow_nan=False)
            destination.write("\n")
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"resource interval: {error}", file=sys.stderr)
        return 1
    print(args.output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
