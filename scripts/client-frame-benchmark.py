#!/usr/bin/env python3
"""Run and summarize deterministic Java-backed Lodestone frame benchmarks.

The runner stays in the foreground for its entire child process lifetime. It
uses only the Python standard library so a fresh checkout needs no virtualenv.
"""

from __future__ import annotations

import argparse
import contextlib
import csv
import json
import math
import os
import pathlib
import platform
import re
import shutil
import socket
import statistics
import subprocess
import sys
import tempfile
import time
from typing import Iterable, Mapping


ROOT = pathlib.Path(__file__).resolve().parents[1]
RESULTS = ROOT / "bench-results" / "live_frame_profile.jsonl"
HEAVY_RESULTS = ROOT / "bench-results" / "heavyweight_scene.jsonl"
SCENE = ROOT / "scripts" / "benchmark-scenes" / "showcase.txt"
HEAVY_SCENE_EMITTER = ROOT / "target" / "release" / "examples" / "heavy-scene-server"
HEAVY_COMMAND_PHASES = ("setup", "after_join", "mutation")
HEAVY_SCENARIOS = (
    "palette", "transparency", "light", "liquid", "sign", "block-entity", "entity", "scheduled", "mixed", "dense-mixed",
)
HEAVY_CAMERA_PLANS = ("stationary", "orbit")
# The server's immutable-plan ceiling is intentionally larger for offline plan
# inspection. The client runner is a foreground local profiler: keep its actual
# resource envelope small enough for a shared development machine.
MAX_HEAVY_SCALE = 2
MAX_HEAVY_SCALE_BY_SCENARIO = {"dense-mixed": 1}
MAX_HEAVY_MUTATION_SECONDS = 10
MAX_HEAVY_TOTAL_SECONDS = 120
RCON_PASSWORD = "lodestone"
RENDER_DISTANCE = 24
SERVER_VIEW_DISTANCE = RENDER_DISTANCE + 1
METADATA_COLUMNS = {"frame", "frame_interval_ms", "segment"}
COUNT_COLUMNS = {
    "world.packed_sections_visited",
    "world.model_sections_visited",
    "world.opaque_sections_drawn",
    "world.water_sections_drawn",
    "world.translucent_sections_drawn",
    "world.entities_drawn",
    "world.block_entities_drawn",
    "world.sign_text_vertices",
    "world.particles_drawn",
    "hud.chat_lines",
    "hud.debug_lines",
    "hud.menu_overlays_drawn",
    "light.relight_input_blocks",
    "light.relight_input_sections",
    "light.relight_cells_visited",
    "light.relight_cells_changed",
    "light.relight_dirty_sections",
    "light.remesh_invalidations_enqueued",
    "light.remesh_invalidations_coalesced",
    "light.remesh_sections_submitted",
}
GPU_SAMPLE_RE = re.compile(
    r"gpu: world_total=(?P<world_total>\d+(?:\.\d+)?)ms, "
    r"world=(?P<world>\d+(?:\.\d+)?)ms, "
    r"first_person=(?P<first_person>\d+(?:\.\d+)?)ms, "
    r"hud_total=(?P<hud_total>\d+(?:\.\d+)?)ms"
)

ORACLES = {
    "terrain": {
        "script": ROOT / "scripts" / "live-oracles" / "terrain.sh",
        "world": ROOT / ".cache" / "mc" / "terrain",
        "game_port": 25580,
        "rcon_port": 25581,
    },
    "showcase": {
        "script": ROOT / "scripts" / "live-oracles" / "creative.sh",
        "world": ROOT / ".cache" / "mc" / "creative",
        "game_port": 25570,
        "rcon_port": 25571,
    },
    "megaworld": {
        "script": ROOT / "scripts" / "live-oracles" / "megaworld.sh",
        "world": ROOT / ".cache" / "mc" / "megaworld",
        "game_port": 25590,
        "rcon_port": 25591,
    },
    "lovelier": {
        "script": ROOT / "scripts" / "live-oracles" / "lovelier.sh",
        "world": ROOT / ".cache" / "mc" / "lovelier",
        "game_port": 25600,
        "rcon_port": 25601,
    },
    "heavyweight": {
        "script": ROOT / "scripts" / "live-oracles" / "creative.sh",
        "world": ROOT / ".cache" / "mc" / "creative",
        "game_port": 25570,
        "rcon_port": 25571,
    },
}


def _validate_emitted_scene(scene: object, scenario: str, seed: int, scale: int) -> None:
    """Reject a malformed server handoff before it can alter a live oracle."""
    if not isinstance(scene, dict) or tuple(scene) != (
        "schema", "spec", "commands", "witnesses", "scene_hash"
    ):
        raise RuntimeError("heavy scene has an unexpected top-level shape or field order")
    if scene["schema"] != 1:
        raise RuntimeError(f"heavy scene schema must be 1, got {scene['schema']!r}")
    spec = scene["spec"]
    if not isinstance(spec, dict) or tuple(spec) != ("scenario", "seed", "scale"):
        raise RuntimeError("heavy scene spec has an unexpected shape or field order")
    if spec != {"scenario": scenario, "seed": seed, "scale": scale}:
        raise RuntimeError(f"heavy scene identity mismatch: requested {(scenario, seed, scale)!r}, got {spec!r}")
    commands = scene["commands"]
    if not isinstance(commands, dict) or tuple(commands) != HEAVY_COMMAND_PHASES:
        raise RuntimeError("heavy scene command phases must be setup, after_join, mutation in order")
    for phase, batch in commands.items():
        if not isinstance(batch, list) or any(not isinstance(command, str) or not command.strip() for command in batch):
            raise RuntimeError(f"heavy scene {phase} commands must be nonblank strings")
    witnesses = scene["witnesses"]
    if not isinstance(witnesses, list) or not witnesses:
        raise RuntimeError("heavy scene must declare at least one witness")
    for witness in witnesses:
        if not isinstance(witness, dict) or tuple(witness) != ("segment", "column", "minimum"):
            raise RuntimeError("heavy scene witness has an unexpected shape or field order")
        if (not isinstance(witness["segment"], str) or not witness["segment"]
                or not isinstance(witness["column"], str) or not witness["column"]
                or not isinstance(witness["minimum"], int) or isinstance(witness["minimum"], bool)
                or witness["minimum"] <= 0):
            raise RuntimeError(f"heavy scene witness is invalid: {witness!r}")
    scene_hash = scene["scene_hash"]
    if not isinstance(scene_hash, str) or not re.fullmatch(r"[0-9a-f]{64}", scene_hash):
        raise RuntimeError("heavy scene hash must be a lowercase SHA-256 hex string")


def _emit_heavy_scene(
    emitter: pathlib.Path, scenario: str, seed: int, scale: int, camera_plan: str
) -> dict:
    result = subprocess.run(
        [str(emitter), "--emit-scene", "-", "--scenario", scenario, "--seed", str(seed),
         "--scale", str(scale), "--camera-plan", camera_plan],
        check=False, text=True, capture_output=True,
    )
    if result.returncode:
        raise RuntimeError(f"heavy scene emitter failed ({result.returncode}): {result.stderr.strip()}")
    try:
        scene = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"heavy scene emitter returned invalid JSON: {error}") from error
    _validate_emitted_scene(scene, scenario, seed, scale)
    return scene


def nearest_rank(values: Iterable[float], quantile: float) -> float:
    """Return the observed nearest-rank percentile, never an interpolation."""
    ordered = sorted(values)
    if not ordered:
        raise ValueError("cannot take a percentile of no values")
    if not 0.0 < quantile <= 1.0:
        raise ValueError(f"quantile must be in (0, 1], got {quantile}")
    rank = max(1, math.ceil(quantile * len(ordered)))
    return ordered[rank - 1]


def _numbers(rows: Iterable[Mapping[str, str]], column: str) -> list[float]:
    values = []
    for row in rows:
        text = row.get(column, "").strip()
        if text:
            values.append(float(text))
    return values


def summarize_rows(rows: list[Mapping[str, str]]) -> dict:
    """Reduce raw CSV rows without treating skipped empty phases as zero."""
    intervals = _numbers(rows, "frame_interval_ms")
    if len(intervals) != len(rows):
        raise ValueError("every measured row must carry frame_interval_ms")
    phase_names = sorted(
        {
            name
            for row in rows
            for name in row
            if name not in METADATA_COLUMNS and name not in COUNT_COLUMNS
        }
    )
    phases = {}
    for name in phase_names:
        values = _numbers(rows, name)
        if values:
            phases[name] = statistics.fmean(values)
    count_summary = {}
    for name in sorted(COUNT_COLUMNS):
        values = _numbers(rows, name)
        if values:
            count_summary[name] = {
                "median": statistics.median(values),
                "p95": nearest_rank(values, 0.95),
                "max": max(values),
            }
    return {
        "frames": len(rows),
        "p50_ms": nearest_rank(intervals, 0.50),
        "p95_ms": nearest_rank(intervals, 0.95),
        "p99_ms": nearest_rank(intervals, 0.99),
        "mean_ms": statistics.fmean(intervals),
        "over_16_67": sum(ms > 16.67 for ms in intervals),
        "over_33_3": sum(ms > 33.3 for ms in intervals),
        "phases_ms": phases,
        "workload_counts": count_summary,
    }


def summarize_gpu_log(log_text: str) -> dict:
    """Summarize asynchronous one-second GPU timestamp snapshots.

    Pass values stay separate. In particular, the diagnostic total spans are
    not added to the real-pass readings or presented as bounds.
    """
    samples = [
        {name: float(value) for name, value in match.groupdict().items()}
        for match in GPU_SAMPLE_RE.finditer(log_text)
    ]
    summary = {"samples": len(samples)}
    for name in ("world_total", "world", "first_person", "hud_total"):
        values = [sample[name] for sample in samples]
        if values:
            summary[name] = {
                "median_ms": statistics.median(values),
                "p95_ms": nearest_rank(values, 0.95),
            }
    return summary


def validate_run(
    rows: list[Mapping[str, str]], log_text: str, workload: str
) -> tuple[int, int]:
    """Reject incomplete or mislabeled runs before they enter the history."""
    if "benchmark complete" not in log_text:
        raise ValueError("completion marker missing from client log")
    if "selected hardware built-in display for fullscreen benchmark" not in log_text:
        raise ValueError("client log does not confirm hardware built-in display selection")
    ready = re.search(
        r"benchmark window ready[^\n]*framebuffer_width=(\d+)"
        r"[^\n]*framebuffer_height=(\d+)[^\n]*fullscreen=(true|false)",
        log_text,
    )
    if ready is None:
        raise ValueError("client log does not contain parseable framebuffer/fullscreen metadata")
    if ready.group(3) != "true":
        raise ValueError("client log does not confirm fullscreen presentation")
    framebuffer = (int(ready.group(1)), int(ready.group(2)))
    if framebuffer[0] <= 0 or framebuffer[1] <= 0:
        raise ValueError(f"invalid physical framebuffer size: {framebuffer}")

    measured = {
        row.get("segment", "")
        for row in rows
        if row.get("segment", "").endswith((".stationary", ".moving"))
    }
    wrong = sorted(segment for segment in measured if not segment.startswith(f"{workload}."))
    if wrong:
        raise ValueError(f"workload metadata mismatch: expected {workload}, saw {wrong}")
    for suffix in ("stationary", "moving"):
        label = f"{workload}.{suffix}"
        if not any(row.get("segment") == label for row in rows):
            raise ValueError(f"{suffix} segment has no frame rows")
    return framebuffer


def _build_frame(request_id: int, packet_type: int, payload: str) -> bytes:
    body = (
        request_id.to_bytes(4, "little", signed=True)
        + packet_type.to_bytes(4, "little", signed=True)
        + payload.encode("utf-8")
        + b"\x00\x00"
    )
    return len(body).to_bytes(4, "little", signed=True) + body


def _recv_exact(sock: socket.socket, size: int) -> bytes:
    chunks = []
    remaining = size
    while remaining:
        chunk = sock.recv(remaining)
        if not chunk:
            raise ConnectionError("RCON connection closed before a full frame arrived")
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def _read_response(sock: socket.socket) -> tuple[int, str]:
    length = int.from_bytes(_recv_exact(sock, 4), "little", signed=True)
    if length < 10:
        raise ValueError(f"short RCON response (length={length})")
    body = _recv_exact(sock, length)
    return (
        int.from_bytes(body[:4], "little", signed=True),
        body[8:-2].decode("utf-8", errors="replace"),
    )


class RconClient:
    """Small persistent Source-RCON client for one local oracle."""

    def __init__(self, port: int):
        self.port = port
        self.sock: socket.socket | None = None
        self.request_id = 10

    def __enter__(self) -> "RconClient":
        self.sock = socket.create_connection(("127.0.0.1", self.port), timeout=15)
        self.sock.settimeout(15)
        self.sock.sendall(_build_frame(1, 3, RCON_PASSWORD))
        response_id, _ = _read_response(self.sock)
        if response_id != 1:
            raise RuntimeError("RCON authentication failed")
        return self

    def __exit__(self, *_exc) -> None:
        if self.sock is not None:
            self.sock.close()
            self.sock = None

    def command(self, command: str) -> str:
        if self.sock is None:
            raise RuntimeError("RCON client is not connected")
        self.request_id += 1
        self.sock.sendall(_build_frame(self.request_id, 2, command))
        response_id, payload = _read_response(self.sock)
        if response_id != self.request_id:
            raise RuntimeError(
                f"RCON response id mismatch for {command!r}: got {response_id}"
            )
        return payload.strip()


@contextlib.contextmanager
def _server_view_distance(world: pathlib.Path, distance: int):
    """Raise Java's cap for this launch, then restore the persistent file."""
    properties = world / "server.properties"
    original = properties.read_text(encoding="utf-8")
    replacement, count = re.subn(
        r"(?m)^view-distance=.*$", f"view-distance={distance}", original
    )
    if count != 1:
        raise RuntimeError(f"{properties} must contain exactly one view-distance entry")
    properties.write_text(replacement, encoding="utf-8")
    try:
        yield
    finally:
        properties.write_text(original, encoding="utf-8")


def start_oracle(workload: str) -> dict:
    oracle = ORACLES[workload]
    print(
        f"starting {workload} Java oracle with server view distance "
        f"{SERVER_VIEW_DISTANCE}...",
        flush=True,
    )
    with _server_view_distance(oracle["world"], SERVER_VIEW_DISTANCE):
        subprocess.run([str(oracle["script"])], cwd=ROOT, check=True)
    with RconClient(oracle["rcon_port"]) as rcon:
        rcon.command("defaultgamemode creative")
        rcon.command("difficulty peaceful")
        rcon.command("gamerule advance_time false")
        rcon.command("time set noon")
    return oracle


def _is_scene_error(command: str, reply: str) -> bool:
    if command.startswith("kill @e[tag=lodestone_benchmark]") and "No entity" in reply:
        return False
    error_markers = (
        "Unknown or incomplete command",
        "Incorrect argument",
        "Expected ",
        "No entity was found",
        "That position is not loaded",
        "Cannot place",
        "Too many blocks",
        "Unable to summon",
        "Could not set the block",
    )
    return any(marker in reply for marker in error_markers)


def prepare_showcase(rcon_port: int) -> None:
    commands = []
    for raw in SCENE.read_text(encoding="utf-8").splitlines():
        command = raw.strip()
        if not command or command.startswith("#"):
            continue
        # These target the benchmark's unique player and are replayed by
        # `configure_joined_player` once that player exists. Command-block NBT
        # containing `@a` is intentionally not filtered.
        if command.startswith(("gamemode creative @a", "effect clear @a", "tp @a")):
            continue
        commands.append(command)
    print(f"installing {len(commands)} showcase RCON commands...", flush=True)
    run_rcon_commands(rcon_port, "showcase", commands)


def run_rcon_commands(rcon_port: int, phase: str, commands: list[str]) -> None:
    """Submit a server-owned command batch in order and fail at its exact edge."""
    with RconClient(rcon_port) as rcon:
        for index, command in enumerate(commands, 1):
            reply = rcon.command(command)
            if _is_scene_error(command, reply):
                raise RuntimeError(
                    f"{phase} command {index}/{len(commands)} failed:\n"
                    f"  {command}\n  {reply or '(empty response)'}"
                )


def prepare_heavy_scene(
    rcon_port: int, emitter: pathlib.Path, scenario: str, seed: int, scale: int, camera_plan: str
) -> dict:
    scene = _emit_heavy_scene(emitter, scenario, seed, scale, camera_plan)
    run_rcon_commands(rcon_port, "setup", scene["commands"]["setup"])
    return scene


def joined_player_commands(workload: str, username: str) -> list[str]:
    commands = [
        f"gamemode creative {username}",
        f"effect clear {username}",
    ]
    if workload == "terrain":
        commands.append(f"tp {username} 0 140 0 0 10")
    elif workload == "showcase":
        commands.append(f"tp {username} 0 65 0 0 0")
    elif workload == "lovelier":
        commands.append(f"tp {username} 0 180 0 0 35")
    return commands


def configure_joined_player(workload: str, rcon_port: int, username: str) -> None:
    commands = joined_player_commands(workload, username)
    with RconClient(rcon_port) as rcon:
        for command in commands:
            reply = rcon.command(command)
            if "No player was found" in reply or "No entity was found" in reply:
                raise RuntimeError(f"post-join command failed: {command}: {reply}")


def process_rss_bytes(pid: int) -> int | None:
    result = subprocess.run(
        ["ps", "-o", "rss=", "-p", str(pid)],
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0 or not result.stdout.strip():
        return None
    return int(result.stdout.strip().splitlines()[0]) * 1024


def _read_csv(path: pathlib.Path) -> list[dict[str, str]]:
    with path.open(newline="", encoding="utf-8") as handle:
        return list(csv.DictReader(handle))


def _client_command(
    binary: pathlib.Path,
    workload: str,
    game_port: int,
    durations: tuple[int, ...],
    debug_overlay: str,
    camera_plan: str | None = None,
    heavy_scene: dict | None = None,
) -> list[str]:
    if heavy_scene is None:
        warmup, stationary, moving = durations
        mutation = None
    else:
        warmup, mutation, stationary, moving = durations
    command = [
        str(binary),
        "--benchmark",
        workload,
        "--benchmark-warmup",
        str(warmup),
        "--benchmark-stationary",
        str(stationary),
        "--benchmark-moving",
        str(moving),
        "--benchmark-debug-overlay",
        debug_overlay,
        "--host",
        "127.0.0.1",
        "--port",
        str(game_port),
        "--protocol",
        "776",
        "--render-distance",
        str(RENDER_DISTANCE),
    ]
    if heavy_scene is not None:
        spec = heavy_scene["spec"]
        command.extend([
            "--heavy-scenario", spec["scenario"],
            "--heavy-seed", str(spec["seed"]),
            "--heavy-scale", str(spec["scale"]),
            "--heavy-camera-plan", camera_plan or "stationary",
            "--benchmark-mutation", str(mutation),
        ])
    return command


def _samply_command(client: list[str], artifact: pathlib.Path) -> list[str]:
    return [
        "samply",
        "record",
        "--save-only",
        "--unstable-presymbolicate",
        "--output",
        str(artifact),
        "--",
        *client,
    ]


def _samply_sidecar_path(artifact: pathlib.Path) -> pathlib.Path:
    """Match Samply's `*.json.gz` presymbolication sidecar spelling."""
    return artifact.with_suffix(".syms.json")


def _heavy_profile_record_path(artifact: pathlib.Path) -> pathlib.Path:
    """Return the immutable scene record written next to one Samply capture."""
    return artifact.with_suffix(".record.json")


def _require_nonempty_artifact(path: pathlib.Path, label: str) -> None:
    if not path.is_file() or path.stat().st_size == 0:
        raise RuntimeError(f"heavyweight profile is missing a nonempty {label}: {path}")


def validate_heavy_profile_artifact(artifact: pathlib.Path) -> dict:
    """Check the local capture's sidecars and emitted heavyweight scene identity.

    This intentionally does not decode the potentially large profile. The profile
    analyzer owns that format; this gate establishes that the capture, Samply symbol
    sidecar, and runner-owned record are a coherent profiling handoff.
    """
    artifact = artifact.resolve()
    _require_nonempty_artifact(artifact, "capture")
    _require_nonempty_artifact(_samply_sidecar_path(artifact), "symbol sidecar")
    record_path = _heavy_profile_record_path(artifact)
    _require_nonempty_artifact(record_path, "scene record")
    try:
        record = json.loads(record_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        raise RuntimeError(f"heavyweight scene record is not JSON: {record_path}: {error}") from error
    if not isinstance(record, dict):
        raise RuntimeError(f"heavyweight scene record must be a JSON object: {record_path}")

    scene_hash = record.get("scene_hash")
    required_durations = {"warmup", "mutation", "stationary", "moving"}
    record_capture = record.get("capture")
    if (
        record.get("schema") != 2
        or record.get("workload") != "heavyweight"
        or record.get("profile") != "release"
        or not isinstance(record_capture, str)
        or pathlib.Path(record_capture).resolve() != artifact
        or not isinstance(scene_hash, str)
        or len(scene_hash) != 64
        or any(character not in "0123456789abcdef" for character in scene_hash)
        or record.get("scenario") not in HEAVY_SCENARIOS
        or not isinstance(record.get("seed"), int)
        or record.get("camera_plan") not in HEAVY_CAMERA_PLANS
        or not isinstance(record.get("scale"), int)
        or not 1 <= record["scale"] <= MAX_HEAVY_SCALE_BY_SCENARIO.get(
            record["scenario"], MAX_HEAVY_SCALE
        )
        or not isinstance(record.get("requested_scale"), int)
        or not 1 <= record["requested_scale"] <= MAX_HEAVY_SCALE_BY_SCENARIO.get(
            record["scenario"], MAX_HEAVY_SCALE
        )
        or not isinstance(record.get("durations_seconds"), dict)
        or set(record["durations_seconds"]) != required_durations
        or any(
            not isinstance(seconds, int) or seconds < 0
            for seconds in record["durations_seconds"].values()
        )
        or sum(record["durations_seconds"].values()) > MAX_HEAVY_TOTAL_SECONDS
        or not isinstance(record.get("segments"), dict)
        or not {"stationary", "moving"}.issubset(record["segments"])
        or any(
            not isinstance(record["segments"][segment], dict)
            for segment in ("stationary", "moving")
        )
    ):
        raise RuntimeError(f"heavyweight scene record does not describe this capture: {record_path}")
    return record


def _unique_username(trial: int) -> str:
    suffix = int(time.time() * 1000) % 100_000
    return f"LdB{os.getpid():x}{trial}{suffix}"[:16]


def run_trial(
    workload: str,
    trial: int,
    binary: pathlib.Path,
    oracle: dict,
    durations: tuple[int, ...],
    debug_overlay: str,
    samply_artifact: pathlib.Path | None = None,
    heavy_scene: dict | None = None,
    camera_plan: str | None = None,
) -> dict:
    with tempfile.TemporaryDirectory(prefix=f"lodestone-{workload}-bench-") as temp_name:
        temp = pathlib.Path(temp_name)
        csv_path = temp / "frames.csv"
        log_path = temp / "client.log"
        data_dir = temp / "data"
        data_dir.mkdir()
        username = _unique_username(trial)
        (data_dir / "offline.json").write_text(
            json.dumps({"username": username}), encoding="utf-8"
        )

        client = _client_command(
            binary, workload, oracle["game_port"], durations, debug_overlay,
            camera_plan=camera_plan, heavy_scene=heavy_scene,
        )
        if samply_artifact is not None:
            command = _samply_command(client, samply_artifact)
        else:
            command = client
        env = os.environ.copy()
        env.update(
            {
                "LODESTONE_DATA_DIR": str(data_dir),
                "LODESTONE_FRAME_PROFILE_DUMP": str(csv_path),
                "RUST_LOG": "frame_profile=info,frame_benchmark=info,warn",
            }
        )

        total = sum(durations)
        deadline = time.monotonic() + total + 120
        rss_samples = []
        joined_configured = False
        after_join_applied = False
        mutation_applied = False
        timed_out = False
        print(
            f"trial {trial}: {workload}, debug_overlay={debug_overlay}, "
            f"user={username}, durations={durations}",
            flush=True,
        )
        with log_path.open("w", encoding="utf-8") as log_handle:
            process = subprocess.Popen(
                command,
                cwd=ROOT,
                env=env,
                stdout=log_handle,
                stderr=subprocess.STDOUT,
            )
            while process.poll() is None:
                rss = process_rss_bytes(process.pid)
                if rss is not None:
                    rss_samples.append(rss)
                if not joined_configured:
                    log_text = log_path.read_text(encoding="utf-8", errors="replace")
                    if f'segment="{workload}.warmup"' in log_text or f"segment={workload}.warmup" in log_text:
                        configure_joined_player(
                            workload, oracle["rcon_port"], username
                        )
                        joined_configured = True
                        if heavy_scene is not None:
                            run_rcon_commands(
                                oracle["rcon_port"], "after_join", heavy_scene["commands"]["after_join"]
                            )
                            after_join_applied = True
                if heavy_scene is not None and not mutation_applied:
                    log_text = log_path.read_text(encoding="utf-8", errors="replace")
                    if ('segment="heavyweight.mutation"' in log_text
                            or "segment=heavyweight.mutation" in log_text):
                        run_rcon_commands(
                            oracle["rcon_port"], "mutation", heavy_scene["commands"]["mutation"]
                        )
                        mutation_applied = True
                if time.monotonic() >= deadline:
                    timed_out = True
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
                    break
                time.sleep(0.05 if not joined_configured else 0.25)
            return_code = process.wait()

        log_text = log_path.read_text(encoding="utf-8", errors="replace")
        if timed_out:
            raise RuntimeError(f"benchmark exceeded its {total + 120}s deadline; log: {log_path}")
        if return_code != 0:
            tail = "\n".join(log_text.splitlines()[-40:])
            raise RuntimeError(f"client exited {return_code}:\n{tail}")
        if samply_artifact is not None:
            sidecar = _samply_sidecar_path(samply_artifact)
            if not samply_artifact.is_file() or samply_artifact.stat().st_size == 0:
                raise RuntimeError(f"samply exited without a nonempty capture: {samply_artifact}")
            if not sidecar.is_file() or sidecar.stat().st_size == 0:
                raise RuntimeError(f"samply exited without a nonempty symbol sidecar: {sidecar}")
        if not joined_configured:
            raise RuntimeError("client never reached a configurable joined warmup")
        if heavy_scene is not None and not after_join_applied:
            raise RuntimeError("heavyweight after_join phase was never observed")
        if heavy_scene is not None and heavy_scene["commands"]["mutation"] and not mutation_applied:
            raise RuntimeError("heavyweight mutation segment was never observed")
        if not csv_path.exists():
            raise RuntimeError("client exited without writing its frame CSV")

        rows = _read_csv(csv_path)
        framebuffer = validate_run(rows, log_text, workload)
        segments = {}
        for suffix in ("stationary", "moving"):
            label = f"{workload}.{suffix}"
            segments[suffix] = summarize_rows(
                [row for row in rows if row.get("segment") == label]
            )
        if heavy_scene is not None:
            mutation_rows = [row for row in rows if row.get("segment") == "heavyweight.mutation"]
            if mutation_rows:
                segments["mutation"] = summarize_rows(mutation_rows)
            _validate_heavy_witnesses(
                heavy_scene,
                {f"heavyweight.{name}": value for name, value in segments.items()},
            )
        rss = {
            "start": rss_samples[0] if rss_samples else 0,
            "peak": max(rss_samples, default=0),
            "end": rss_samples[-1] if rss_samples else 0,
        }
        return {
            "trial": trial,
            "username": username,
            "debug_overlay": debug_overlay,
            "segments": segments,
            "rss_bytes": rss,
            "framebuffer": framebuffer,
            "gpu_timestamp_ms": summarize_gpu_log(log_text),
            "log": log_text,
        }


def _validate_heavy_witnesses(scene: dict, segments: dict[str, dict]) -> None:
    """Require every emitter-owned witness to have appeared in its stated phase."""
    for witness in scene["witnesses"]:
        segment = segments.get(witness["segment"])
        if segment is None:
            raise RuntimeError(f"heavyweight witness segment missing: {witness['segment']}")
        observed = segment["workload_counts"].get(witness["column"], {}).get("max", 0.0)
        if observed < witness["minimum"]:
            raise RuntimeError(
                f"heavyweight witness {witness['column']} in {witness['segment']} "
                f"requires {witness['minimum']}, maximum {observed:g}"
            )


def _git_sha() -> str:
    return subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=True,
    ).stdout.strip()


def _print_trial(workload: str, result: dict) -> None:
    rss = result["rss_bytes"]
    print(
        f"trial {result['trial']} debug_overlay={result['debug_overlay']} "
        f"RSS MiB start/peak/end: "
        f"{rss['start']/1048576:.1f}/{rss['peak']/1048576:.1f}/{rss['end']/1048576:.1f}"
    )
    for suffix, summary in result["segments"].items():
        misses_60 = summary["over_16_67"]
        misses_30 = summary["over_33_3"]
        print(
            f"  {workload}.{suffix}: n={summary['frames']} "
            f"p50/p95/p99={summary['p50_ms']:.2f}/"
            f"{summary['p95_ms']:.2f}/{summary['p99_ms']:.2f} ms "
            f">16.67={misses_60} >33.3={misses_30}"
        )
        phase_line = " ".join(
            f"{name}={value:.2f}"
            for name, value in sorted(summary["phases_ms"].items())
        )
        print(f"    phase means ms: {phase_line}")
        if summary["workload_counts"]:
            count_line = " ".join(
                f"{name}=median/p95/max "
                f"{values['median']:.0f}/{values['p95']:.0f}/{values['max']:.0f}"
                for name, values in sorted(summary["workload_counts"].items())
            )
            print(f"    workload counts: {count_line}")
    gpu = result["gpu_timestamp_ms"]
    if gpu["samples"]:
        print(f"  GPU timestamp snapshots: n={gpu['samples']}")
        for name in ("world", "first_person", "world_total", "hud_total"):
            values = gpu[name]
            qualifier = " (diagnostic span)" if name.endswith("_total") else ""
            print(
                f"    {name}{qualifier}: median/p95="
                f"{values['median_ms']:.2f}/{values['p95_ms']:.2f} ms"
            )


def _append_records(
    workload: str,
    result: dict,
    durations: tuple[int, int, int],
    binary: pathlib.Path,
) -> None:
    RESULTS.parent.mkdir(parents=True, exist_ok=True)
    common = {
        "git_sha": _git_sha(),
        "machine": platform.platform(),
        "arch": platform.machine(),
        "profile": "release",
        "scene": workload,
        "debug_overlay": result["debug_overlay"],
        "trial": result["trial"],
        "binary": str(binary),
        "render_distance": RENDER_DISTANCE,
        "framebuffer": list(result["framebuffer"]),
        "durations_seconds": {
            "warmup": durations[0],
            "stationary": durations[1],
            "moving": durations[2],
        },
        "rss_bytes": result["rss_bytes"],
        "gpu_timestamp_ms": result["gpu_timestamp_ms"],
    }
    with RESULTS.open("a", encoding="utf-8") as handle:
        for suffix, summary in result["segments"].items():
            record = {**common, "segment": f"{workload}.{suffix}", **summary}
            handle.write(json.dumps(record, separators=(",", ":"), sort_keys=True) + "\n")


def _heavy_record(
    workload: str, trial: int, binary: pathlib.Path, durations: tuple[int, ...],
    debug_overlay: str, requested_scale: int, camera_plan: str, scene: dict, segments: dict,
) -> dict:
    spec = scene["spec"]
    return {
        "schema": 2,
        "workload": workload,
        "trial": trial,
        "profile": "release",
        "git_sha": _git_sha(),
        "machine": platform.platform(),
        "arch": platform.machine(),
        "binary": str(binary),
        "scenario": spec["scenario"],
        "seed": spec["seed"],
        "requested_scale": requested_scale,
        "scale": spec["scale"],
        "camera_plan": camera_plan,
        "scene_hash": scene["scene_hash"],
        "debug_overlay": debug_overlay,
        "durations_seconds": dict(zip(("warmup", "mutation", "stationary", "moving"), durations)),
        "segments": segments,
    }


def _print_spread(workload: str, debug_overlay: str, results: list[dict]) -> None:
    if len(results) < 2:
        return
    print(f"trial spread (p50 frame interval, debug_overlay={debug_overlay}):")
    for suffix in ("stationary", "moving"):
        values = [result["segments"][suffix]["p50_ms"] for result in results]
        print(
            f"  {workload}.{suffix}: min/median/max="
            f"{min(values):.2f}/{statistics.median(values):.2f}/{max(values):.2f} ms"
        )


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workload", choices=sorted(ORACLES))
    parser.add_argument(
        "--validate-heavy-profile",
        type=pathlib.Path,
        metavar="CAPTURE",
        help="validate a saved heavyweight Samply capture and its sidecars without running a scene",
    )
    parser.add_argument("--trials", type=int, default=3)
    parser.add_argument("--smoke", action="store_true")
    parser.add_argument("--samply", action="store_true")
    parser.add_argument("--heavy-scenario", choices=HEAVY_SCENARIOS)
    parser.add_argument("--heavy-seed", type=int, default=1)
    parser.add_argument("--heavy-scale", type=int, default=2)
    parser.add_argument("--heavy-camera-plan", choices=HEAVY_CAMERA_PLANS, default="orbit")
    parser.add_argument("--heavy-mutation-seconds", type=int, default=7)
    parser.add_argument("--debug-overlay", choices=("closed", "open", "both"))
    parser.add_argument(
        "--binary", type=pathlib.Path, default=ROOT / "target" / "release" / "lodestone"
    )
    args = parser.parse_args(argv)
    if args.validate_heavy_profile is not None:
        if args.workload is not None:
            parser.error("--validate-heavy-profile cannot be combined with --workload")
        return args
    if args.workload is None:
        parser.error("--workload is required unless --validate-heavy-profile is used")
    if args.trials < 1:
        parser.error("--trials must be at least 1")
    if (args.workload == "heavyweight") != (args.heavy_scenario is not None):
        parser.error("--heavy-scenario is required exactly with --workload heavyweight")
    if args.heavy_seed < 0:
        parser.error("--heavy-seed must be non-negative")
    if not 1 <= args.heavy_scale <= MAX_HEAVY_SCALE:
        parser.error(f"--heavy-scale must be in 1..={MAX_HEAVY_SCALE}")
    scenario_max_scale = MAX_HEAVY_SCALE_BY_SCENARIO.get(args.heavy_scenario, MAX_HEAVY_SCALE)
    if args.heavy_scale > scenario_max_scale:
        parser.error(
            f"--heavy-scale for {args.heavy_scenario} must be in 1..={scenario_max_scale}; "
            "its fixed density already bounds local setup"
        )
    if not 0 <= args.heavy_mutation_seconds <= MAX_HEAVY_MUTATION_SECONDS:
        parser.error(
            f"--heavy-mutation-seconds must be in 0..={MAX_HEAVY_MUTATION_SECONDS}"
        )
    return args


def overlay_arms(workload: str, requested: str | None) -> list[str]:
    policy = requested or ("both" if workload == "megaworld" else "closed")
    return ["closed", "open"] if policy == "both" else [policy]


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if args.validate_heavy_profile is not None:
        record = validate_heavy_profile_artifact(args.validate_heavy_profile)
        print(
            "validated heavyweight profile: "
            f"scenario={record['scenario']} scene_hash={record['scene_hash']}"
        )
        return 0
    binary = args.binary.resolve()
    if not binary.is_file():
        raise SystemExit(f"release client not found: {binary}; build it before benchmarking")
    if args.samply and shutil.which("samply") is None:
        raise SystemExit("--samply requested but samply is not on PATH")

    if args.workload == "heavyweight":
        durations = (2, 1, 2, 3) if args.smoke else (20, args.heavy_mutation_seconds, 30, 60)
        if sum(durations) > MAX_HEAVY_TOTAL_SECONDS:
            parser.error(f"heavyweight duration must be at most {MAX_HEAVY_TOTAL_SECONDS}s")
    else:
        durations = (2, 2, 3) if args.smoke else (
            (45, 30, 60) if args.workload in ("megaworld", "lovelier") else (20, 30, 60)
        )
    trial_count = 1 if args.smoke or args.samply or args.workload == "heavyweight" else args.trials
    arms = overlay_arms(args.workload, args.debug_overlay)
    if args.samply and len(arms) != 1:
        raise SystemExit(
            "--samply with megaworld requires --debug-overlay closed or open"
        )
    oracle = start_oracle(args.workload)
    if args.workload == "showcase":
        prepare_showcase(oracle["rcon_port"])
    heavy_scene = None
    emitted_scale = 1 if args.smoke else args.heavy_scale
    if args.workload == "heavyweight":
        heavy_scene = prepare_heavy_scene(
            oracle["rcon_port"], HEAVY_SCENE_EMITTER, args.heavy_scenario,
            args.heavy_seed, emitted_scale, args.heavy_camera_plan,
        )

    profile_artifact = None
    if args.samply:
        profiles = ROOT / "bench-results" / "profiles"
        profiles.mkdir(parents=True, exist_ok=True)
        stamp = time.strftime("%Y%m%d-%H%M%S")
        profile_artifact = profiles / f"{args.workload}-{arms[0]}-{stamp}.json.gz"

    for debug_overlay in arms:
        results = []
        for trial in range(1, trial_count + 1):
            result = run_trial(
                args.workload,
                trial,
                binary,
                oracle,
                durations,
                debug_overlay,
                samply_artifact=profile_artifact,
                heavy_scene=heavy_scene,
                camera_plan=args.heavy_camera_plan if heavy_scene is not None else None,
            )
            _print_trial(args.workload, result)
            if args.workload == "heavyweight":
                record = _heavy_record(
                    args.workload, trial, binary, durations, debug_overlay, args.heavy_scale,
                    args.heavy_camera_plan, heavy_scene, result["segments"],
                )
                if profile_artifact is not None:
                    record_path = _heavy_profile_record_path(profile_artifact)
                    record_path.write_text(
                        json.dumps({**record, "capture": str(profile_artifact)}, separators=(",", ":"), sort_keys=True) + "\n",
                        encoding="utf-8",
                    )
                    validate_heavy_profile_artifact(profile_artifact)
                    print(f"heavyweight capture record: {record_path}")
                else:
                    HEAVY_RESULTS.parent.mkdir(parents=True, exist_ok=True)
                    with HEAVY_RESULTS.open("a", encoding="utf-8") as handle:
                        handle.write(json.dumps(record, separators=(",", ":"), sort_keys=True) + "\n")
            elif not args.samply:
                _append_records(args.workload, result, durations, binary)
                results.append(result)
        _print_spread(args.workload, debug_overlay, results)
    if profile_artifact is not None:
        print(f"samply profile: {profile_artifact}")
    elif args.workload == "heavyweight":
        print(f"appended heavyweight scene evidence to {HEAVY_RESULTS}")
    else:
        print(f"appended comparable records to {RESULTS}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as exc:
        print(f"benchmark failed: {exc}", file=sys.stderr)
        raise SystemExit(1)
