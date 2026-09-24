#!/usr/bin/env python3
"""Fail-closed coordinator for the three-dimension P06 full-grid oracle.

The Java oracle remains the authority for materialization and shard bytes.  This
driver owns only durable path/configuration checks and the finite sequence of
commands around it: materialize each independent root, export resumable shards,
merge and duplicate-read authenticate each dimension, then invoke the Rust
comparator once per accepted manifest.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Sequence


MIN_COORD = -500
MAX_COORD = 500
GRID_SIDE = MAX_COORD - MIN_COORD + 1
GRID_COUNT = GRID_SIDE * GRID_SIDE
HALO_MIN = MIN_COORD - 1
HALO_MAX = MAX_COORD + 1
P06_SCHEMA = "lodestone-full-grid-p06/v1"
STATE_NAME = ".lodestone-full-grid-p06.json"
GIB = 1024**3

# A full v6 manifest and its required packet-audit sidecar.  The estimate is
# intentionally conservative: it includes two complete reads plus merge and
# accepted copies, and is only a preflight floor, not a capacity claim.
MANIFEST_BYTES = 256 + GRID_COUNT * 2
AUDIT_BYTES = 256 + GRID_COUNT * 32
OUTPUT_FLOOR_BYTES = 2 * 2 * (MANIFEST_BYTES + AUDIT_BYTES)
DEFAULT_MIN_FREE_BYTES = 8 * GIB
DEFAULT_MIN_RAM_BYTES = 4 * GIB
DEFAULT_BATCH_SIZE = 256
MAX_BATCH_SIZE = 2048
DEFAULT_WORKERS = 4


class DriverError(Exception):
    """A refusal that must stop the run before any oracle command starts."""


@dataclass(frozen=True)
class Dimension:
    name: str
    root: Path


DIMENSION_NAMES = ("overworld", "nether", "end")


def script_dir() -> Path:
    return Path(__file__).resolve().parent


def repo_root() -> Path:
    return script_dir().parents[1]


def parse_bytes(value: str, option: str) -> int:
    try:
        result = int(value, 10)
    except ValueError as error:
        raise DriverError(f"{option} must be a positive integer byte count: {value!r}") from error
    if result <= 0:
        raise DriverError(f"{option} must be positive")
    return result


def absolute_outside_repo(value: str, option: str, root: Path) -> Path:
    path = Path(value).expanduser()
    if not path.is_absolute():
        raise DriverError(f"{option} must be an absolute path outside the repository")
    path = path.resolve()
    try:
        path.relative_to(root)
    except ValueError:
        return path
    raise DriverError(f"{option} must be outside the repository: {path}")


def available_ram_bytes() -> int | None:
    """Return a conservative host RAM figure without third-party packages."""
    try:
        if sys.platform == "darwin":
            result = subprocess.run(
                ["sysctl", "-n", "hw.memsize"],
                check=True,
                capture_output=True,
                text=True,
            )
            return int(result.stdout.strip())
        meminfo = Path("/proc/meminfo")
        if meminfo.is_file():
            values = {}
            for line in meminfo.read_text(encoding="ascii").splitlines():
                key, _, raw = line.partition(":")
                if key in {"MemAvailable", "MemTotal"}:
                    values[key] = int(raw.strip().split()[0]) * 1024
            return values.get("MemAvailable") or values.get("MemTotal")
    except (OSError, ValueError, subprocess.SubprocessError):
        return None
    return None


def disk_free_bytes(path: Path) -> int:
    probe = path
    while not probe.exists() and probe != probe.parent:
        probe = probe.parent
    try:
        return shutil.disk_usage(probe).free
    except OSError as error:
        raise DriverError(f"cannot measure free disk space near {path}: {error}") from error


def preflight_resources(
    dimensions: Sequence[Dimension],
    output_root: Path,
    batch_size: int,
    min_free_bytes: int,
    min_ram_bytes: int,
    *,
    ram_bytes: int | None = None,
    workers: int = 1,
) -> None:
    if not 1 <= batch_size <= MAX_BATCH_SIZE:
        raise DriverError(f"--batch-size must be between 1 and {MAX_BATCH_SIZE}; got {batch_size}")
    # A batch is held by the JVM as packet bodies plus the audit list.  The
    # estimate deliberately overstates each centre at 128 KiB, and reserves a
    # one-GiB server/runtime floor before accepting a requested batch.
    estimated_batch_bytes = 1 * GIB + workers * batch_size * 128 * 1024
    if estimated_batch_bytes > min_ram_bytes:
        raise DriverError(
            f"--batch-size={batch_size} requires about {estimated_batch_bytes} bytes of RAM "
            f"but --min-ram-bytes is {min_ram_bytes}"
        )
    observed_ram = available_ram_bytes() if ram_bytes is None else ram_bytes
    if observed_ram is None:
        raise DriverError("cannot measure host RAM; refusing to start the P06 oracle")
    if observed_ram < min_ram_bytes:
        raise DriverError(
            f"RAM preflight failed: observed {observed_ram} bytes, required {min_ram_bytes}"
        )
    required_disk = max(min_free_bytes, OUTPUT_FLOOR_BYTES)
    paths = [output_root, *(dimension.root for dimension in dimensions)]
    for path in paths:
        free = disk_free_bytes(path)
        if free < required_disk:
            raise DriverError(
                f"disk preflight failed near {path}: observed {free} bytes, "
                f"required at least {required_disk}"
            )


def state_payload(dimensions: Sequence[Dimension], output_root: Path, batch_size: int, workers: int) -> dict:
    return {
        "schema": P06_SCHEMA,
        "format": "P06",
        "seed": 42,
        "bounds": [MIN_COORD, MAX_COORD, MIN_COORD, MAX_COORD],
        "halo": [HALO_MIN, HALO_MAX, HALO_MIN, HALO_MAX],
        "batch_size": batch_size,
        "workers": workers,
        "output_root": str(output_root),
        "dimensions": {dimension.name: str(dimension.root) for dimension in dimensions},
    }


def load_or_create_state(
    output_root: Path,
    dimensions: Sequence[Dimension],
    batch_size: int,
    workers: int,
    *,
    dry_run: bool,
) -> None:
    expected = state_payload(dimensions, output_root, batch_size, workers)
    state_path = output_root / STATE_NAME
    if state_path.exists():
        if not state_path.is_file():
            raise DriverError(f"P06 state path is not a regular file: {state_path}")
        try:
            actual = json.loads(state_path.read_text(encoding="utf-8"))
        except (OSError, ValueError) as error:
            raise DriverError(f"cannot read P06 state file {state_path}: {error}") from error
        if actual != expected:
            raise DriverError(
                f"P06 state is incompatible at {state_path}; refusing to reuse or overwrite the output root"
            )
        return
    if output_root.exists() and not output_root.is_dir():
        raise DriverError(f"--output-root is not a directory: {output_root}")
    if output_root.exists() and any(output_root.iterdir()):
        raise DriverError(
            f"existing output root {output_root} has no compatible {STATE_NAME}; "
            "refusing to overwrite unknown shards"
        )
    if dry_run:
        return
    output_root.mkdir(parents=True, exist_ok=True)
    temporary = state_path.with_name(state_path.name + ".tmp")
    if temporary.exists():
        raise DriverError(f"P06 state update is unfinished at {temporary}; refusing to resume")
    temporary.write_text(json.dumps(expected, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    try:
        os.replace(temporary, state_path)
    except OSError:
        temporary.unlink(missing_ok=True)
        raise


def legacy_provenance_paths(root: Path) -> Iterable[Path]:
    names = [
        "lodestone-large-parity-v3.freeze.sha256",
        "lodestone-large-parity-v3.materialize",
        "lodestone-large-parity-v3.materialize.tmp",
    ]
    for dimension in DIMENSION_NAMES:
        names.extend(
            (
                f"lodestone-large-parity-v4-{dimension}.freeze.sha256",
                f"lodestone-large-parity-v4-{dimension}.materialize",
                f"lodestone-large-parity-v4-{dimension}.materialize.tmp",
                f"lodestone-large-parity-v5-{dimension}.freeze.sha256",
                f"lodestone-large-parity-v5-{dimension}.materialize",
                f"lodestone-large-parity-v5-{dimension}.materialize.tmp",
            )
        )
    return (root / name for name in names)


def ensure_world_root(root: Path, dimension: str) -> None:
    if root.exists() and not root.is_dir():
        raise DriverError(f"{dimension} world root is not a directory: {root}")
    for path in legacy_provenance_paths(root):
        if path.exists():
            raise DriverError(
                f"{dimension} root contains incompatible legacy provenance {path}; "
                "create a new empty P06 root"
            )
    if not root.exists():
        return
    p06_progress = root / f"lodestone-large-parity-materialization-v6-{dimension}.materialize"
    p06_temporary = root / f"lodestone-large-parity-materialization-v6-{dimension}.materialize.tmp"
    p06_seal = root / f"lodestone-large-parity-materialization-v6-{dimension}.freeze.sha256"
    if any(root.iterdir()) and not (p06_progress.exists() or p06_temporary.exists() or p06_seal.exists()):
        raise DriverError(
            f"{dimension} root {root} is non-empty but has no compatible P06 checkpoint; "
            "refusing to regenerate or overwrite it"
        )
    if p06_temporary.exists():
        raise DriverError(
            f"{dimension} root has an unfinished P06 progress update {p06_temporary}; refusing resume"
        )
    if p06_progress.exists():
        try:
            values = {}
            for line in p06_progress.read_text(encoding="ascii").splitlines():
                key, separator, value = line.partition("=")
                if not separator or not key or key in values:
                    raise ValueError("malformed progress")
                values[key] = value
            expected_marker = f"lodestone-large-parity-materialization-v6-{dimension}-progress"
            expected = {
                expected_marker: "1",
                "seed": "42",
                "tile-size": "16",
                "min-x": str(HALO_MIN),
                "max-x": str(HALO_MAX),
                "min-z": str(HALO_MIN),
                "max-z": str(HALO_MAX),
                "tiles-x": "63",
                "tiles-z": "63",
                "inflight-end": "-1",
            }
            if any(values.get(key) != value for key, value in expected.items()):
                raise ValueError("P06 progress geometry or provenance differs")
            if set(values) != set(expected) | {"epoch-tiles", "next-tile"}:
                raise ValueError("P06 progress fields differ")
            if int(values["epoch-tiles"]) <= 0 or not 0 <= int(values["next-tile"]) <= 63 * 63:
                raise ValueError("P06 progress cursor is out of range")
        except (OSError, UnicodeError, ValueError) as error:
            raise DriverError(
                f"{dimension} root has an incompatible P06 progress journal {p06_progress}: {error}"
            ) from error
    elif p06_seal.exists():
        raise DriverError(
            f"{dimension} root has a P06 seal without its full-grid progress journal; refusing reuse"
        )


def materialize_command(dimension: str) -> list[str]:
    return [
        "bash",
        str(script_dir() / "large-parity.sh"),
        "--mode",
        "materialize",
        "--raw-packet",
        "--dimension",
        dimension,
        "--cx",
        str(MIN_COORD),
        str(MAX_COORD),
        "--cz",
        str(MIN_COORD),
        str(MAX_COORD),
    ]


def shard_commands(dimension: str, read_name: str) -> list[list[str]]:
    """Build resumable P06 exports in the Java oracle's stable order."""
    commands: list[list[str]] = []
    if dimension == "end":
        # End is intentionally one JVM at a time.  Two-row shards bound dirty
        # chunk state while preserving the scheduler order required by P06.
        for z0 in range(MIN_COORD, MAX_COORD + 1, 2):
            z1 = min(MAX_COORD, z0 + 1)
            relative = f"{read_name}/end/shard-z{z0}-{z1}.lwp"
            commands.append(
                export_command(
                    dimension,
                    relative,
                    MIN_COORD,
                    MAX_COORD,
                    z0,
                    z1,
                )
            )
    else:
        for x0 in range(MIN_COORD, MAX_COORD + 1, 32):
            x1 = min(MAX_COORD, x0 + 31)
            relative = f"{read_name}/{dimension}/shard-x{x0}-{x1}.lwp"
            commands.append(
                export_command(
                    dimension,
                    relative,
                    x0,
                    x1,
                    MIN_COORD,
                    MAX_COORD,
                )
            )
    return commands


def export_command(dimension: str, relative: str, x0: int, x1: int, z0: int, z1: int) -> list[str]:
    return [
        "bash",
        str(script_dir() / "large-parity.sh"),
        "--mode",
        "export",
        "--raw-packet",
        "--dimension",
        dimension,
        "--out",
        f"/oracle-out/{relative}",
        "--cx",
        str(x0),
        str(x1),
        "--cz",
        str(z0),
        str(z1),
        "--resume",
    ]


def run(command: Sequence[str], env: dict[str, str], *, dry_run: bool) -> None:
    rendered = shlex.join(command)
    print(f"$ {rendered}")
    if dry_run:
        return
    subprocess.run(command, check=True, env=env)


def merge_command(output: Path, paths: Sequence[Path]) -> list[str]:
    return [
        sys.executable,
        str(script_dir() / "large-parity-manifest.py"),
        "merge",
        "--out",
        str(output),
        *(str(path) for path in paths),
    ]


def validate_command(paths: Sequence[Path]) -> list[str]:
    return [
        sys.executable,
        str(script_dir() / "large-parity-manifest.py"),
        "validate",
        *(str(path) for path in paths),
    ]


def accept_command(output: Path, first: Path, second: Path) -> list[str]:
    return [
        sys.executable,
        str(script_dir() / "large-parity-manifest.py"),
        "accept",
        "--out",
        str(output),
        str(first),
        str(second),
    ]


def safe_generated_file(command: Sequence[str], destination: Path, env: dict[str, str], *, dry_run: bool) -> None:
    """Run a merge/accept into a temporary path and never overwrite a result."""
    if dry_run:
        run(command, env, dry_run=True)
        return
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="p06-merge-", dir=destination.parent) as temporary:
        temporary_path = Path(temporary) / destination.name
        rewritten = list(command)
        try:
            index = rewritten.index("--out")
            rewritten[index + 1] = str(temporary_path)
        except (ValueError, IndexError) as error:
            raise DriverError("internal command missing --out") from error
        run(rewritten, env, dry_run=False)
        temporary_audit = Path(str(temporary_path) + ".packet-audit")
        destination_audit = Path(str(destination) + ".packet-audit")
        if destination.exists():
            if destination.read_bytes() != temporary_path.read_bytes():
                raise DriverError(f"existing generated output differs; refusing overwrite: {destination}")
            if temporary_audit.exists() != destination_audit.exists() or (
                temporary_audit.exists()
                and destination_audit.read_bytes() != temporary_audit.read_bytes()
            ):
                raise DriverError(f"existing packet-audit output differs; refusing overwrite: {destination_audit}")
            return
        os.replace(temporary_path, destination)
        if temporary_audit.exists():
            os.replace(temporary_audit, destination_audit)


def existing_shards(directory: Path) -> list[Path]:
    return sorted(path for path in directory.rglob("*.lwp") if path.is_file())


def rust_command(raw: str | None) -> list[str]:
    value = raw or os.environ.get(
        "LODESTONE_P06_RUST_COMMAND",
        "cargo test -p lodestone-v26-2 --test large_worldgen_parity "
        "parity_manifest_streams_before_rust_comparison -- --ignored --nocapture",
    )
    command = shlex.split(value)
    if not command:
        raise DriverError("--rust-command must not be empty")
    return command


def run_dimension(
    dimension: Dimension,
    output_root: Path,
    workers: int,
    batch_size: int,
    rust: Sequence[str],
    *,
    dry_run: bool,
) -> None:
    if not dry_run:
        # run.sh requires the host mountpoint to exist before it starts the
        # container.  Creating only these explicitly selected roots is safe;
        # no existing root is cleared or replaced.
        dimension.root.mkdir(parents=True, exist_ok=True)
        (output_root / dimension.name).mkdir(parents=True, exist_ok=True)
    world_env = os.environ.copy()
    world_env.update(
        {
            "LODESTONE_ORACLE_WORLD_ROOT": str(dimension.root),
            "LODESTONE_ORACLE_EPOCH_TILES": world_env.get("LODESTONE_ORACLE_EPOCH_TILES", "32"),
            "LODESTONE_ORACLE_BATCH": str(batch_size),
            "LODESTONE_ORACLE_DIMENSION": dimension.name,
        }
    )
    run(materialize_command(dimension.name), world_env, dry_run=dry_run)

    for read_name in ("read-a", "read-b"):
        read_root = output_root / dimension.name / read_name
        export_env = os.environ.copy()
        export_env.update(
            {
                "LODESTONE_ORACLE_FROZEN_WORLD_ROOT": str(dimension.root),
                "LODESTONE_ORACLE_OUTPUT_ROOT": str(output_root / dimension.name),
                "LODESTONE_ORACLE_DIMENSION": dimension.name,
                "LODESTONE_ORACLE_BATCH": str(batch_size),
            }
        )
        if dimension.name == "end":
            export_env["LODESTONE_ORACLE_END_BOUNDED"] = "1"
        commands = shard_commands(dimension.name, read_name)
        if dimension.name != "end" and workers > 1 and not dry_run:
            # Each shard has an independent output file and reads the sealed
            # root through its own container.  End is deliberately excluded:
            # its persisted-light export order is part of the P06 contract.
            with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
                futures = [pool.submit(run, command, export_env, dry_run=False) for command in commands]
                for future in futures:
                    future.result()
        else:
            for command in commands:
                run(command, export_env, dry_run=dry_run)
        shards = existing_shards(read_root) if not dry_run else [read_root / "<planned-shards>"]
        merged = output_root / dimension.name / f"{read_name}.lwp"
        safe_generated_file(merge_command(merged, shards), merged, export_env, dry_run=dry_run)
        run(validate_command([merged]), export_env, dry_run=dry_run)

    accepted = output_root / dimension.name / "accepted.lwp"
    accepted_command = accept_command(
        accepted,
        output_root / dimension.name / "read-a.lwp",
        output_root / dimension.name / "read-b.lwp",
    )
    safe_generated_file(accepted_command, accepted, os.environ.copy(), dry_run=dry_run)
    run(validate_command([accepted]), os.environ.copy(), dry_run=dry_run)

    compare_env = os.environ.copy()
    compare_env.update(
        {
            "LODESTONE_LARGE_PARITY_MANIFEST": str(accepted),
            "LODESTONE_LARGE_PARITY_REQUIRE_FULL_GRID": "1",
            "LODESTONE_LARGE_PARITY_RAW_PACKET": "1",
        }
    )
    print(f"# P06 Rust comparison: {dimension.name} (End exports were serial)" if dimension.name == "end" else f"# P06 Rust comparison: {dimension.name}")
    run(rust, compare_env, dry_run=dry_run)


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--overworld-root", required=True)
    result.add_argument("--nether-root", required=True)
    result.add_argument("--end-root", required=True)
    result.add_argument("--output-root", required=True)
    result.add_argument("--batch-size", type=int, default=DEFAULT_BATCH_SIZE)
    result.add_argument("--workers", type=int, default=DEFAULT_WORKERS)
    result.add_argument("--min-free-bytes", default=str(DEFAULT_MIN_FREE_BYTES))
    result.add_argument("--min-ram-bytes", default=str(DEFAULT_MIN_RAM_BYTES))
    result.add_argument("--rust-command", help="shell-like command for the ignored Rust comparator")
    result.add_argument("--dry-run", action="store_true", help="print the complete command plan without starting an oracle")
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        root = repo_root()
        output_root = absolute_outside_repo(args.output_root, "--output-root", root)
        dimensions = [
            Dimension("overworld", absolute_outside_repo(args.overworld_root, "--overworld-root", root)),
            Dimension("nether", absolute_outside_repo(args.nether_root, "--nether-root", root)),
            Dimension("end", absolute_outside_repo(args.end_root, "--end-root", root)),
        ]
        if len({dimension.root for dimension in dimensions}) != len(dimensions):
            raise DriverError("Overworld, Nether, and End roots must be three distinct directories")
        if output_root in {dimension.root for dimension in dimensions}:
            raise DriverError("--output-root must be distinct from every dimension world root")
        if args.workers < 1:
            raise DriverError("--workers must be positive")
        min_free = parse_bytes(args.min_free_bytes, "--min-free-bytes")
        min_ram = parse_bytes(args.min_ram_bytes, "--min-ram-bytes")
        for dimension in dimensions:
            ensure_world_root(dimension.root, dimension.name)
        # A dry plan must remain usable in restricted CI sandboxes where the
        # host RAM query is intentionally denied.  Real execution always uses
        # the measured value; tests of the rejection logic call
        # ``preflight_resources`` directly with an explicit observation.
        observed_ram = min_ram if args.dry_run else None
        preflight_resources(
            dimensions,
            output_root,
            args.batch_size,
            min_free,
            min_ram,
            ram_bytes=observed_ram,
            workers=args.workers,
        )
        load_or_create_state(output_root, dimensions, args.batch_size, args.workers, dry_run=args.dry_run)
        rust = rust_command(args.rust_command)
        for dimension in dimensions:
            run_dimension(dimension, output_root, args.workers, args.batch_size, rust, dry_run=args.dry_run)
    except DriverError as error:
        print(f"full-grid-p06 refusal: {error}", file=sys.stderr)
        return 2
    except subprocess.CalledProcessError as error:
        print(f"full-grid-p06 command failed with exit {error.returncode}: {shlex.join(error.cmd)}", file=sys.stderr)
        return error.returncode or 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
