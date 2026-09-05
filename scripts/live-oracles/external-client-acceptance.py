#!/usr/bin/env python3
"""Run bounded release-client acceptance rows against dedicated Lodestone hosts.

This runner deliberately does not contain a protocol client. A launch driver must
control a separately installed, unmodified release client and leave its own
join/action witness. That separation prevents an adapter round trip from being
reported as external-client evidence.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import socket
import subprocess
import sys
import time
from dataclasses import dataclass
from typing import Any


ROOT = pathlib.Path(__file__).resolve().parents[2]
ACTION = "start_destroy_block"
SCHEMA = 1


@dataclass(frozen=True)
class Row:
    family: str
    protocol: int
    release: str


# These are the registry's hostable rows, not all joinable revisions. Each
# release string is the external client's provenance identity and each feature
# builds the matching server family without naming a version crate in Rust.
ROWS = (
    Row("v1-7", 5, "1.7.10"),
    Row("v1-8", 47, "1.8.9"),
    Row("v1-9", 110, "1.9.4"),
    Row("v1-9", 210, "1.10.2"),
    Row("v1-9", 316, "1.11.2"),
    Row("v1-9", 340, "1.12.2"),
    Row("v1-13", 404, "1.13.2"),
    Row("v1-14", 498, "1.14.4"),
    Row("v1-14", 578, "1.15.2"),
    Row("v1-14", 754, "1.16.5"),
    Row("v1-17", 756, "1.17.1"),
    Row("v1-17", 758, "1.18.2"),
    Row("v1-19", 762, "1.19.4"),
    Row("v1-20-6", 766, "1.20.6"),
    Row("v1-21-11", 774, "1.21.11"),
    Row("v26-2", 776, "26.2"),
)


def reserve_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return int(probe.getsockname()[1])


def sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_properties(directory: pathlib.Path, port: int) -> None:
    (directory / "eula.txt").write_text("eula=true\n", encoding="utf-8")
    (directory / "server.properties").write_text(
        "\n".join((
            f"server-port={port}", "online-mode=false", "gamemode=creative",
            "difficulty=peaceful", "view-distance=2", "simulation-distance=0",
            "level-seed=343", "level-name=world", "enable-rcon=false", "")),
        encoding="utf-8",
    )


def wait_for_server(log: pathlib.Path, process: subprocess.Popen[bytes], deadline: float) -> None:
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"server exited with {process.returncode}; see {log}")
        if log.exists() and "Done! Listening on" in log.read_text(encoding="utf-8", errors="replace"):
            return
        time.sleep(0.2)
    raise RuntimeError(f"timed out waiting for server readiness; see {log}")


def artifact(value: Any, evidence: pathlib.Path, name: str) -> pathlib.Path:
    if not isinstance(value, str) or not value:
        raise RuntimeError(f"evidence provenance.{name} must be a non-empty path")
    path = pathlib.Path(value)
    if not path.is_absolute():
        path = evidence.parent / path
    if not path.is_file() or path.stat().st_size == 0:
        raise RuntimeError(f"evidence provenance.{name} is not a non-empty file: {path}")
    return path


def validate_evidence(evidence: pathlib.Path, row: Row, started_at: float) -> dict[str, Any]:
    if not evidence.is_file() or evidence.stat().st_mtime < started_at:
        raise RuntimeError(f"no fresh evidence at {evidence}")
    try:
        value = json.loads(evidence.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        raise RuntimeError(f"evidence is not JSON: {error}") from error
    if not isinstance(value, dict):
        raise RuntimeError("evidence root must be an object")
    expected = {"schema": SCHEMA, "protocol": row.protocol, "release": row.release}
    for key, wanted in expected.items():
        if value.get(key) != wanted:
            raise RuntimeError(f"evidence {key} must be {wanted!r}, got {value.get(key)!r}")
    join = value.get("join")
    action = value.get("play_action")
    provenance = value.get("provenance")
    if not isinstance(join, dict) or join.get("observed") is not True:
        raise RuntimeError("evidence join.observed must be true")
    if not isinstance(action, dict) or action.get("kind") != ACTION or action.get("observed") is not True:
        raise RuntimeError(f"evidence play_action must record observed {ACTION}")
    if not isinstance(provenance, dict):
        raise RuntimeError("evidence provenance must be an object")
    for key in ("client_binary", "client_build", "capture_method"):
        if not isinstance(provenance.get(key), str) or not provenance[key]:
            raise RuntimeError(f"evidence provenance.{key} must be a non-empty string")
    capture = artifact(provenance.get("capture"), evidence, "capture")
    client_log = artifact(provenance.get("client_log"), evidence, "client_log")
    return {
        "evidence": str(evidence),
        "capture": str(capture),
        "capture_sha256": sha256(capture),
        "client_log": str(client_log),
        "client_log_sha256": sha256(client_log),
        "client_binary": provenance["client_binary"],
        "client_build": provenance["client_build"],
        "capture_method": provenance["capture_method"],
    }


def stop(process: subprocess.Popen[bytes] | None) -> None:
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def run_row(row: Row, args: argparse.Namespace) -> dict[str, Any]:
    row_dir = args.output / f"protocol-{row.protocol}"
    server_dir = row_dir / "server"
    server_dir.mkdir(parents=True, exist_ok=False)
    port = reserve_port()
    write_properties(server_dir, port)
    evidence = row_dir / "external-client-evidence.json"
    server_log = row_dir / "server.log"
    target = args.target_dir / f"protocol-{row.protocol}"
    command = [
        "cargo", "run", "-j", str(args.jobs), "--target-dir", str(target),
        "-p", "lodestone-dedicated-server", "--no-default-features", "--features", row.family,
        "--", "--protocol", str(row.protocol), str(server_dir),
    ]
    started_at = time.time()
    process: subprocess.Popen[bytes] | None = None
    try:
        with server_log.open("wb") as log:
            process = subprocess.Popen(command, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT)
            wait_for_server(server_log, process, time.monotonic() + args.deadline_seconds)
            if args.mode == "launch":
                if not args.driver:
                    raise RuntimeError("--mode launch requires --driver or LODESTONE_EXTERNAL_CLIENT_DRIVER")
                client = [
                    args.driver, "--release", row.release, "--protocol", str(row.protocol),
                    "--host", "127.0.0.1", "--port", str(port), "--action", ACTION,
                    "--evidence", str(evidence), "--deadline-seconds", str(args.deadline_seconds),
                ]
                result = subprocess.run(client, cwd=ROOT, timeout=args.deadline_seconds, check=False)
                if result.returncode:
                    raise RuntimeError(f"external client driver exited with {result.returncode}")
            else:
                print(f"Attach release {row.release} (protocol {row.protocol}) to 127.0.0.1:{port}.")
                print(f"Perform {ACTION}, then write the driver evidence to {evidence}.")
                end = time.monotonic() + args.deadline_seconds
                while time.monotonic() < end and not evidence.exists():
                    time.sleep(0.2)
            provenance = validate_evidence(evidence, row, started_at)
            return {"status": "passed", "family": row.family, "protocol": row.protocol,
                    "release": row.release, "server_log": str(server_log), **provenance}
    except Exception as error:  # results are the durable per-protocol report.
        return {"status": "failed", "family": row.family, "protocol": row.protocol,
                "release": row.release, "server_log": str(server_log), "error": str(error)}
    finally:
        stop(process)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--list", action="store_true", help="print the hosted acceptance matrix and exit")
    parser.add_argument("--protocol", type=int, action="append", help="run only this hosted protocol (repeatable)")
    parser.add_argument("--mode", choices=("launch", "attach"), default="launch")
    parser.add_argument("--driver", default=os.environ.get("LODESTONE_EXTERNAL_CLIENT_DRIVER"))
    parser.add_argument("--output", type=pathlib.Path, help="empty directory for evidence and logs")
    parser.add_argument("--target-dir", type=pathlib.Path, default=pathlib.Path("/private/tmp/lodestone-external-client-target"))
    parser.add_argument("--deadline-seconds", type=int, default=90)
    parser.add_argument("--jobs", type=int, default=2)
    args = parser.parse_args()
    if args.list:
        for row in ROWS:
            print(f"{row.protocol}\t{row.family}\t{row.release}")
        return 0
    if args.output is None:
        parser.error("--output is required for an acceptance run")
    if args.deadline_seconds <= 0 or args.jobs <= 0:
        parser.error("--deadline-seconds and --jobs must be positive")
    args.output = args.output.resolve()
    if args.output.exists():
        parser.error(f"--output must not already exist: {args.output}")
    selected = tuple(row for row in ROWS if not args.protocol or row.protocol in args.protocol)
    unknown = set(args.protocol or ()) - {row.protocol for row in ROWS}
    if unknown:
        parser.error(f"not hosted: {sorted(unknown)}")
    args.output.mkdir(parents=True)
    results = [run_row(row, args) for row in selected]
    report = {"schema": SCHEMA, "action": ACTION, "mode": args.mode, "results": results}
    report_path = args.output / "report.json"
    report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    for result in results:
        print(f"{result['status'].upper()} protocol {result['protocol']} ({result['release']}): {result.get('error', result['evidence'])}")
    print(f"report: {report_path}")
    return 0 if all(result["status"] == "passed" for result in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
