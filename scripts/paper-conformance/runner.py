#!/usr/bin/env python3
"""Validate the operator-owned Paper/plugin conformance contract.

This is deliberately a contract and result scaffold, not a compatibility
claim.  ``run`` validates the supplied artifacts and writes deterministic
NDJSON explaining why execution is blocked until Lodestone has a Paper plugin
lifecycle, event dispatch, and shared scenario driver.  It never starts a
one-sided Paper run that could be mistaken for differential evidence.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any, TextIO


SCHEMA = 1
ALLOWED_DOMAINS = frozenset({"world-editing", "permissions-economy", "server-api"})
REQUIRED_PREREQUISITES = (
    {
        "name": "lodestone-paper-plugin-lifecycle",
        "reason": "no production path constructs and enables an unmodified Paper plugin",
    },
    {
        "name": "lodestone-paper-event-dispatch",
        "reason": "no production Paper event bus dispatches BlockBreakEvent semantics",
    },
    {
        "name": "lodestone-shared-scenario-driver",
        "reason": "no runner executes this exact scenario against Lodestone and Paper",
    },
)
EXPECTED_OBSERVATIONS = (
    ("listener-present", "cancelled"),
    ("no-listener", "completed"),
)

SECRET_KEY = re.compile(
    r"(?:password|passphrase|token|secret|api[_-]?key|authorization|cookie|private[_-]?key)",
    re.IGNORECASE,
)
SECRET_ASSIGNMENT = re.compile(
    r"(?i)(\b(?:password|passphrase|token|secret|api[_-]?key|authorization|cookie)\b\s*[=:]\s*)[^\s,;]+"
)


class ContractError(ValueError):
    """The supplied contract cannot provide reproducible evidence."""


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _path(value: Any, *, field: str, base: Path, must_exist: bool = True) -> Path:
    if not isinstance(value, str) or not value.strip():
        raise ContractError(f"{field} must be a non-empty path")
    path = Path(value)
    if not path.is_absolute():
        path = base / path
    path = path.resolve()
    if must_exist and (not path.is_file() or path.stat().st_size == 0):
        raise ContractError(f"{field} must be a non-empty file: {path}")
    return path


def _directory(value: Any, *, field: str) -> Path:
    if not isinstance(value, str) or not value.strip():
        raise ContractError(f"{field} must identify the operator-supplied JDK directory")
    path = Path(value).expanduser().resolve()
    if not path.is_dir():
        raise ContractError(f"{field} must be an existing directory: {path}")
    return path


def _sha(path: Path, value: Any, *, field: str) -> str:
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-fA-F]{64}", value):
        raise ContractError(f"{field} must be a 64-character SHA-256 hex digest")
    observed = sha256(path)
    if observed.lower() != value.lower():
        raise ContractError(f"{field} does not match {path}: observed {observed}")
    return observed


def _command(value: Any, *, field: str, allow_missing: bool = False) -> list[str] | None:
    if value is None and allow_missing:
        return None
    if not isinstance(value, list) or not value or any(
        not isinstance(item, str) or not item for item in value
    ):
        raise ContractError(f"{field} must be a non-empty argv array; shell strings are not allowed")
    if any("{output}" in item for item in value):
        raise ContractError(f"{field} may not interpolate {{output}}; stdout is captured by the runner")
    shell_names = {"sh", "bash", "zsh", "fish", "cmd", "cmd.exe", "powershell", "pwsh"}
    executable = Path(value[0]).name.lower()
    if executable in shell_names or any(item in {"-c", "/c", "-command"} for item in value[1:]):
        raise ContractError(f"{field} must invoke a program directly; shell wrappers are not allowed")
    return list(value)


def _scenario(path: Path) -> dict[str, Any]:
    try:
        scenario = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        raise ContractError(f"scenario is not valid JSON: {path}: {error}") from error
    if not isinstance(scenario, dict) or scenario.get("schema") != SCHEMA:
        raise ContractError(f"scenario.schema must be {SCHEMA}")
    if scenario.get("id") != "block-break-cancel":
        raise ContractError("scenario.id must be 'block-break-cancel'")
    actions = scenario.get("actions")
    if actions != [
        {"kind": "break_block", "position": [1, 64, 0], "expected": "cancelled"}
    ]:
        raise ContractError(
            "scenario.actions must contain the one deterministic break_block cancellation action"
        )
    controls = scenario.get("controls")
    if not isinstance(controls, dict) or controls.get("no_listener") != {
        "listener": "absent",
        "expected": "completed",
    }:
        raise ContractError(
            "scenario.controls.no_listener must declare listener=absent and expected=completed"
        )
    return scenario


def load_contract(path: Path) -> dict[str, Any]:
    """Load and validate an operator-supplied contract, returning normalized data."""
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except OSError as error:
        raise ContractError(f"cannot read contract: {path}: {error}") from error
    except json.JSONDecodeError as error:
        raise ContractError(f"contract is not valid JSON: {path}: {error}") from error
    if not isinstance(raw, dict) or raw.get("schema") != SCHEMA:
        raise ContractError(f"contract.schema must be {SCHEMA}")
    base = path.resolve().parent

    jdk = raw.get("jdk")
    if not isinstance(jdk, dict) or not isinstance(jdk.get("version"), str) or not jdk["version"]:
        raise ContractError("jdk.version must identify the pinned operator JDK")
    java_home = _directory(jdk.get("java_home"), field="jdk.java_home")

    paper = raw.get("paper")
    if not isinstance(paper, dict):
        raise ContractError("paper must be an object")
    paper_path = _path(paper.get("jar"), field="paper.jar", base=base)
    paper_hash = _sha(paper_path, paper.get("sha256"), field="paper.sha256")
    if not isinstance(paper.get("version"), str) or not paper["version"]:
        raise ContractError("paper.version must identify the supported Paper release")
    if isinstance(paper.get("build"), bool) or not isinstance(paper.get("build"), int):
        raise ContractError("paper.build must be an integer")

    plugins = raw.get("plugins")
    if not isinstance(plugins, list) or not plugins:
        raise ContractError("plugins must contain at least one unmodified plugin")
    normalized_plugins: list[dict[str, Any]] = []
    names: set[str] = set()
    for index, plugin in enumerate(plugins):
        field = f"plugins[{index}]"
        if not isinstance(plugin, dict):
            raise ContractError(f"{field} must be an object")
        name = plugin.get("name")
        if not isinstance(name, str) or not name or name in names:
            raise ContractError(f"{field}.name must be unique and non-empty")
        names.add(name)
        domain = plugin.get("domain")
        if domain not in ALLOWED_DOMAINS:
            raise ContractError(f"{field}.domain must be one of {sorted(ALLOWED_DOMAINS)}")
        if plugin.get("unmodified") is not True:
            raise ContractError(f"{field}.unmodified must be true")
        jar = _path(plugin.get("jar"), field=f"{field}.jar", base=base)
        plugin_hash = _sha(jar, plugin.get("sha256"), field=f"{field}.sha256")
        entrypoint = plugin.get("entrypoint")
        if not isinstance(entrypoint, str) or not entrypoint or "/" in entrypoint:
            raise ContractError(f"{field}.entrypoint must be a binary Java class name")
        normalized_plugins.append(
            {
                "name": name,
                "domain": domain,
                "entrypoint": entrypoint,
                "jar": str(jar),
                "sha256": plugin_hash,
                "unmodified": True,
            }
        )

    scenario = _path(raw.get("scenario"), field="scenario", base=base)
    scenario_value = _scenario(scenario)
    commands = raw.get("commands")
    if not isinstance(commands, dict):
        raise ContractError("commands must be an object")
    paper_command = _command(commands.get("paper"), field="commands.paper")
    lodestone_command = _command(
        commands.get("lodestone"), field="commands.lodestone", allow_missing=True
    )

    return {
        "schema": SCHEMA,
        "jdk": {"java_home": str(java_home), "version": jdk["version"]},
        "paper": {
            "jar": str(paper_path),
            "sha256": paper_hash,
            "version": paper["version"],
            "build": paper["build"],
        },
        "plugins": normalized_plugins,
        "scenario": {"path": str(scenario), "sha256": sha256(scenario), **scenario_value},
        "commands": {"paper": paper_command, "lodestone": lodestone_command},
    }


def redact(value: Any, *, key: str = "") -> Any:
    """Redact secret-shaped values while retaining machine-readable structure."""
    if SECRET_KEY.search(key):
        return "[REDACTED]"
    if isinstance(value, dict):
        return {name: redact(item, key=name) for name, item in value.items()}
    if isinstance(value, list):
        return [redact(item, key=key) for item in value]
    if isinstance(value, str):
        return SECRET_ASSIGNMENT.sub(r"\1[REDACTED]", value)
    return value


def canonical_json(value: Any) -> str:
    return json.dumps(redact(value), sort_keys=True, separators=(",", ":"))


def blocked_records(contract: dict[str, Any]) -> list[dict[str, Any]]:
    """Return deterministic records for the current, intentionally blocked gate."""
    contract_id = hashlib.sha256(canonical_json(contract).encode("utf-8")).hexdigest()
    return [
        {
            "schema": SCHEMA,
            "kind": "contract",
            "status": "validated",
            "contract_sha256": contract_id,
            "scenario": contract["scenario"]["id"],
            "plugins": [plugin["name"] for plugin in contract["plugins"]],
        },
        {
            "schema": SCHEMA,
            "kind": "negative_control",
            "id": "no-listener",
            "status": "blocked",
            "listener": "absent",
            "expected": "completed",
            "reason": "shared Paper/Lodestone execution is unavailable; this control must not pass alone",
        },
        {
            "schema": SCHEMA,
            "kind": "execution",
            "backend": "lodestone",
            "status": "blocked",
            "missing_prerequisites": list(REQUIRED_PREREQUISITES),
        },
        {
            "schema": SCHEMA,
            "kind": "execution",
            "backend": "paper",
            "status": "not_run",
            "reason": "differential evidence requires both backends; Paper is not run one-sided",
        },
    ]


def _load_observation(
    path: Path, *, backend: str, scenario_id: str, allow_synthetic: bool
) -> dict[str, Any]:
    """Load one complete backend observation from an NDJSON file."""
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError as error:
        raise ContractError(f"cannot read {backend} results: {path}: {error}") from error
    records: list[dict[str, Any]] = []
    for line_number, line in enumerate(lines, start=1):
        if not line.strip():
            continue
        try:
            value = json.loads(line)
        except json.JSONDecodeError as error:
            raise ContractError(f"{backend} results line {line_number} is not valid JSON: {error}") from error
        if not isinstance(value, dict):
            raise ContractError(f"{backend} results line {line_number} must be an object")
        if value.get("kind") == "observation":
            records.append(value)
    if len(records) != 1 or len([line for line in lines if line.strip()]) != 1:
        raise ContractError(
            f"{backend} results must contain exactly one observation record and no auxiliary records"
        )
    observation = records[0]
    if observation.get("schema") != SCHEMA:
        raise ContractError(f"{backend} observation.schema must be {SCHEMA}")
    if observation.get("backend") != backend:
        raise ContractError(f"{backend} observation.backend must be {backend!r}")
    if observation.get("scenario") != scenario_id:
        raise ContractError(f"{backend} observation.scenario must be {scenario_id!r}")
    if observation.get("status") != "complete":
        raise ContractError(
            f"{backend} observation.status must be 'complete'; blocked or not_run is not evidence"
        )
    evidence_kind = observation.get("evidence_kind")
    if evidence_kind not in {"external", "synthetic"}:
        raise ContractError(f"{backend} observation.evidence_kind must be 'external' or 'synthetic'")
    if evidence_kind == "synthetic" and not allow_synthetic:
        raise ContractError(f"{backend} synthetic evidence is test-only and cannot pass the CLI gate")
    if not isinstance(observation.get("source"), str) or not observation["source"].strip():
        raise ContractError(f"{backend} observation.source must identify its runner")
    observations = observation.get("observations")
    if not isinstance(observations, list) or len(observations) != len(EXPECTED_OBSERVATIONS):
        raise ContractError(
            f"{backend} observation.observations must contain exactly {len(EXPECTED_OBSERVATIONS)} entries"
        )
    seen: set[str] = set()
    normalized: list[dict[str, str]] = []
    for item in observations:
        if not isinstance(item, dict):
            raise ContractError(f"{backend} observation entries must be objects")
        identifier = item.get("id")
        outcome = item.get("outcome")
        if identifier in seen or not isinstance(identifier, str) or not identifier:
            raise ContractError(f"{backend} observation ids must be unique non-empty strings")
        if not isinstance(outcome, str) or not outcome:
            raise ContractError(f"{backend} observation {identifier!r}.outcome must be non-empty")
        seen.add(identifier)
        normalized.append({"id": identifier, "outcome": outcome})
    expected_ids = {identifier for identifier, _ in EXPECTED_OBSERVATIONS}
    if seen != expected_ids:
        raise ContractError(
            f"{backend} observation ids must be {sorted(expected_ids)}, got {sorted(seen)}"
        )
    return {
        "backend": backend,
        "scenario": scenario_id,
        "evidence_kind": evidence_kind,
        "source": observation["source"],
        "observations": normalized,
    }


def compare_evidence(
    contract: dict[str, Any],
    paper: dict[str, Any],
    lodestone: dict[str, Any],
    *,
    allow_synthetic: bool = False,
) -> dict[str, Any]:
    """Compare two already-loaded observations without running either backend.

    ``allow_synthetic`` exists only for this module's independent controls. The
    CLI never enables it, so synthetic fixtures cannot become compatibility
    evidence accidentally.
    """
    for evidence in (paper, lodestone):
        if evidence["evidence_kind"] == "synthetic" and not allow_synthetic:
            raise ContractError("synthetic evidence is test-only and cannot pass the comparison gate")
    paper_by_id = {item["id"]: item["outcome"] for item in paper["observations"]}
    lodestone_by_id = {item["id"]: item["outcome"] for item in lodestone["observations"]}
    controls: list[dict[str, str]] = []
    differences: list[dict[str, str]] = []
    for identifier, expected in EXPECTED_OBSERVATIONS:
        paper_outcome = paper_by_id[identifier]
        lodestone_outcome = lodestone_by_id[identifier]
        matches = paper_outcome == lodestone_outcome == expected
        controls.append(
            {
                "id": identifier,
                "expected": expected,
                "paper": paper_outcome,
                "lodestone": lodestone_outcome,
                "status": "match" if matches else "difference",
            }
        )
        if paper_outcome != lodestone_outcome:
            differences.append(
                {
                    "id": identifier,
                    "field": "outcome",
                    "reason": "backend-mismatch",
                    "paper": paper_outcome,
                    "lodestone": lodestone_outcome,
                }
            )
        if paper_outcome != expected:
            differences.append(
                {
                    "id": identifier,
                    "field": "outcome",
                    "reason": "paper-expected-mismatch",
                    "expected": expected,
                    "paper": paper_outcome,
                }
            )
        if lodestone_outcome != expected:
            differences.append(
                {
                    "id": identifier,
                    "field": "outcome",
                    "reason": "lodestone-expected-mismatch",
                    "expected": expected,
                    "lodestone": lodestone_outcome,
                }
            )
    status = "pass" if not differences else "fail"
    return {
        "schema": SCHEMA,
        "kind": "comparison",
        "status": status,
        "scenario": contract["scenario"]["id"],
        "evidence_kind": "synthetic" if allow_synthetic else "external",
        "controls": controls,
        "differences": differences,
    }


def write_records(records: list[dict[str, Any]], output: TextIO) -> None:
    for record in records:
        output.write(canonical_json(record) + "\n")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("validate", "run", "compare"))
    parser.add_argument("--contract", type=Path, required=True)
    parser.add_argument("--output", type=Path, help="NDJSON destination; defaults to stdout")
    parser.add_argument("--paper-results", type=Path)
    parser.add_argument("--lodestone-results", type=Path)
    args = parser.parse_args(argv)
    try:
        contract = load_contract(args.contract)
    except ContractError as error:
        print(f"contract-error: {error}", file=sys.stderr)
        return 2

    if args.command == "validate":
        print(canonical_json(contract))
        return 0

    if args.command == "compare":
        if not args.paper_results or not args.lodestone_results:
            print("contract-error: compare requires --paper-results and --lodestone-results", file=sys.stderr)
            return 2
        try:
            scenario_id = contract["scenario"]["id"]
            paper = _load_observation(
                args.paper_results,
                backend="paper",
                scenario_id=scenario_id,
                allow_synthetic=False,
            )
            lodestone = _load_observation(
                args.lodestone_results,
                backend="lodestone",
                scenario_id=scenario_id,
                allow_synthetic=False,
            )
            comparison = compare_evidence(contract, paper, lodestone)
        except ContractError as error:
            print(f"comparison-error: {error}", file=sys.stderr)
            return 2
        if args.output:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            with args.output.open("w", encoding="utf-8") as destination:
                write_records([comparison], destination)
        else:
            write_records([comparison], sys.stdout)
        return 0 if comparison["status"] == "pass" else 1

    records = blocked_records(contract)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        with args.output.open("w", encoding="utf-8") as destination:
            write_records(records, destination)
    else:
        write_records(records, sys.stdout)
    print(
        "conformance-blocked: missing "
        + ", ".join(item["name"] for item in REQUIRED_PREREQUISITES),
        file=sys.stderr,
    )
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
