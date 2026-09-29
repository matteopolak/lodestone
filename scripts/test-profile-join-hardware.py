#!/usr/bin/env python3
"""Command-shape and captured-counter checks for profile-join-hardware.py."""

from __future__ import annotations

import copy
import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
import xml.etree.ElementTree as ET


ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts/profile-join-hardware.py"
SPEC = importlib.util.spec_from_file_location("profile_join_hardware", SCRIPT)
HARDWARE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HARDWARE)
WITNESS = ROOT / "scripts/fixtures/xctrace-guided-cycles.xml"


class ProfileJoinHardwareTests(unittest.TestCase):
    def guided(self, root=None, pid=25150):
        if root is None:
            return HARDWARE.guided_counter_lines(WITNESS, pid)
        with tempfile.TemporaryDirectory() as directory:
            xml = Path(directory) / "metrics.xml"
            ET.ElementTree(root).write(xml, encoding="utf-8")
            return HARDWARE.guided_counter_lines(xml, pid)

    def test_real_guided_witness_reports_cycles_only(self) -> None:
        output = "\n".join(self.guided())
        self.assertIn("samples=2 cycles=1643138 instructions=unavailable ipc=unavailable", output)
        self.assertIn("observed_interval_ns=912000000..914000000", output)

    def test_guided_resolves_xml_references(self) -> None:
        root = ET.parse(WITNESS).getroot()
        rows = root.findall("node/row")
        rows[0].find("process").set("id", "process1")
        for row in rows[1:]:
            row[2] = ET.Element("process", ref="process1")
        self.assertIn("cycles=1643138", self.guided(root)[0])

    def test_guided_coarse_buckets_are_a_cross_check_not_extra_cycles(self) -> None:
        root = ET.parse(WITNESS).getroot()
        coarse = copy.deepcopy(root.findall("node/row")[1])
        coarse.find("start-time").text = "910000000"
        coarse.find("duration").text = "10000000"
        coarse.find("uint64").text = "1643138"
        coarse.find("boolean").text = "0"
        root.find("node").append(coarse)
        self.assertIn("samples=2 cycles=1643138", self.guided(root)[0])
        coarse.find("uint64").text = "1643139"
        with self.assertRaisesRegex(ValueError, "totals disagree"):
            self.guided(root)

    def test_guided_duplicate_or_overlapping_buckets_fail(self) -> None:
        for start in ("912000000", "912500000"):
            root = ET.parse(WITNESS).getroot()
            duplicate = copy.deepcopy(root.findall("node/row")[1])
            duplicate.find("start-time").text = start
            root.find("node").append(duplicate)
            with self.assertRaisesRegex(ValueError, "duplicate or overlapping"):
                self.guided(root)

    def test_guided_missing_or_mislabelled_cycles_fail(self) -> None:
        root = ET.parse(WITNESS).getroot()
        for row in root.findall("node/row"):
            row.find("string").text = "useful"
        with self.assertRaisesRegex(ValueError, "no precise cycle"):
            self.guided(root)
        with self.assertRaisesRegex(ValueError, "no precise cycle"):
            self.guided(pid=25151)
        root = ET.parse(WITNESS).getroot()
        root.findall("node/row")[0].find("string").text = "cycle"
        with self.assertRaisesRegex(ValueError, "contains a ratio"):
            self.guided(root)

    def test_guided_broken_reference_fails(self) -> None:
        root = ET.parse(WITNESS).getroot()
        root.findall("node/row")[1][2] = ET.Element("process", ref="missing")
        with self.assertRaisesRegex(ValueError, "invalid XML reference"):
            self.guided(root)

    def test_guided_rejects_unknown_schema_or_types(self) -> None:
        root = ET.parse(WITNESS).getroot()
        root.find("node/schema").set("name", "CounterMetricByThread")
        with self.assertRaisesRegex(ValueError, "expected MetricAggregationForProcess"):
            self.guided(root)
        root = ET.parse(WITNESS).getroot()
        root.find("node/schema/col/engineering-type").text = "uint64"
        with self.assertRaisesRegex(ValueError, "engineering types"):
            self.guided(root)
        root = ET.parse(WITNESS).getroot()
        root.findall("node/row")[1].find("uint64").tag = "fixed-decimal"
        with self.assertRaisesRegex(ValueError, "row types"):
            self.guided(root)

    def run_dry(self, workload: str) -> str:
        result = subprocess.run(
            [sys.executable, str(SCRIPT), workload, "--radius", "0",
             "--run-id", f"test-{workload}", "--dry-run"],
            cwd=ROOT, text=True, stdout=subprocess.PIPE, check=True,
        )
        return result.stdout

    def test_integrated_uses_join_profile_and_counter_template(self) -> None:
        output = self.run_dry("integrated")
        self.assertIn("--template CPU Counters", output)
        self.assertIn("/release/join_profile 4242 0 240", output)
        self.assertIn("process=join_profile", output)

    def test_client_keeps_single_threaded_fixture_bounded(self) -> None:
        output = self.run_dry("client")
        self.assertIn("--test client_join_mesh_profile", output)
        self.assertIn("--test-threads=1", output)
        self.assertIn("process=client_join_mesh_profile", output)


if __name__ == "__main__":
    unittest.main()
