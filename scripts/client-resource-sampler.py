#!/usr/bin/env python3
"""Bounded process-tree RSS and observed interval CPU sampling for client runs."""

import argparse
from collections import defaultdict
import ctypes
from functools import lru_cache
import json
import math
import os
from pathlib import Path
import platform
import subprocess
import sys
import time


PS_COMMAND = ["ps", "-axo", "pid=,ppid=,lstart=,time=,rss="]
MAX_PROCESS_OBSERVATIONS = 65536
RSS_NOTE = "Summed process RSS includes shared pages in every process; it is not unique physical RAM or device memory."
CPU_NOTE = "Observed cumulative process CPU deltas; 100 percent is one core. New/missing/exited processes can leave unattributed CPU. ps time resolution can quantize short intervals."
RETIRED_NOTE = "Process-wide retired instructions and CPU cycles across all threads, not wall time, GPU work, or per-column costs. Only consecutive observations of identical process lifetimes are summed; missing/exited processes leave attribution gaps."


class RusageV4(ctypes.Structure):
    _fields_ = [("uuid", ctypes.c_uint8 * 16), ("prefix", ctypes.c_uint64 * 29),
                ("instructions", ctypes.c_uint64), ("cycles", ctypes.c_uint64),
                ("tail", ctypes.c_uint64 * 4)]


@lru_cache(maxsize=1)
def mac_rusage_function():
    if platform.system() != "Darwin":
        raise OSError("Retired counters require macOS proc_pid_rusage")
    library = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    function = library.proc_pid_rusage
    function.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
    function.restype = ctypes.c_int
    return function


def mac_retired_counters(pid):
    record = RusageV4()
    if mac_rusage_function()(pid, 4, ctypes.byref(record)) != 0:
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error))
    if not record.prefix[8] or not record.instructions or not record.cycles:
        raise OSError("Process retired counters are unavailable")
    return {"start_abstime": record.prefix[8], "instructions": record.instructions,
            "cycles": record.cycles}


def parse_cpu_time(text):
    if text in ("-", "?", "??"):
        return None
    try:
        days, clock = text.split("-", 1) if "-" in text else ("0", text)
        fields = clock.split(":")
        if len(fields) == 2:
            hours, minutes, seconds = "0", *fields
        elif len(fields) == 3:
            hours, minutes, seconds = fields
        else:
            return None
        value = int(days) * 86400 + int(hours) * 3600 + int(minutes) * 60 + float(seconds)
        return value if math.isfinite(value) and value >= 0 else None
    except ValueError:
        return None


def parse_snapshot(text):
    rows, rejected = {}, 0
    for line in text.splitlines():
        fields = line.split()
        if len(fields) != 9:
            rejected += 1
            continue
        try:
            pid, parent = int(fields[0]), int(fields[1])
        except ValueError:
            rejected += 1
            continue
        rss = int(fields[8]) * 1024 if fields[8].isdigit() else None
        rows[pid] = {
            "pid": pid, "ppid": parent, "birth": " ".join(fields[2:7]),
            "cpu_seconds": parse_cpu_time(fields[7]), "rss_bytes": rss,
        }
    return rows, rejected


def ps_snapshot():
    environment = os.environ.copy()
    environment["LC_ALL"] = "C"
    result = subprocess.run(PS_COMMAND, capture_output=True, text=True, env=environment, timeout=2, check=True)
    return parse_snapshot(result.stdout)


def identity(row):
    return f"{row['pid']}@{row['birth']}"


def gpu_memory_status():
    if platform.system() == "Darwin" and platform.machine() == "arm64":
        reason = "Unified memory: ps provides neither dedicated VRAM nor per-process resident GPU bytes."
    else:
        reason = "Unsupported by this ps collector: no dedicated or per-process resident VRAM instrument."
    return {"dedicated_vram_bytes": None, "resident_gpu_bytes": None, "reason": reason}


class ProcessTreeSampler:
    def __init__(self, root_pids, association, interval_seconds=1.0, max_samples=600, max_processes=64,
                 snapshot=ps_snapshot, clock=time.monotonic, retired_reader=None):
        if not root_pids or any(isinstance(pid, bool) or not isinstance(pid, int) or pid <= 0 for pid in root_pids):
            raise ValueError("Explicit positive root PIDs are required")
        if not isinstance(association, str) or not association.strip():
            raise ValueError("A nonblank PID association explanation is required")
        if not math.isfinite(interval_seconds) or interval_seconds < 0.1:
            raise ValueError("Sampling interval must be finite and at least 0.1 seconds")
        if any(isinstance(value, bool) or not isinstance(value, int) for value in (max_samples, max_processes)):
            raise ValueError("Sample and process bounds must be integers")
        if not 1 <= max_samples <= 10000 or not 1 <= max_processes <= 1024:
            raise ValueError("Bounds: 1..10000 samples and 1..1024 selected processes")
        if max_samples * max_processes > MAX_PROCESS_OBSERVATIONS:
            raise ValueError("Retained sample/process product exceeds 65536 observations")
        self.root_pids = sorted(set(root_pids))
        self.association = association
        self.interval = interval_seconds
        self.max_samples, self.max_processes = max_samples, max_processes
        self.snapshot, self.clock = snapshot, clock
        self.retired_reader = retired_reader
        self.root_births = None
        self.previous = {}
        self.samples = []
        self.next_due = None
        self.limit_reached = False

    def sample_if_due(self, gpu_allocation_estimate=None):
        if self.next_due is not None and self.clock() < self.next_due:
            return None
        return self.take_sample(gpu_allocation_estimate)

    def take_sample(self, gpu_allocation_estimate=None):
        if len(self.samples) >= self.max_samples:
            self.limit_reached = True
            return None
        if gpu_allocation_estimate is not None:
            estimate = gpu_allocation_estimate
            if not isinstance(estimate, dict) or isinstance(estimate.get("bytes"), bool):
                raise ValueError("GPU allocation estimate requires bytes and explicit source")
            if not isinstance(estimate.get("bytes"), int) or estimate["bytes"] < 0:
                raise ValueError("GPU allocation estimate bytes must be a nonnegative integer")
            if not isinstance(estimate.get("source"), str) or not estimate["source"].strip():
                raise ValueError("GPU allocation estimate source must be nonblank")
            gpu_allocation_estimate = {
                "kind": "application-tracked-allocation-estimate", "bytes": estimate["bytes"],
                "source": estimate["source"],
            }
        before = self.clock()
        error = None
        try:
            rows, rejected = self.snapshot()
        except (OSError, subprocess.SubprocessError) as failure:
            rows, rejected = {}, 0
            error = type(failure).__name__
        after = self.clock()
        timestamp = (before + after) / 2
        self.next_due = after + self.interval
        if self.root_births is None:
            self.root_births = {pid: rows[pid]["birth"] if pid in rows else None for pid in self.root_pids}
        children = defaultdict(list)
        for row in rows.values():
            children[row["ppid"]].append(row["pid"])
        unavailable_roots, reused_roots, queue = [], [], []
        for pid, birth in self.root_births.items():
            if birth is None or pid not in rows:
                unavailable_roots.append(pid)
            elif rows[pid]["birth"] != birth:
                reused_roots.append(pid)
            else:
                queue.append(pid)
        selected, visited = [], set()
        for pid in queue:
            if pid in visited:
                continue
            visited.add(pid)
            selected.append(rows[pid])
            queue.extend(sorted(children[pid]))
            if len(selected) >= self.max_processes:
                break
        process_truncated = any(pid not in visited for pid in queue)
        current = {identity(row): row for row in selected}
        previous_timestamp = self.samples[-1]["monotonic_seconds"] if self.samples else None
        interval = timestamp - previous_timestamp if previous_timestamp is not None else None
        previous_births = {row["pid"]: row["birth"] for row in self.previous.values()}
        reused = [identity(row) for row in selected
                  if row["pid"] in previous_births and previous_births[row["pid"]] != row["birth"]]
        delta, comparable, missing_cpu = 0.0, 0, []
        for key, row in current.items():
            old = self.previous.get(key)
            if old is None or old["cpu_seconds"] is None or row["cpu_seconds"] is None:
                missing_cpu.append(key)
                continue
            change = row["cpu_seconds"] - old["cpu_seconds"]
            if change < 0:
                missing_cpu.append(key)
                continue
            comparable += 1
            delta += change
        missing = sorted(set(self.previous) - set(current))
        known_rss = [row["rss_bytes"] for row in selected if row["rss_bytes"] is not None]
        known_cpu = [row["cpu_seconds"] for row in selected if row["cpu_seconds"] is not None]
        complete_tree = bool(selected) and not (unavailable_roots or reused_roots or process_truncated or rejected or error)
        rss_complete = complete_tree and len(known_rss) == len(selected)
        cpu_complete = complete_tree and comparable == len(selected) and not missing and interval is not None and interval > 0
        sample = {
            "monotonic_seconds": timestamp, "interval_seconds": interval,
            "snapshot_overhead_seconds": after - before,
            "rss_bytes": sum(known_rss) if rss_complete else None,
            "rss_observed_bytes": sum(known_rss) if known_rss else None,
            "rss_complete": rss_complete,
            "cpu_observed_cumulative_seconds": sum(known_cpu) if known_cpu else None,
            "cpu_observed_delta_seconds": delta if comparable else None,
            "cpu_observed_interval_percent": delta / interval * 100 if comparable and interval and interval > 0 else None,
            "cpu_complete": cpu_complete, "cpu_comparable_processes": comparable,
            "unavailable_root_pids": unavailable_roots, "reused_root_pids": reused_roots,
            "reused_process_identities": reused, "cpu_without_interval_baseline": missing_cpu,
            "missing_or_exited_identities": missing,
            "missing_attribution_note": "Disappeared/reparented processes have no final CPU/RSS observation; short-lived children between snapshots are unobserved.",
            "process_limit_truncated": process_truncated, "rejected_ps_rows": rejected,
            "snapshot_error": error, "processes": selected,
            "gpu_memory": gpu_memory_status(), "application_gpu_allocation_estimate": gpu_allocation_estimate,
        }
        if self.retired_reader is not None:
            sample["retired_counters"] = self.retired_interval(current, complete_tree, missing)
        self.samples.append(sample)
        self.previous = current
        sample["sampler_overhead_seconds"] = self.clock() - before
        return sample

    def retired_interval(self, current, complete_tree, missing):
        instructions, cycles, comparable = 0, 0, 0
        errors, without_baseline = {}, []
        for key, row in current.items():
            try:
                counter = self.retired_reader(row["pid"])
            except OSError as error:
                errors[key] = str(error)
                counter = None
            row["retired_counters"] = counter
            old = self.previous.get(key, {}).get("retired_counters")
            if counter is None or old is None or counter["start_abstime"] != old["start_abstime"]:
                without_baseline.append(key)
                continue
            instruction_delta = counter["instructions"] - old["instructions"]
            cycle_delta = counter["cycles"] - old["cycles"]
            if instruction_delta < 0 or cycle_delta < 0:
                without_baseline.append(key)
                continue
            instructions += instruction_delta
            cycles += cycle_delta
            comparable += 1
        return {
            "observed_instruction_delta": instructions if comparable else None,
            "observed_cycle_delta": cycles if comparable else None,
            "comparable_processes": comparable,
            "complete": complete_tree and comparable == len(current) and not missing,
            "without_interval_baseline": without_baseline, "errors": errors,
        }

    def report(self, stop_reason="caller_finished"):
        def peak(field):
            values = [sample[field] for sample in self.samples if sample[field] is not None]
            return max(values) if values else None

        deltas = [sample["cpu_observed_delta_seconds"] for sample in self.samples
                  if sample["cpu_observed_delta_seconds"] is not None]
        intervals = [sample for sample in self.samples if sample["cpu_observed_interval_percent"] is not None]
        observed_cpu_percent = (
            sum(sample["cpu_observed_delta_seconds"] for sample in intervals)
            / sum(sample["interval_seconds"] for sample in intervals) * 100
        ) if intervals else None
        retired = [sample["retired_counters"] for sample in self.samples if "retired_counters" in sample]
        instruction_deltas = [sample["observed_instruction_delta"] for sample in retired
                              if sample["observed_instruction_delta"] is not None]
        cycle_deltas = [sample["observed_cycle_delta"] for sample in retired
                        if sample["observed_cycle_delta"] is not None]
        return {
            "schema": 1, "association": self.association, "root_pids": self.root_pids,
            "root_births": self.root_births, "ps_fields": "pid,ppid,lstart,time,rss (no args or environment)",
            "rss_note": RSS_NOTE, "cpu_note": CPU_NOTE,
            "retired_counters": {
                "enabled": self.retired_reader is not None,
                "method": "proc_pid_rusage RUSAGE_INFO_V4" if self.retired_reader is mac_retired_counters else "injected reader" if self.retired_reader else None,
                "note": RETIRED_NOTE,
                "observed_instructions": sum(instruction_deltas) if instruction_deltas else None,
                "observed_cycles": sum(cycle_deltas) if cycle_deltas else None,
                "complete_intervals": sum(sample["complete"] for sample in retired),
                "error_observations": sum(len(sample["errors"]) for sample in retired),
            },
            "birth_note": "ps lstart has calendar-second resolution; PID reuse within the same birth second cannot be distinguished.",
            "bounds": {"max_samples": self.max_samples, "max_processes": self.max_processes,
                       "requested_interval_seconds": self.interval,
                       "max_process_observations": MAX_PROCESS_OBSERVATIONS},
            "summary": {
                "samples": len(self.samples), "stop_reason": stop_reason,
                "observed_span_seconds": self.samples[-1]["monotonic_seconds"] - self.samples[0]["monotonic_seconds"] if self.samples else None,
                "truncated": self.limit_reached or any(sample["process_limit_truncated"] for sample in self.samples),
                "peak_complete_rss_bytes": peak("rss_bytes"),
                "peak_observed_rss_bytes": peak("rss_observed_bytes"),
                "peak_observed_interval_cpu_percent": peak("cpu_observed_interval_percent"),
                "mean_observed_interval_cpu_percent": observed_cpu_percent,
                "cpu_observed_delta_seconds": sum(deltas) if deltas else None,
                "complete_cpu_intervals": sum(sample["cpu_complete"] for sample in self.samples),
                "sampler_overhead_seconds": sum(sample["sampler_overhead_seconds"] for sample in self.samples),
                "missing_or_exited_observations": sum(len(sample["missing_or_exited_identities"]) for sample in self.samples),
                "snapshot_errors": sum(sample["snapshot_error"] is not None for sample in self.samples),
            },
            "gpu_memory": gpu_memory_status(), "samples": self.samples,
        }

    def write_report(self, path, stop_reason="caller_finished"):
        with Path(path).open("x", encoding="utf-8") as destination:
            json.dump(self.report(stop_reason), destination, indent=2, allow_nan=False)
            destination.write("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root-pid", type=int, action="append", required=True)
    parser.add_argument("--association", required=True, help="How these PIDs were identified and which workload they include")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--duration", type=float, default=60)
    parser.add_argument("--interval", type=float, default=1)
    parser.add_argument("--max-samples", type=int, default=600)
    parser.add_argument("--max-processes", type=int, default=64)
    parser.add_argument("--retired-counters", action="store_true", help="Sample macOS process-wide instructions/cycles")
    args = parser.parse_args()
    try:
        if not math.isfinite(args.duration) or not 0 < args.duration <= 3600:
            raise ValueError("Duration must be positive and at most 3600 seconds")
        if args.output.exists():
            raise ValueError("Refusing to overwrite resource report")
        sampler = ProcessTreeSampler(args.root_pid, args.association, args.interval, args.max_samples, args.max_processes,
                                     retired_reader=mac_retired_counters if args.retired_counters else None)
        deadline = time.monotonic() + args.duration
        stop_reason = "duration_elapsed"
        while time.monotonic() < deadline:
            sample = sampler.take_sample()
            if sample is None:
                stop_reason = "sample_limit"
                break
            if not sample["processes"]:
                stop_reason = "roots_unavailable"
                break
            remaining = min(sampler.next_due, deadline) - time.monotonic()
            if remaining > 0:
                time.sleep(remaining)
        sampler.write_report(args.output, stop_reason)
    except (OSError, ValueError) as error:
        print(f"resource sampler: {error}", file=sys.stderr)
        return 1
    print(args.output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
