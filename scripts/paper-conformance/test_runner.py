#!/usr/bin/env python3
"""Focused controls for the Paper conformance contract scaffold.

Run with ``PYTHONDONTWRITEBYTECODE=1 python3 scripts/paper-conformance/test_runner.py``.
The suite is stdlib-only and creates its fixtures outside the checkout.
"""

from __future__ import annotations

import hashlib
import importlib.util
import json
import tempfile
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from io import StringIO


SCRIPT = Path(__file__).with_name("runner.py")
SPEC = importlib.util.spec_from_file_location("paper_conformance_runner", SCRIPT)
assert SPEC and SPEC.loader
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)

FAILURES: list[str] = []
PASSES = 0


def check(name: str, condition: bool, detail: str = "") -> None:
    global PASSES
    if condition:
        PASSES += 1
        print(f"  ok   {name}")
    else:
        FAILURES.append(f"{name}: {detail}")
        print(f"  FAIL {name}: {detail}")


def fixture(root: Path) -> Path:
    paper = root / "paper.jar"
    plugin = root / "plugin.jar"
    paper.write_bytes(b"operator supplied Paper fixture")
    plugin.write_bytes(b"operator supplied unmodified plugin fixture")
    scenario = root / "scenario.json"
    scenario.write_text(
        json.dumps(
            {
                "schema": 1,
                "id": "block-break-cancel",
                "seed": 730,
                "world": {"name": "paper-conformance", "spawn": [0, 65, 0]},
                "actions": [
                    {"kind": "break_block", "position": [1, 64, 0], "expected": "cancelled"}
                ],
                "controls": {
                    "no_listener": {"listener": "absent", "expected": "completed"}
                },
            }
        ),
        encoding="utf-8",
    )
    digest = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
    contract = root / "contract.json"
    contract.write_text(
        json.dumps(
            {
                "schema": 1,
                "jdk": {"java_home": str(root), "version": "25.0.3+9"},
                "paper": {
                    "jar": "paper.jar",
                    "sha256": digest(paper),
                    "version": "26.2",
                    "build": 121,
                },
                "plugins": [
                    {
                        "name": "operator-protection-plugin",
                        "domain": "world-editing",
                        "jar": "plugin.jar",
                        "sha256": digest(plugin),
                        "entrypoint": "example.protection.Plugin",
                        "unmodified": True,
                    }
                ],
                "scenario": "scenario.json",
                "commands": {"paper": ["{java}", "-jar", "{paper_jar}"], "lodestone": None},
            }
        ),
        encoding="utf-8",
    )
    return contract


def synthetic_observation(backend: str, *, no_listener: str = "completed") -> dict:
    return {
        "schema": 1,
        "kind": "observation",
        "backend": backend,
        "scenario": "block-break-cancel",
        "status": "complete",
        "evidence_kind": "synthetic",
        "source": f"independent-synthetic-{backend}",
        "observations": [
            {"id": "listener-present", "outcome": "cancelled"},
            {"id": "no-listener", "outcome": no_listener},
        ],
    }


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="paper-conformance-test-") as directory:
        contract_path = fixture(Path(directory))
        contract = runner.load_contract(contract_path)
        check("operator contract validates", contract["paper"]["build"] == 121)

        records = runner.blocked_records(contract)
        check("negative control is present", records[1]["id"] == "no-listener")
        check("negative control cannot pass alone", records[1]["status"] == "blocked")
        check(
            "missing prerequisites are named",
            [item["name"] for item in records[2]["missing_prerequisites"]]
            == [item["name"] for item in runner.REQUIRED_PREREQUISITES],
        )
        check("Paper is not run one-sided", records[3]["status"] == "not_run")

        bad_command = json.loads(contract_path.read_text(encoding="utf-8"))
        bad_command["commands"]["paper"] = ["sh", "-c", "java -jar paper.jar"]
        contract_path.write_text(json.dumps(bad_command), encoding="utf-8")
        try:
            runner.load_contract(contract_path)
        except runner.ContractError as error:
            check("shell command is rejected", "shell wrappers" in str(error))
        else:
            check("shell command is rejected", False, "validation unexpectedly passed")

        fixture(Path(directory))
        contract = runner.load_contract(contract_path)

        secret = runner.canonical_json(
            {"token": "do-not-print", "argv": ["--password=do-not-print", "plain"]}
        )
        check("structured and argv secrets are redacted", "do-not-print" not in secret)

        output = StringIO()
        runner.write_records(records, output)
        lines = output.getvalue().splitlines()
        check("results are deterministic NDJSON", len(lines) == 4 and all(json.loads(line) for line in lines))

        stdout = StringIO()
        stderr = StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            exit_code = runner.main(["run", "--contract", str(contract_path)])
        check("run exits blocked", exit_code == 2)
        check("run names the missing seam", "lodestone-paper-event-dispatch" in stderr.getvalue())
        check("run emits NDJSON", len(stdout.getvalue().splitlines()) == 4)

        paper_evidence = synthetic_observation("paper")
        lodestone_evidence = synthetic_observation("lodestone")
        comparison = runner.compare_evidence(
            contract, paper_evidence, lodestone_evidence, allow_synthetic=True
        )
        check("independent matching controls compare", comparison["status"] == "pass")
        check("matching comparison has no differences", comparison["differences"] == [])
        mismatched = synthetic_observation("lodestone", no_listener="cancelled")
        mismatch = runner.compare_evidence(
            contract, paper_evidence, mismatched, allow_synthetic=True
        )
        check("independent mismatch is machine-readable", mismatch["status"] == "fail")
        check(
            "mismatch names the no-listener control",
            any(
                item["id"] == "no-listener" and item["reason"] == "backend-mismatch"
                for item in mismatch["differences"]
            ),
        )

        paper_results = Path(directory) / "paper.ndjson"
        lodestone_results = Path(directory) / "lodestone.ndjson"
        paper_results.write_text(json.dumps(paper_evidence) + "\n", encoding="utf-8")
        lodestone_results.write_text(json.dumps(lodestone_evidence) + "\n", encoding="utf-8")
        stdout = StringIO()
        stderr = StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            exit_code = runner.main(
                [
                    "compare",
                    "--contract",
                    str(contract_path),
                    "--paper-results",
                    str(paper_results),
                    "--lodestone-results",
                    str(lodestone_results),
                ]
            )
        check("CLI rejects synthetic evidence", exit_code == 2)
        check("CLI names synthetic evidence", "synthetic" in stderr.getvalue())

        paper_results.write_text(
            json.dumps(paper_evidence) + "\n" + json.dumps({"kind": "log", "message": "extra"}) + "\n",
            encoding="utf-8",
        )
        with redirect_stdout(stdout), redirect_stderr(stderr):
            exit_code = runner.main(
                [
                    "compare",
                    "--contract",
                    str(contract_path),
                    "--paper-results",
                    str(paper_results),
                    "--lodestone-results",
                    str(lodestone_results),
                ]
            )
        check("auxiliary result records are rejected", exit_code == 2)

        bad = json.loads(contract_path.read_text(encoding="utf-8"))
        bad["plugins"][0]["unmodified"] = False
        contract_path.write_text(json.dumps(bad), encoding="utf-8")
        try:
            runner.load_contract(contract_path)
        except runner.ContractError as error:
            check("modified plugin is rejected", "unmodified must be true" in str(error))
        else:
            check("modified plugin is rejected", False, "validation unexpectedly passed")

    if FAILURES:
        print("\nFailures:")
        print("\n".join(f"- {failure}" for failure in FAILURES))
        return 1
    print(f"{PASSES} controls passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
