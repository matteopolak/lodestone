#!/usr/bin/env python3
"""Command-shape tests for the production worldgen hardware profiler."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts/profile-worldgen-hardware.sh"


class ProfileWorldgenHardwareTests(unittest.TestCase):
    def run_dry(self, *args: str) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as directory:
            env = os.environ.copy()
            env["LODESTONE_WORLDGEN_PROFILE_DRY_RUN"] = "1"
            env["LODESTONE_WORLDGEN_PROFILE_DIR"] = directory
            env["LODESTONE_WORLDGEN_PROFILE_RUN_ID"] = "script-test"
            return subprocess.run(
                [str(SCRIPT), *args], cwd=ROOT, env=env, text=True,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            )

    def run_dry_with_events(self) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as directory:
            env = os.environ.copy()
            env.update(
                LODESTONE_WORLDGEN_PROFILE_DRY_RUN="1",
                LODESTONE_WORLDGEN_PROFILE_DIR=directory,
                LODESTONE_WORLDGEN_PROFILE_RUN_ID="script-test-events",
                LODESTONE_WORLDGEN_XCTRACE_TEMPLATE="Cache Counters",
                LODESTONE_WORLDGEN_XCTRACE_EVENTS="Cycles,Instructions,L1D misses,DRAM bytes",
            )
            return subprocess.run(
                [str(SCRIPT), "42", "1", "1", "line"],
                cwd=ROOT,
                env=env,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )

    def test_dry_run_profiles_production_test_binary(self) -> None:
        result = self.run_dry("42", "8", "2", "line")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("strict_single_thread_worldgen", result.stdout)
        self.assertIn("strict_single_thread_production_worldgen", result.stdout)
        self.assertIn("metric=production_request", result.stdout)
        self.assertNotIn("bench_worldgen", result.stdout)
        self.assertIn("workers=1", result.stdout)
        self.assertIn("batch_size=2", result.stdout)
        self.assertIn("layout=line", result.stdout)

    def test_square_layout_requires_perfect_square(self) -> None:
        result = self.run_dry("42", "9", "3", "square")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("layout=square", result.stdout)

        result = self.run_dry("42", "8", "2", "square")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("perfect-square", result.stderr)

    def test_invalid_batch_is_rejected(self) -> None:
        result = self.run_dry("42", "8", "0", "line")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("batch size", result.stderr)

    def test_custom_event_names_are_forwarded_in_dry_run(self) -> None:
        result = self.run_dry_with_events()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("template=Cache Counters", result.stdout)
        self.assertIn("events=Cycles,Instructions,L1D misses,DRAM bytes", result.stdout)

    def test_counter_summary_keeps_extra_pmu_events_and_phase_controls(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            counters = root / "counters.xml"
            toc = root / "toc.xml"
            phase_log = root / "run.log"
            counters.write_text(
                """<?xml version=\"1.0\"?>
<trace-query-result>
  <node><schema name=\"counters-profile\"><col><mnemonic>counters-array</mnemonic></col></schema>
    <row>
      <process id=\"p\" fmt=\"strict_single_thread_worldgen (7)\"/>
      <pmc-events id=\"c1\" fmt=\"(10), (20), (30), (40)\">10 20 30 40</pmc-events>
      <tagged-backtrace id=\"s\"><backtrace id=\"b\"><frame name=\"fixed_kernel\"/></backtrace></tagged-backtrace>
    </row>
    <row><process ref=\"p\"/><pmc-events ref=\"c1\"/><tagged-backtrace ref=\"s\"/></row>
  </node>
</trace-query-result>
""",
                encoding="utf-8",
            )
            toc.write_text(
                """<trace-toc><run><data>
<table schema=\"counters-profile\" pmc-events=\"Cycles Instructions CacheMisses MemoryBytes\"/>
</data></run></trace-toc>""",
                encoding="utf-8",
            )
            phase_log.write_text(
                "STRICT_WORLDGEN metric=pmu_calibration phase=fixed_inline_never instructions=70 cycles=30\n"
                "STRICT_WORLDGEN metric=retained_target_control phase=immediate instructions=0 cycles=0\n",
                encoding="utf-8",
            )
            result = subprocess.run(
                [
                    "python3",
                    str(ROOT / "scripts/summarize-xctrace-counters.py"),
                    str(counters),
                    "--process",
                    "strict_single_thread_worldgen",
                    "--toc",
                    str(toc),
                    "--phase-log",
                    str(phase_log),
                ],
                cwd=ROOT,
                text=True,
                stdout=subprocess.PIPE,
                check=True,
            )
        self.assertIn("cycles=20 instructions=40", result.stdout)
        self.assertIn("cachemisses=60", result.stdout)
        self.assertIn("memorybytes=80", result.stdout)
        self.assertIn("metric=pmu_calibration phase=fixed_inline_never", result.stdout)
        self.assertIn(
            "metric=retained_target_control phase=immediate instructions=0 cycles=0",
            result.stdout,
        )


if __name__ == "__main__":
    unittest.main()
