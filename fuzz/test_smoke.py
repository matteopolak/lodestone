"""Exercise smoke-runner budget and failure propagation without compiling Rust."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class SmokeContract(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="lodestone-smoke-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        shutil.copyfile(Path(__file__).with_name("smoke.sh"), self.root / "smoke.sh")
        (self.root / "fuzz_targets").mkdir()
        for name in ("alpha", "beta"):
            (self.root / "fuzz_targets" / f"{name}.rs").touch()
        (self.root / "seeds" / "alpha").mkdir(parents=True)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        cargo = self.bin / "cargo"
        cargo.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, sys\n"
            "with open(os.environ['SMOKE_CALLS'], 'a') as log:\n"
            "    log.write(json.dumps(sys.argv[1:]) + '\\n')\n"
            "sys.exit(1 if os.environ.get('SMOKE_FAIL') in sys.argv[1:] else 0)\n"
        )
        cargo.chmod(0o755)
        (self.bin / "cargo-fuzz").symlink_to(cargo)
        self.env = {
            **os.environ,
            "PATH": f"{self.bin}:{os.environ['PATH']}",
            "SMOKE_CALLS": str(self.root / "calls.jsonl"),
        }
        self.env.pop("FUZZ_SMOKE_RUNS", None)

    def run_smoke(self, *args, **env):
        return subprocess.run(
            ["bash", str(self.root / "smoke.sh"), *args],
            env={**self.env, **env}, capture_output=True, text=True, timeout=10,
        )

    def calls(self):
        path = self.root / "calls.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def test_invalid_time_budgets_fail_before_cargo(self):
        for value in ("0", "-1", "abc", "1.5", "", "01", "2147483648"):
            with self.subTest(value=value):
                result = self.run_smoke(value)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn("seconds-per-target", result.stderr)
                self.assertEqual(self.calls(), [])

    def test_invalid_execution_budget_fails_before_cargo(self):
        result = self.run_smoke("1", FUZZ_SMOKE_RUNS="0")
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("FUZZ_SMOKE_RUNS", result.stderr)
        self.assertEqual(self.calls(), [])

    def test_each_run_has_both_budgets_and_read_only_seed_order(self):
        result = self.run_smoke("3", FUZZ_SMOKE_RUNS="17")
        self.assertEqual(result.returncode, 0, result.stderr)
        runs = [call for call in self.calls() if call[:2] == ["fuzz", "run"]]
        self.assertEqual(len(runs), 2)
        for call in runs:
            self.assertIn("-max_total_time=3", call)
            self.assertIn("-runs=17", call)
        self.assertLess(runs[0].index("corpus/alpha"), runs[0].index("seeds/alpha"))

    def test_target_failure_propagates_after_other_targets_run(self):
        result = self.run_smoke("1", SMOKE_FAIL="alpha")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("fuzz smoke failed: alpha", result.stdout)
        self.assertTrue(any("beta" in call for call in self.calls()))

    def test_all_excluded_is_a_failure_before_cargo(self):
        (self.root / "smoke-exclusions.txt").write_text("alpha\nbeta\n")
        result = self.run_smoke("1")
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("no gated targets", result.stderr)
        self.assertEqual(self.calls(), [])


if __name__ == "__main__":
    unittest.main()
