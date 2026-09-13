#!/usr/bin/env python3
"""Focused controls for the Paper conformance contract scaffold.

Run with ``PYTHONDONTWRITEBYTECODE=1 python3 scripts/paper-conformance/test_runner.py``.
The suite is stdlib-only and creates its fixtures outside the checkout.
"""

from __future__ import annotations

import hashlib
import importlib.util
import json
import subprocess
import sys
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
    target = root / "target.jar"
    paper.write_bytes(b"operator supplied Paper fixture")
    plugin.write_bytes(b"operator supplied unmodified plugin fixture")
    target.write_bytes(b"operator supplied maintained target fixture")
    java = root / "bin" / "java"
    java.parent.mkdir(exist_ok=True)
    java.write_text(
        "#!" + sys.executable + "\n"
        "import sys\n"
        "import json\n"
        "from pathlib import Path\n"
        "if '-version' in sys.argv:\n"
        "    print('openjdk version \\\"25.0.3\\\"')\n"
        "else:\n"
        "    if '--mutate-staged' in sys.argv:\n"
        "        Path('plugins/target.jar').write_bytes(b'mutated staged target')\n"
        "    print(json.dumps({\"schema\": 1, \"kind\": \"observation\", "
        "\"backend\": \"paper\", \"scenario\": \"block-break-cancel\", "
        "\"status\": \"complete\", \"evidence_kind\": "
        "(\"external\" if '--external' in sys.argv else \"synthetic\"), "
        "\"source\": \"operator-test-paper-driver\", \"observations\": "
        "[{\"id\": \"listener-present\", \"outcome\": \"cancelled\"}, "
        "{\"id\": \"no-listener\", \"outcome\": \"completed\"}], "
        "\"enabled_plugins\": [\"operator-protection-plugin\"]}))\n",
        encoding="utf-8",
    )
    java.chmod(0o755)
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
                        "name": "LodestonePaperConformance",
                        "domain": "world-editing",
                        "jar": "plugin.jar",
                        "sha256": digest(plugin),
                        "entrypoint": "io.lodestone.conformance.PaperConformancePlugin",
                        "unmodified": True,
                    },
                    {
                        "name": "operator-protection-plugin",
                        "domain": "world-editing",
                        "jar": "target.jar",
                        "sha256": digest(target),
                        "entrypoint": "example.protection.Plugin",
                        "unmodified": True,
                    }
                ],
                "target_plugins": ["operator-protection-plugin"],
                "driver": {
                    "plugin": "LodestonePaperConformance",
                    "entrypoint": "io.lodestone.conformance.PaperConformancePlugin",
                    "protocol": "paper-observation-v1",
                    "controls": ["listener-present", "no-listener"],
                    "requires_real_block_break": True,
                    "build": {
                        "jdk_release": 25,
                        "paper_api": "{paper_jar}",
                        "network": False,
                    },
                },
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
    driver_source = Path(__file__).with_name("driver") / "src/io/lodestone/conformance/PaperConformancePlugin.java"
    driver_build = Path(__file__).with_name("driver") / "build.sh"
    source_text = driver_source.read_text(encoding="utf-8")
    build_text = driver_build.read_text(encoding="utf-8")
    check(
        "driver source uses real break listener",
        all(
            marker in source_text
            for marker in (
                "BlockBreakEvent",
                "EventPriority.MONITOR",
                "event.isCancelled()",
                "HandlerList.unregisterAll(plugin)",
                "target.getType() != Material.AIR",
                "enabled_plugins",
                "Bukkit.getPluginManager().getPlugins()",
            )
        ),
    )
    check("driver does not decide target cancellation", "event.setCancelled" not in source_text)
    check(
        "driver build is offline and pinned",
        "--release 25" in build_text
        and "-cp \"$paper_jar\"" in build_text
        and all(command not in build_text for command in ("curl", "wget", "mvn", "gradle")),
    )
    check("driver build recipe is valid shell", subprocess.run(["sh", "-n", str(driver_build)]).returncode == 0)
    with tempfile.TemporaryDirectory(prefix="paper-conformance-test-") as directory:
        contract_path = fixture(Path(directory))
        contract = runner.load_contract(contract_path)
        check("operator contract validates", contract["paper"]["build"] == 121)

        external = runner._normalize_observation(
            {
                "schema": 1,
                "kind": "observation",
                "backend": "paper",
                "scenario": "block-break-cancel",
                "status": "complete",
                "evidence_kind": "external",
                "source": "operator-paper-driver",
                "observations": [
                    {"id": "listener-present", "outcome": "cancelled"},
                    {"id": "no-listener", "outcome": "completed"},
                ],
                "enabled_plugins": ["operator-protection-plugin"],
            },
            backend="paper",
            scenario_id="block-break-cancel",
            allow_synthetic=False,
            required_plugins=("operator-protection-plugin",),
        )
        check("external Paper observation shape validates", external["backend"] == "paper")

        missing_target = dict(external)
        missing_target["schema"] = 1
        missing_target["status"] = "complete"
        missing_target["enabled_plugins"] = []
        try:
            runner._normalize_observation(
                missing_target,
                backend="paper",
                scenario_id="block-break-cancel",
                allow_synthetic=False,
                required_plugins=("operator-protection-plugin",),
            )
        except runner.ContractError as error:
            check("Paper must report enabled target plugins", "missing enabled target" in str(error))
        else:
            check("Paper must report enabled target plugins", False, "missing target unexpectedly passed")

        bad_hash = json.loads(contract_path.read_text(encoding="utf-8"))
        bad_hash["paper"]["sha256"] = "0" * 64
        contract_path.write_text(json.dumps(bad_hash), encoding="utf-8")
        try:
            runner.load_contract(contract_path)
        except runner.ContractError as error:
            check("Paper hash is verified before launch", "does not match" in str(error))
        else:
            check("Paper hash is verified before launch", False, "validation unexpectedly passed")
        fixture(Path(directory))
        contract = runner.load_contract(contract_path)

        bad_target = json.loads(contract_path.read_text(encoding="utf-8"))
        bad_target["target_plugins"] = ["LodestonePaperConformance"]
        contract_path.write_text(json.dumps(bad_target), encoding="utf-8")
        try:
            runner.load_contract(contract_path)
        except runner.ContractError as error:
            check("driver cannot masquerade as target", "separate from target_plugins" in str(error))
        else:
            check("driver cannot masquerade as target", False, "driver target unexpectedly passed")
        fixture(Path(directory))
        contract = runner.load_contract(contract_path)

        bad_driver = json.loads(contract_path.read_text(encoding="utf-8"))
        del bad_driver["driver"]
        contract_path.write_text(json.dumps(bad_driver), encoding="utf-8")
        try:
            runner.load_contract(contract_path)
        except runner.ContractError as error:
            check("driver contract is mandatory", "driver must identify" in str(error))
        else:
            check("driver contract is mandatory", False, "validation unexpectedly passed")
        fixture(Path(directory))
        contract = runner.load_contract(contract_path)

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

        bad_launcher = json.loads(contract_path.read_text(encoding="utf-8"))
        bad_launcher["commands"]["paper"] = ["java-wrapper", "{java}", "-jar", "{paper_jar}"]
        contract_path.write_text(json.dumps(bad_launcher), encoding="utf-8")
        try:
            runner.load_contract(contract_path)
        except runner.ContractError as error:
            check("Paper cannot hide the pinned JDK behind a wrapper", "invoke the pinned JDK directly" in str(error))
        else:
            check("Paper cannot hide the pinned JDK behind a wrapper", False, "wrapper unexpectedly passed")

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

        external_contract = json.loads(contract_path.read_text(encoding="utf-8"))
        external_contract["commands"]["paper"] = [
            "{java}",
            "-jar",
            "{paper_jar}",
            "--external",
        ]
        contract_path.write_text(json.dumps(external_contract), encoding="utf-8")
        external_loaded = runner.load_contract(contract_path)
        external_run = runner.run_paper_backend(external_loaded)
        check(
            "Paper backend accepts external target-load evidence",
            external_run["backend"] == "paper"
            and "operator-protection-plugin" in external_run["enabled_plugins"],
        )
        fixture(Path(directory))
        contract = runner.load_contract(contract_path)

        mutated_contract = json.loads(contract_path.read_text(encoding="utf-8"))
        mutated_contract["commands"]["paper"] = [
            "{java}",
            "-jar",
            "{paper_jar}",
            "--external",
            "--mutate-staged",
        ]
        contract_path.write_text(json.dumps(mutated_contract), encoding="utf-8")
        mutated_loaded = runner.load_contract(contract_path)
        try:
            runner.run_paper_backend(mutated_loaded)
        except runner.ContractError as error:
            check("staged plugin mutation is rejected", "staged plugin changed" in str(error))
        else:
            check("staged plugin mutation is rejected", False, "staged mutation unexpectedly passed")
        fixture(Path(directory))
        contract = runner.load_contract(contract_path)

        try:
            runner.run_paper_backend(contract)
        except runner.ContractError as error:
            check("synthetic Paper process output is rejected", "synthetic" in str(error))
        else:
            check("synthetic Paper process output is rejected", False, "synthetic output unexpectedly passed")

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
