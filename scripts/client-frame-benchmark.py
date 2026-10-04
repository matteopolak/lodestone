#!/usr/bin/env python3
"""Run and summarize deterministic Java-backed Lodestone frame benchmarks.

The runner stays in the foreground for its entire child process lifetime. It
uses only the Python standard library so a fresh checkout needs no virtualenv.
"""

from __future__ import annotations

import argparse
import contextlib
import csv
import dataclasses
import hashlib
import importlib.util
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
RCON_COMMAND_TIMEOUT_SECONDS = 15
# Dense scenes deliberately retain their complete 7,937-action setup even for
# a smoke run. One deadline for the reload-plus-function transaction makes a
# stalled local oracle fail predictably instead of multiplying the socket
# timeout by every scene action.
HEAVY_SETUP_DEADLINE_SECONDS = 90
HEAVY_DATAPACK_FORMAT = 107
RENDER_DISTANCE = 24
SETTINGS_INTEGER_RANGES = {
    "framerate_limit": (10, 260), "fov": (30, 110),
    "render_distance": (2, 256), "biome_blend_radius": (0, 7),
}
SETTINGS_BOOLEAN_FIELDS = {"enable_vsync", "cutout_leaves", "entity_shadows"}
SETTINGS_ENUM_VALUES = {
    "inactivity_fps_limit": {"minimized", "afk"},
    "graphics_preset": {"fast", "fancy", "fabulous", "custom"},
    "cloud_status": {"off", "fast", "fancy"},
    "particles": {"all", "decreased", "minimal"},
}
SETTINGS_FIELDS = set(SETTINGS_INTEGER_RANGES) | SETTINGS_BOOLEAN_FIELDS | set(SETTINGS_ENUM_VALUES)
PRESENTATION_COLUMNS = [
    "attempt", "startedUs", "finishedUs", "outcome", "submission", "intervalUs",
    "targetFps", "vsync", "gpuCompletionCallbackUs",
]
MAX_SNAPSHOT_FILES = 512
MAX_SNAPSHOT_BYTES = 1 << 30
METADATA_COLUMNS = {"frame", "frame_interval_ms", "segment"}
COUNT_COLUMNS = {
    "world.terrain_camera_bind_calls",
    "world.terrain_origin_vertex_binds",
    "world.terrain_indexed_draw_calls",
    "world.terrain_buffer_bind_pairs",
    "primary.encoders_created",
    "primary.encoders_finished",
    "primary.queue_submissions",
    "world.packed_sections_visited",
    "world.model_sections_visited",
    "world.opaque_sections_drawn",
    "world.water_sections_drawn",
    "world.translucent_sections_drawn",
    "world.entities_drawn",
    "world.block_entities_drawn",
    "world.sign_text_vertices",
    "world.particles_drawn",
    "world.world_pass_begins",
    "world.world_text_pass_begins",
    "world.nametag_pass_begins",
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
    r"gpu: timer=(?P<timer>\d+) frame=(?P<frame>\d+) age_frames=(?P<age>\d+) "
    r"world=(?P<world>\d+(?:\.\d+)?ms|not_run|invalid|map_error) "
    r"first_person=(?P<first_person>\d+(?:\.\d+)?ms|not_run|invalid|map_error)"
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
    if not intervals or any(not math.isfinite(value) or value <= 0 for value in intervals):
        raise ValueError("measured frame intervals must be positive and finite")
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
    """Summarize distinct asynchronous samples, never held readings or totals."""
    samples = {}
    for match in GPU_SAMPLE_RE.finditer(log_text):
        sample = match.groupdict()
        identity = (int(sample["timer"]), int(sample["frame"]))
        samples.setdefault(identity, sample)
    summary = {"samples": len(samples), "calibration_verified": False}
    for name in ("world", "first_person"):
        values = [float(sample[name][:-2]) for sample in samples.values() if sample[name].endswith("ms")]
        if values:
            summary[name] = {
                "samples": len(values),
                "median_ms": statistics.median(values),
                "p95_ms": nearest_rank(values, 0.95),
            }
    return summary


def validate_run(
    rows: list[Mapping[str, str]], log_text: str, workload: str,
    benchmark_window: str = "builtin-fullscreen",
    benchmark_pacing: str = "uncapped", settings: dict | None = None,
    expected_resolution: tuple[int, int] = (2560, 1440),
) -> tuple[int, int]:
    """Reject incomplete, mislabeled, or no-op runs before they enter history.

    ``world.model_sections_visited`` is emitted by the production client's
    world submission path. Requiring a positive sample in both measured
    segments keeps a disconnected or skipped render bridge from becoming a
    seemingly valid timing record.
    """
    if "benchmark complete" not in log_text:
        raise ValueError("completion marker missing from client log")
    if benchmark_window not in ("builtin-fullscreen", "windowed"):
        raise ValueError(f"unknown benchmark window policy: {benchmark_window}")
    if benchmark_pacing not in ("uncapped", "options"):
        raise ValueError(f"unknown benchmark pacing policy: {benchmark_pacing}")
    if benchmark_window == "builtin-fullscreen" and "selected hardware built-in display for fullscreen benchmark" not in log_text:
        raise ValueError("client log does not confirm hardware built-in display selection")
    observed = _observed_trial_metadata(rows, log_text)
    window = observed["window"]
    if any(window.get(name) is None for name in ("framebuffer_width", "framebuffer_height", "fullscreen")):
        raise ValueError("client log does not contain parseable framebuffer/fullscreen metadata")
    if benchmark_window == "builtin-fullscreen" and not window["fullscreen"]:
        raise ValueError("client log does not confirm fullscreen presentation")
    framebuffer = (window["framebuffer_width"], window["framebuffer_height"])
    if framebuffer[0] <= 0 or framebuffer[1] <= 0:
        raise ValueError(f"invalid physical framebuffer size: {framebuffer}")
    if benchmark_window == "windowed" and (framebuffer != expected_resolution or window["fullscreen"]):
        raise ValueError(f"windowed benchmark requires a physical {expected_resolution[0]}x{expected_resolution[1]} framebuffer and fullscreen=false")
    if settings is not None or benchmark_window != "builtin-fullscreen" or benchmark_pacing != "uncapped":
        if settings is None:
            raise ValueError("explicit comparison policies require declared settings")
        validate_settings(settings)
        if observed["effective_graphics_settings"] != settings:
            raise ValueError("effective graphics settings are missing or differ from declared settings")
        if window.get("benchmark_window") != benchmark_window or window.get("benchmark_pacing") != benchmark_pacing:
            raise ValueError("observed benchmark policy is missing or differs from the requested policy")
        cap, vsync = _requested_pacing(settings, benchmark_pacing)
        if not window.get("target_fps_observed") or window.get("target_fps") != cap or window.get("effective_vsync") is not vsync:
            raise ValueError("observed effective pacing differs from the requested policy")
        if window.get("configured_present_mode") is None:
            raise ValueError("configured present mode observation is missing")
        if not vsync and window["configured_present_mode"] != "AutoNoVsync":
            raise ValueError("configured present mode differs from the no-VSync request")

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

    for suffix in ("stationary", "moving"):
        label = f"{workload}.{suffix}"
        segment_rows = [row for row in rows if row.get("segment") == label]
        model_sections = []
        for row in segment_rows:
            raw = row.get("world.model_sections_visited", "").strip()
            if not raw:
                continue
            try:
                value = float(raw)
            except ValueError as error:
                raise ValueError(
                    f"{label} has a non-numeric world.model_sections_visited value: {raw!r}"
                ) from error
            if not math.isfinite(value) or value < 0 or not value.is_integer():
                raise ValueError(
                    f"{label} has an invalid world.model_sections_visited value: {raw!r}"
                )
            model_sections.append(value)
        if not model_sections:
            raise ValueError(
                f"{label} has no world.model_sections_visited samples; "
                "the production render submission path was not observed"
            )
        if max(model_sections) <= 0:
            raise ValueError(
                f"{label} has zero world.model_sections_visited samples; "
                "the production render submission path did no work"
            )
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

    def __init__(self, port: int, timeout_seconds: float = RCON_COMMAND_TIMEOUT_SECONDS):
        self.port = port
        self.timeout_seconds = timeout_seconds
        self.sock: socket.socket | None = None
        self.request_id = 10

    def __enter__(self) -> "RconClient":
        self.sock = socket.create_connection(
            ("127.0.0.1", self.port), timeout=self.timeout_seconds
        )
        self.sock.settimeout(self.timeout_seconds)
        self.sock.sendall(_build_frame(1, 3, RCON_PASSWORD))
        response_id, _ = _read_response(self.sock)
        if response_id != 1:
            raise RuntimeError("RCON authentication failed")
        return self

    def __exit__(self, *_exc) -> None:
        if self.sock is not None:
            self.sock.close()
            self.sock = None

    def command(self, command: str, timeout_seconds: float | None = None) -> str:
        if self.sock is None:
            raise RuntimeError("RCON client is not connected")
        if timeout_seconds is not None:
            self.sock.settimeout(timeout_seconds)
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


def start_oracle(workload: str, render_distance: int = RENDER_DISTANCE) -> dict:
    oracle = ORACLES[workload]
    server_view_distance = render_distance + 1
    print(
        f"starting {workload} Java oracle with server view distance "
        f"{server_view_distance}...",
        flush=True,
    )
    with _server_view_distance(oracle["world"], server_view_distance):
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


def _rcon_deadline_remaining(phase: str, action: int, deadline: float | None) -> float:
    if deadline is None:
        return RCON_COMMAND_TIMEOUT_SECONDS
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise TimeoutError(f"{phase} RCON deadline expired before action {action}")
    return min(remaining, RCON_COMMAND_TIMEOUT_SECONDS)


def run_rcon_commands(
    rcon_port: int, phase: str, commands: list[str], deadline: float | None = None
) -> list[str]:
    """Submit a batch in order, optionally under one deadline for every request."""
    replies = []
    with RconClient(rcon_port, _rcon_deadline_remaining(phase, 0, deadline)) as rcon:
        for index, command in enumerate(commands, 1):
            try:
                reply = rcon.command(
                    command, _rcon_deadline_remaining(phase, index, deadline)
                )
            except (socket.timeout, TimeoutError) as error:
                raise TimeoutError(
                    f"{phase} RCON deadline expired at action {index}/{len(commands)}"
                ) from error
            if _is_scene_error(command, reply):
                raise RuntimeError(
                    f"{phase} command {index}/{len(commands)} failed:\n"
                    f"  {command}\n  {reply or '(empty response)'}"
                )
            replies.append(reply)
    return replies


@dataclasses.dataclass
class HeavySetupDatapack:
    """One runner-owned function pack, removed after the local benchmark."""

    root: pathlib.Path
    function_id: str
    objective: str

    def cleanup(self) -> None:
        shutil.rmtree(self.root)


def _heavy_setup_function_lines(commands: list[str], objective: str) -> list[str]:
    """Keep each producer command intact while proving the aggregate succeeded."""
    lines = [
        f"scoreboard objectives add {objective} dummy",
        f"scoreboard players set #successful {objective} 0",
    ]
    successful_commands = 0
    for command in commands:
        # The plan's idempotent stale-entity cleanup is permitted to find no
        # entities. Every scene producer, in contrast, must report success.
        if command.startswith("kill @e[tag=lodestone_heavy_scene]"):
            lines.append(command)
            continue
        successful_commands += 1
        lines.extend((
            f"execute store success score #last {objective} run {command}",
            f"execute if score #last {objective} matches 1 run scoreboard players add #successful {objective} 1",
        ))
    lines.extend((
        f"execute if score #successful {objective} matches {successful_commands} run return {successful_commands}",
        "return 0",
    ))
    return lines


def _write_heavy_setup_datapack(world: pathlib.Path, scene: dict) -> HeavySetupDatapack:
    """Materialize the immutable setup list as one normal server function call."""
    datapacks = world / "datapacks"
    datapacks.mkdir(parents=True, exist_ok=True)
    root = pathlib.Path(tempfile.mkdtemp(prefix="lodestone-heavy-", dir=datapacks))
    nonce = root.name.rsplit("-", 1)[-1].replace("-", "_")
    function_name = f"setup_{scene['scene_hash'][:16]}_{nonce}"
    function_id = f"lodestone_heavy:{function_name}"
    objective = f"lh{scene['scene_hash'][:12]}"
    try:
        (root / "pack.mcmeta").write_text(
            json.dumps(
                {
                    "pack": {
                        "description": "temporary Lodestone heavyweight benchmark setup",
                        "min_format": [HEAVY_DATAPACK_FORMAT, 1],
                        "max_format": HEAVY_DATAPACK_FORMAT,
                    }
                },
                separators=(",", ":"),
            )
            + "\n",
            encoding="utf-8",
        )
        function = (
            root / "data" / "lodestone_heavy" / "function" / f"{function_name}.mcfunction"
        )
        function.parent.mkdir(parents=True)
        function.write_text(
            "\n".join(_heavy_setup_function_lines(scene["commands"]["setup"], objective))
            + "\n",
            encoding="utf-8",
        )
    except BaseException:
        shutil.rmtree(root)
        raise
    return HeavySetupDatapack(root, function_id, objective)


def _function_success_count(reply: str) -> int | None:
    match = re.search(r"returned\s+(\d+)", reply, re.IGNORECASE)
    return int(match.group(1)) if match else None


def prepare_heavy_scene(
    rcon_port: int, world: pathlib.Path, emitter: pathlib.Path, scenario: str, seed: int,
    scale: int, camera_plan: str,
) -> tuple[dict, HeavySetupDatapack]:
    scene = _emit_heavy_scene(emitter, scenario, seed, scale, camera_plan)
    datapack = _write_heavy_setup_datapack(world, scene)
    deadline = time.monotonic() + HEAVY_SETUP_DEADLINE_SECONDS
    function_called = False
    try:
        run_rcon_commands(rcon_port, "setup.reload", ["reload"], deadline)
        # Mark this before waiting for the reply: a socket timeout can happen
        # after the server has already created the temporary objective.
        function_called = True
        replies = run_rcon_commands(
            rcon_port, "setup", [f"function {datapack.function_id}"], deadline
        )
        expected = sum(
            not command.startswith("kill @e[tag=lodestone_heavy_scene]")
            for command in scene["commands"]["setup"]
        )
        successful = _function_success_count(replies[0])
        if successful != expected:
            raise RuntimeError(
                f"setup function reported {successful!r} successful producers, expected {expected}"
            )
        run_rcon_commands(
            rcon_port, "setup.cleanup", [f"scoreboard objectives remove {datapack.objective}"], deadline
        )
    except BaseException:
        if function_called:
            with contextlib.suppress(Exception):
                run_rcon_commands(
                    rcon_port,
                    "setup.cleanup",
                    [f"scoreboard objectives remove {datapack.objective}"],
                    deadline,
                )
        datapack.cleanup()
        raise
    return scene, datapack


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


def _resource_sampler(pid: int, profiled: bool):
    spec = importlib.util.spec_from_file_location(
        "lodestone_client_resources", ROOT / "scripts" / "client-resource-sampler.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    scope = "Samply launcher and its client descendants" if profiled else "native client and its descendants"
    return module.ProcessTreeSampler([pid], association=f"Popen-returned PID: {scope}")


def _sha256_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _identity_digest(value: object) -> str:
    return hashlib.sha256(
        json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()


def validate_settings(settings: object) -> dict:
    if not isinstance(settings, dict) or set(settings) != SETTINGS_FIELDS:
        raise ValueError("settings require exactly these graphics fields: " + ", ".join(sorted(SETTINGS_FIELDS)))
    for name, (minimum, maximum) in SETTINGS_INTEGER_RANGES.items():
        value = settings[name]
        if type(value) is not int or not minimum <= value <= maximum:
            raise ValueError(f"settings {name} must be an integer in {minimum}..={maximum}")
    for name in SETTINGS_BOOLEAN_FIELDS:
        if type(settings[name]) is not bool:
            raise ValueError(f"settings {name} must be boolean")
    for name, values in SETTINGS_ENUM_VALUES.items():
        if not isinstance(settings[name], str) or settings[name] not in values:
            raise ValueError(f"settings {name} must be one of {', '.join(sorted(values))}")
    return dict(settings)


def _unique_json_object(pairs: list[tuple[str, object]]) -> dict:
    value = {}
    for name, item in pairs:
        if name in value:
            raise ValueError(f"duplicate JSON field: {name}")
        value[name] = item
    return value


def _read_settings(path: pathlib.Path) -> dict:
    if path.stat().st_size > 65536:
        raise ValueError("settings declaration exceeds 64 KiB")
    return validate_settings(json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=_unique_json_object))


def _requested_pacing(settings: dict | None, policy: str) -> tuple[int | None, bool]:
    if policy == "uncapped":
        return None, False
    if policy != "options" or settings is None:
        raise ValueError("options pacing requires declared settings")
    cap = settings["framerate_limit"]
    return (None if cap == 260 else cap), settings["enable_vsync"]


def _write_trial_options(workspace: pathlib.Path, settings: dict) -> str:
    payload = json.dumps(validate_settings(settings), indent=2, sort_keys=True) + "\n"
    (workspace / "data" / "options.json").write_text(payload, encoding="utf-8")
    retained = workspace / "options.json"
    retained.write_text(payload, encoding="utf-8")
    return _sha256_file(retained)


def _require_offline_oracle(oracle: dict) -> None:
    for port in (oracle["game_port"], oracle["rcon_port"]):
        try:
            connection = socket.create_connection(("127.0.0.1", port), timeout=1)
        except ConnectionRefusedError:
            continue
        except OSError as error:
            raise RuntimeError(f"cannot establish offline snapshot state on port {port}") from error
        connection.close()
        raise RuntimeError(f"stop the oracle on port {port} before hashing its world snapshot")


def _world_snapshot_identity(declaration: pathlib.Path, oracle: dict) -> dict:
    """Hash only an explicit bounded set of offline world files, without copying."""
    if declaration.stat().st_size > 1024 * 1024:
        raise ValueError("world snapshot declaration exceeds 1 MiB")
    plan = json.loads(declaration.read_text(encoding="utf-8"))
    if (not isinstance(plan, dict) or type(plan.get("schema")) is not int or plan["schema"] != 1
            or set(plan) - {"schema", "files", "snapshot_sha256"}):
        raise ValueError("world snapshot declaration requires schema 1 and files")
    files = plan.get("files")
    if (not isinstance(files, list) or not 1 <= len(files) <= MAX_SNAPSHOT_FILES
            or any(not isinstance(name, str) or not name for name in files)):
        raise ValueError(f"world snapshot requires 1..={MAX_SNAPSHOT_FILES} named files")
    if len(set(files)) != len(files):
        raise ValueError("world snapshot file names must be unique")
    expected = plan.get("snapshot_sha256")
    if expected is not None and (not isinstance(expected, str) or not re.fullmatch(r"[0-9a-f]{64}", expected)):
        raise ValueError("world snapshot expected digest must be lowercase SHA-256")
    world = oracle["world"] / "world"
    if world.is_symlink():
        raise ValueError("world snapshot root must not be a symbolic link")
    root = world.resolve(strict=True)
    _require_offline_oracle(oracle)
    entries = []
    stamps = []
    total = 0
    for name in sorted(files):
        relative = pathlib.PurePosixPath(name)
        if relative.is_absolute() or ".." in relative.parts or relative.as_posix() != name or "\\" in name:
            raise ValueError(f"world snapshot path must be canonical and relative: {name!r}")
        path = root.joinpath(*relative.parts)
        if any(root.joinpath(*relative.parts[:i]).is_symlink() for i in range(1, len(relative.parts) + 1)):
            raise ValueError(f"world snapshot path is a symbolic link: {name!r}")
        if not path.is_file() or not path.resolve(strict=True).is_relative_to(root):
            raise ValueError(f"world snapshot path is not a contained regular file: {name!r}")
        before = path.stat()
        total += before.st_size
        if total > MAX_SNAPSHOT_BYTES:
            raise ValueError("declared world snapshot exceeds 1 GiB; declare the bounded scene files")
        digest = _sha256_file(path)
        after = path.stat()
        if (before.st_size, before.st_mtime_ns, before.st_ino) != (after.st_size, after.st_mtime_ns, after.st_ino):
            raise RuntimeError(f"world snapshot changed while hashing: {name}")
        entries.append({"path": name, "bytes": before.st_size, "sha256": digest})
        stamps.append((path, after))
    for path, before in stamps:
        after = path.stat()
        if (before.st_size, before.st_mtime_ns, before.st_ino) != (after.st_size, after.st_mtime_ns, after.st_ino):
            raise RuntimeError(f"world snapshot changed while hashing: {path.relative_to(root)}")
    _require_offline_oracle(oracle)
    digest = _identity_digest({"schema": 1, "files": entries})
    if expected is not None and digest != expected:
        raise ValueError(f"world snapshot digest mismatch: expected {expected}, got {digest}")
    return {
        "sha256": digest, "root": str(root), "files": entries, "bytes": total,
        "boundary": "offline before oracle launch",
        "coverage": "declared files only; complete world coverage is not inferred",
        "trial_restore_verified": False,
    }


def _comparison_identity(
    binary: pathlib.Path, workload: str, snapshot: dict | None,
    settings: dict | None = None, benchmark_window: str = "builtin-fullscreen",
    benchmark_pacing: str = "uncapped",
    benchmark_resolution: tuple[int, int] | None = None,
) -> dict:
    settings = validate_settings(settings) if settings is not None else None
    distance = settings["render_distance"] if settings is not None else RENDER_DISTANCE
    cap, vsync = _requested_pacing(settings, benchmark_pacing)
    asset_root = os.environ.get("LODESTONE_ASSETS")
    resources = {"explicit_root": asset_root, "official_provenance_verified": False, "files": {}}
    if asset_root:
        for name in ("client.jar", "lodestone-resources.zip", "generated/reports/blocks.json"):
            path = pathlib.Path(asset_root) / name
            resources["files"][name] = _sha256_file(path) if path.is_file() else None
    identity = {
        "checkout_git_sha": _git_sha(), "binary_sha256": _sha256_file(binary),
        "build_profile_requested": "release", "binary_build_profile_verified": None,
        "workload": workload, "protocol_requested": 776, "game_release_requested": "26.2",
        "world_snapshot": snapshot,
        "resources": resources, "render_distance_requested": distance,
        "server_view_distance_requested": distance + 1,
        "simulation_distance_observed": None, "effective_graphics_settings": None,
        "graphics_settings_requested": settings,
        "graphics_settings_sha256": _identity_digest(settings) if settings is not None else None,
        "benchmark_window_requested": benchmark_window, "benchmark_pacing_requested": benchmark_pacing,
        "benchmark_resolution_requested": benchmark_resolution or (2560, 1440),
        "requested_frame_cap": "uncapped" if cap is None else cap,
        "requested_vsync": vsync,
        "requested_present_mode": None if vsync else "AutoNoVsync",
        "requested_present_mode_policy": "surface default" if vsync else "AutoNoVsync",
        "actual_present_mode": None, "gpu_adapter": None,
        "machine": platform.platform(), "arch": platform.machine(),
        "showcase_commands_sha256": _sha256_file(SCENE) if workload == "showcase" else None,
        "choreography_source_sha256": _sha256_file(ROOT / "crates/lodestone-shell/src/app/benchmark.rs"),
        "choreography_binary_link_verified": False,
    }
    return {"sha256": _identity_digest(identity), **identity}


def _observed_trial_metadata(rows: list[Mapping[str, str]], log_text: str) -> dict:
    plain_log = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", log_text)
    ready = re.search(r"[^\n]*benchmark window ready[^\n]*", plain_log)
    window = {}
    settings = {}
    if ready:
        fields = {
            name: quoted if quoted else bare
            for name, quoted, bare in re.findall(r'\b([a-z_]+)=(?:"([^"\n]*)"|([^\s,]+))', ready.group())
        }
        for name in ("framebuffer_width", "framebuffer_height", *SETTINGS_INTEGER_RANGES):
            value = fields.get(name, "")
            parsed = int(value) if re.fullmatch(r"\d+", value) else None
            if name in SETTINGS_INTEGER_RANGES:
                settings[name] = parsed
            if name in ("framebuffer_width", "framebuffer_height", "render_distance"):
                window[name] = parsed
        for name in ("fullscreen", "effective_vsync", *sorted(SETTINGS_BOOLEAN_FIELDS)):
            value = fields.get(name)
            parsed = value == "true" if value in ("true", "false") else None
            if name in SETTINGS_BOOLEAN_FIELDS:
                settings[name] = parsed
            else:
                window[name] = parsed
        for name in SETTINGS_ENUM_VALUES:
            settings[name] = fields.get(name)
        for name in ("benchmark_window", "benchmark_pacing", "configured_present_mode"):
            window[name] = fields.get(name)
        mode = window["configured_present_mode"]
        if mode == "None":
            window["configured_present_mode"] = None
        elif mode and re.fullmatch(r"Some\([A-Za-z]+\)", mode):
            window["configured_present_mode"] = mode[5:-1]
        target = fields.get("target_fps", "")
        window["target_fps"] = int(target) if re.fullmatch(r"\d+", target) else None
        window["target_fps_observed"] = target == "none" or bool(re.fullmatch(r"\d+", target))
    try:
        effective_settings = validate_settings(settings)
    except ValueError:
        effective_settings = None
    segments = {}
    for row in rows:
        label = row.get("segment", "")
        counts = segments.setdefault(label, {"redraw_rows": 0, "present_rows": 0})
        counts["redraw_rows"] += 1
        present = row.get("present")
        counts["present_rows"] += isinstance(present, str) and bool(present.strip())
    return {
        "window": window, "segments": segments,
        "frame_interval_boundary": "successive redraw starts, including skipped presentations",
        "displayed_frame_cadence_verified": False,
        "gpu_adapter": None, "actual_present_mode": None,
        "effective_graphics_settings": effective_settings,
        "segment_transition_records": [line for line in log_text.splitlines() if "benchmark segment transition" in line],
    }


@contextlib.contextmanager
def _trial_workspace(prefix: str, artifact_dir: pathlib.Path | None, record: dict):
    retained = None
    if artifact_dir is not None:
        artifact_dir.mkdir(parents=True, exist_ok=True)
        retained = pathlib.Path(tempfile.mkdtemp(prefix=prefix, dir=artifact_dir))
        print(f"raw trial artifacts: {retained}", flush=True)
    with tempfile.TemporaryDirectory(prefix=prefix) as temp_name:
        temp = pathlib.Path(temp_name)
        try:
            yield temp
        except BaseException:
            record["status"] = "failed"
            raise
        finally:
            if retained is not None:
                record["ended_unix_seconds"] = time.time()
                record["artifacts"] = {}
                for name in ("frames.csv", "client.log", "resources.json", "options.json", "presentation.json"):
                    source = temp / name
                    if source.is_file():
                        destination = retained / name
                        shutil.copy2(source, destination)
                        record["artifacts"][name] = {"bytes": destination.stat().st_size, "sha256": _sha256_file(destination)}
                log = temp / "client.log"
                csv_file = temp / "frames.csv"
                try:
                    record["observed"] = _observed_trial_metadata(
                        _read_csv(csv_file) if csv_file.is_file() else [],
                        log.read_text(encoding="utf-8", errors="replace") if log.is_file() else "",
                    )
                except (OSError, ValueError, csv.Error) as error:
                    record["observation_error"] = str(error)
                (retained / "trial.json").write_text(json.dumps(record, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def _read_csv(path: pathlib.Path) -> list[dict[str, str]]:
    with path.open(newline="", encoding="utf-8") as handle:
        return list(csv.DictReader(handle))


def summarize_presentation_capture(
    path: pathlib.Path, settings: dict | None = None, benchmark_pacing: str = "uncapped",
) -> dict:
    _require_nonempty_artifact(path, "presentation capture")
    if path.stat().st_size > 8 * 1024 * 1024:
        raise ValueError("presentation capture exceeds 8 MiB")
    capture = json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=_unique_json_object)
    if not isinstance(capture, dict) or type(capture.get("schema")) is not int or capture["schema"] != 2:
        raise ValueError("presentation capture requires schema 2")
    for name, expected in (
        ("metric", "successful-presentation-submission"), ("clock", "portable-monotonic"),
        ("timeUnit", "microseconds"), ("rowScope", "retained-prefix"),
        ("aggregateScope", "whole-capture"), ("columns", PRESENTATION_COLUMNS),
        ("outcomes", {"0": "not-submitted", "1": "menu", "2": "world"}),
    ):
        if capture.get(name) != expected:
            raise ValueError(f"presentation capture has unexpected {name}")
    counts = ("elapsedUs", "attempts", "submissions", "skippedAttempts", "menuSubmissions",
              "worldSubmissions", "droppedRows", "rejectedSubmissions", "intervalCount", "intervalSumUs")
    if any(type(capture.get(name)) is not int or capture[name] < 0 for name in counts):
        raise ValueError("presentation capture requires non-negative integer aggregates")
    if capture["droppedRows"] != 0:
        raise ValueError("presentation capture droppedRows must be zero; shorten the stationary duration")
    if capture["rejectedSubmissions"] != 0:
        raise ValueError("presentation capture contains rejected submissions")
    if capture["elapsedUs"] == 0 or capture["worldSubmissions"] == 0:
        raise ValueError("presentation capture requires positive elapsed time and successful world handoffs")
    rows = capture.get("rows")
    if not isinstance(rows, list) or not rows or len(rows) != capture["attempts"]:
        raise ValueError("presentation capture rows must cover every attempt")
    cap, vsync = _requested_pacing(settings, benchmark_pacing)
    submissions = menus = worlds = skips = 0
    intervals = []
    previous_finished = None
    for index, row in enumerate(rows, 1):
        if (not isinstance(row, list) or len(row) != len(PRESENTATION_COLUMNS)
                or any(value is not None and (type(value) is not int or value < 0) for value in row)):
            raise ValueError(f"presentation capture row {index} requires nine nullable non-negative integers")
        attempt, started, finished, outcome, submission, interval, target, sync, _completion = row
        if (attempt != index or started is None or finished is None or started > finished
                or finished > capture["elapsedUs"] or outcome not in (0, 1, 2)):
            raise ValueError(f"presentation capture row {index} has invalid attempt timing or outcome")
        if outcome == 0:
            skips += 1
            if submission is not None or interval is not None:
                raise ValueError(f"presentation capture row {index} claims a skipped submission")
            continue
        submissions += 1
        menus += outcome == 1
        worlds += outcome == 2
        expected_interval = None if previous_finished is None else finished - previous_finished
        interval_matches = (interval is None if expected_interval is None else
                            interval is not None and abs(interval - expected_interval) <= 1)
        if submission != submissions or not interval_matches or (interval is not None and interval <= 0):
            raise ValueError(f"presentation capture row {index} has inconsistent submission cadence")
        if target != cap or sync != int(vsync):
            raise ValueError(f"presentation capture row {index} effective pacing differs from the requested policy")
        previous_finished = finished
        if interval is not None:
            intervals.append(interval)
    expected_counts = {
        "submissions": submissions, "skippedAttempts": skips, "menuSubmissions": menus,
        "worldSubmissions": worlds, "intervalCount": len(intervals), "intervalSumUs": sum(intervals),
        "intervalMinUs": min(intervals, default=None), "intervalMaxUs": max(intervals, default=None),
    }
    if any(capture.get(name) != value for name, value in expected_counts.items()):
        raise ValueError("presentation capture aggregates disagree with retained rows")
    return {
        "schema": 2, "segment_scope": "selected stationary segment",
        "metric": capture["metric"], "boundary": "successful post-present surface handoffs",
        "elapsed_seconds": capture["elapsedUs"] / 1_000_000,
        "attempts": capture["attempts"], "submissions": submissions,
        "world_submissions": worlds, "menu_submissions": menus, "skipped_attempts": skips,
        "dropped_rows": capture["droppedRows"], "interval_count": len(intervals),
        "p50_ms": nearest_rank(intervals, 0.50) / 1000 if intervals else None,
        "p95_ms": nearest_rank(intervals, 0.95) / 1000 if intervals else None,
        "p99_ms": nearest_rank(intervals, 0.99) / 1000 if intervals else None,
        "successful_handoffs_per_second": submissions * 1_000_000 / capture["elapsedUs"],
        "displayed_frame_cadence_verified": False,
        "gpu_completion_boundary": "queue-completion callback delay; not GPU execution or scanout",
        "sha256": _sha256_file(path),
    }


def _client_command(
    binary: pathlib.Path,
    workload: str,
    game_port: int,
    durations: tuple[int, ...],
    debug_overlay: str,
    camera_plan: str | None = None,
    heavy_scene: dict | None = None,
    render_distance: int = RENDER_DISTANCE,
    benchmark_window: str = "builtin-fullscreen",
    benchmark_pacing: str = "uncapped",
    benchmark_resolution: tuple[int, int] | None = None,
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
        str(render_distance),
        "--benchmark-window", benchmark_window,
        "--benchmark-pacing", benchmark_pacing,
    ]
    if benchmark_resolution is not None:
        command.extend(["--benchmark-resolution", f"{benchmark_resolution[0]}x{benchmark_resolution[1]}"])
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


def _validate_heavy_segment_summary(segment: str, summary: dict) -> None:
    """Reject a saved heavyweight record whose measured phase is a no-op.

    ``_heavy_record`` normally receives these fields from ``summarize_rows``.
    The artifact validator is also an independent handoff boundary, though, so
    it must not accept an empty object (or a zero-frame placeholder) as proof
    that the stationary and moving phases ran.
    """
    required = {
        "frames",
        "p50_ms",
        "p95_ms",
        "p99_ms",
        "mean_ms",
        "over_16_67",
        "over_33_3",
        "phases_ms",
        "workload_counts",
    }
    missing = sorted(required - set(summary))
    if missing:
        raise RuntimeError(
            f"heavyweight scene record segment {segment!r} is missing summary fields: "
            f"{', '.join(missing)}"
        )

    frames = summary["frames"]
    if isinstance(frames, bool) or not isinstance(frames, int) or frames <= 0:
        raise RuntimeError(
            f"heavyweight scene record segment {segment!r} must have a positive integer frames count"
        )

    timings = {}
    for name in ("p50_ms", "p95_ms", "p99_ms", "mean_ms"):
        value = summary[name]
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise RuntimeError(
                f"heavyweight scene record segment {segment!r} has a non-numeric {name}"
            )
        value = float(value)
        if not math.isfinite(value) or value < 0:
            raise RuntimeError(
                f"heavyweight scene record segment {segment!r} has an invalid {name}"
            )
        timings[name] = value
    if not timings["p50_ms"] <= timings["p95_ms"] <= timings["p99_ms"]:
        raise RuntimeError(
            f"heavyweight scene record segment {segment!r} has unordered percentile timings"
        )

    for name in ("over_16_67", "over_33_3"):
        value = summary[name]
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            raise RuntimeError(
                f"heavyweight scene record segment {segment!r} has an invalid {name} count"
            )
    for name in ("phases_ms", "workload_counts"):
        if not isinstance(summary[name], dict):
            raise RuntimeError(
                f"heavyweight scene record segment {segment!r} has a non-object {name}"
            )


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
    for segment in ("stationary", "moving"):
        _validate_heavy_segment_summary(segment, record["segments"][segment])
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
    artifact_dir: pathlib.Path | None = None,
    comparison_identity: dict | None = None,
    settings: dict | None = None,
    benchmark_window: str = "builtin-fullscreen",
    benchmark_pacing: str = "uncapped",
    benchmark_resolution: tuple[int, int] | None = None,
) -> dict:
    settings = validate_settings(settings) if settings is not None else None
    render_distance = settings["render_distance"] if settings is not None else RENDER_DISTANCE
    record = {
        "schema": 1, "status": "incomplete", "trial": trial, "workload": workload,
        "started_unix_seconds": time.time(), "identity": comparison_identity,
        "debug_overlay": debug_overlay, "durations_seconds": list(durations),
        "camera_plan": camera_plan or "built-in workload choreography",
        "scene_hash": heavy_scene["scene_hash"] if heavy_scene is not None else None,
        "graphics_settings_requested": settings,
        "benchmark_window_requested": benchmark_window, "benchmark_pacing_requested": benchmark_pacing,
        "benchmark_resolution_requested": benchmark_resolution or (2560, 1440),
        "unknown_fields": "null means unavailable or unverified; never inferred from requested settings",
    }
    record["trial_config_sha256"] = _identity_digest({
        name: record[name]
        for name in ("identity", "debug_overlay", "durations_seconds", "camera_plan", "scene_hash",
                     "graphics_settings_requested", "benchmark_window_requested", "benchmark_pacing_requested",
                     "benchmark_resolution_requested")
    })
    with _trial_workspace(f"lodestone-{workload}-bench-", artifact_dir, record) as temp:
        csv_path = temp / "frames.csv"
        log_path = temp / "client.log"
        resource_path = temp / "resources.json"
        presentation_path = temp / "presentation.json"
        data_dir = temp / "data"
        data_dir.mkdir()
        if settings is not None:
            record["options_sha256"] = _write_trial_options(temp, settings)
        username = _unique_username(trial)
        (data_dir / "offline.json").write_text(
            json.dumps({"username": username}), encoding="utf-8"
        )

        client = _client_command(
            binary, workload, oracle["game_port"], durations, debug_overlay,
            camera_plan=camera_plan, heavy_scene=heavy_scene,
            render_distance=render_distance, benchmark_window=benchmark_window,
            benchmark_pacing=benchmark_pacing,
            benchmark_resolution=benchmark_resolution,
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
                "LODESTONE_PRESENTATION_CAPTURE": str(presentation_path),
                "LODESTONE_PRESENTATION_CAPTURE_SEGMENT": f"{workload}.stationary",
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
            resources = _resource_sampler(process.pid, samply_artifact is not None)
            sample = resources.take_sample()
            if sample["rss_bytes"] is not None:
                rss_samples.append(sample["rss_bytes"])
            while process.poll() is None:
                sample = resources.sample_if_due()
                if sample is not None and sample["rss_bytes"] is not None:
                    rss_samples.append(sample["rss_bytes"])
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
            resources.take_sample()
            stop_reason = "deadline" if timed_out else "client_exit"
            resources.write_report(resource_path, stop_reason)
            resource_report = resources.report(stop_reason)
            record["resources"] = {
                "scope": "whole launch, including startup and all workload phases",
                "summary": resource_report["summary"],
                "gpu_memory": resource_report["gpu_memory"],
            }

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
        framebuffer = validate_run(rows, log_text, workload, benchmark_window, benchmark_pacing, settings,
                                   benchmark_resolution or (2560, 1440))
        presentation = summarize_presentation_capture(presentation_path, settings, benchmark_pacing)
        presentation["segment"] = f"{workload}.stationary"
        record["presentation"] = presentation
        record["observed"] = _observed_trial_metadata(rows, log_text)
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
        record["status"] = "complete"
        record["segments"] = segments
        rss = {
            "start": rss_samples[0] if rss_samples else None,
            "peak": max(rss_samples, default=None),
            "end": rss_samples[-1] if rss_samples else None,
        }
        return {
            "trial": trial,
            "username": username,
            "debug_overlay": debug_overlay,
            "segments": segments,
            "rss_bytes": rss,
            "resources": record["resources"],
            "framebuffer": framebuffer,
            "render_distance_requested": render_distance,
            "graphics_settings_requested": settings,
            "options_sha256": record.get("options_sha256"),
            "benchmark_window_requested": benchmark_window, "benchmark_pacing_requested": benchmark_pacing,
            "benchmark_resolution_requested": benchmark_resolution or (2560, 1440),
            "observed": record["observed"], "presentation": presentation,
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
    memory = "/".join(f"{rss[name]/1048576:.1f}" if rss[name] is not None else "unavailable"
                      for name in ("start", "peak", "end"))
    print(
        f"trial {result['trial']} debug_overlay={result['debug_overlay']} "
        f"process-tree RSS MiB start/peak/last-observed: {memory}"
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
    presentation = result["presentation"]
    print(
        f"  {presentation['segment']} surface handoffs: world={presentation['world_submissions']} "
        f"skipped={presentation['skipped_attempts']} elapsed={presentation['elapsed_seconds']:.3f}s "
        f"successful/s={presentation['successful_handoffs_per_second']:.2f} "
        f"interval p50/p95/p99={presentation['p50_ms']}/{presentation['p95_ms']}/{presentation['p99_ms']} ms"
    )
    if gpu["samples"]:
        print(f"  GPU timestamp snapshots: n={gpu['samples']}")
        for name in ("world", "first_person"):
            if name not in gpu:
                continue
            values = gpu[name]
            print(
                f"    {name} (n={values['samples']}): median/p95="
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
        "render_distance": result["render_distance_requested"],
        "graphics_settings_requested": result["graphics_settings_requested"],
        "options_sha256": result["options_sha256"],
        "benchmark_window_requested": result["benchmark_window_requested"],
        "benchmark_pacing_requested": result["benchmark_pacing_requested"],
        "observed": result["observed"],
        "presentation": result["presentation"],
        "framebuffer": list(result["framebuffer"]),
        "durations_seconds": {
            "warmup": durations[0],
            "stationary": durations[1],
            "moving": durations[2],
        },
        "rss_bytes": result["rss_bytes"],
        "rss_scope": "summed process-tree RSS, sampled at one-second requested intervals",
        "resources": result["resources"],
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


def _trial_durations(args: argparse.Namespace) -> tuple[int, ...]:
    defaults = (2, 2, 3) if args.smoke else (
        (45, 30, 60) if args.workload in ("megaworld", "lovelier") else (20, 30, 60)
    )
    warmup, stationary, moving = (
        override if override is not None else default
        for override, default in zip((args.warmup_seconds, args.stationary_seconds, args.moving_seconds), defaults)
    )
    if args.workload == "heavyweight":
        durations = (warmup, 1 if args.smoke else args.heavy_mutation_seconds, stationary, moving)
        if sum(durations) > MAX_HEAVY_TOTAL_SECONDS:
            raise ValueError(f"heavyweight duration must be at most {MAX_HEAVY_TOTAL_SECONDS}s")
        return durations
    return warmup, stationary, moving


def _physical_resolution(value: str) -> tuple[int, int]:
    parts = value.split("x")
    if len(parts) == 2 and all(part.isascii() and part.isdecimal() for part in parts):
        width, height = map(int, parts)
        if 320 <= width <= 8192 and 240 <= height <= 8192:
            return width, height
    raise argparse.ArgumentTypeError("resolution requires WIDTHxHEIGHT, width 320–8192 and height 240–8192")


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
    parser.add_argument("--settings", type=pathlib.Path, help="complete eleven-field graphics settings JSON for isolated trial options")
    parser.add_argument("--benchmark-window", choices=("builtin-fullscreen", "windowed"), default="builtin-fullscreen")
    parser.add_argument("--benchmark-pacing", choices=("uncapped", "options"), default="uncapped")
    parser.add_argument("--benchmark-resolution", type=_physical_resolution, help="windowed physical framebuffer size, e.g. 1280x720")
    parser.add_argument("--warmup-seconds", type=int)
    parser.add_argument("--stationary-seconds", type=int)
    parser.add_argument("--moving-seconds", type=int)
    parser.add_argument("--artifact-dir", type=pathlib.Path, help="retain raw CSV, log, presentation capture, declared options and trial identity in unique directories")
    parser.add_argument("--world-snapshot-manifest", type=pathlib.Path, help="hash declared bounded world files while the oracle is offline, before launch")
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
    if args.benchmark_resolution is not None and args.benchmark_window != "windowed":
        parser.error("--benchmark-resolution requires --benchmark-window windowed")
    if args.settings is None and (args.benchmark_window != "builtin-fullscreen" or args.benchmark_pacing != "uncapped"):
        parser.error("--benchmark-window windowed and --benchmark-pacing options require --settings")
    args.graphics_settings = None
    if args.settings is not None:
        try:
            args.graphics_settings = _read_settings(args.settings)
        except (OSError, ValueError) as error:
            parser.error(str(error))
    for name, minimum in (("warmup_seconds", 0), ("stationary_seconds", 1), ("moving_seconds", 1)):
        value = getattr(args, name)
        if value is not None and not minimum <= value <= 0xffffffffffffffff:
            parser.error(f"--{name.replace('_', '-')} must be an integer in {minimum}..=18446744073709551615")
    if args.world_snapshot_manifest is not None and args.artifact_dir is None:
        parser.error("--world-snapshot-manifest requires --artifact-dir to retain its identity")
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
    try:
        args.durations = _trial_durations(args)
    except ValueError as error:
        parser.error(str(error))
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

    durations = args.durations
    trial_count = 1 if args.smoke or args.samply or args.workload == "heavyweight" else args.trials
    arms = overlay_arms(args.workload, args.debug_overlay)
    if args.samply and len(arms) != 1:
        raise SystemExit(
            "--samply with megaworld requires --debug-overlay closed or open"
        )
    comparison_identity = None
    if args.artifact_dir is not None:
        snapshot = (
            _world_snapshot_identity(args.world_snapshot_manifest, ORACLES[args.workload])
            if args.world_snapshot_manifest is not None else None
        )
        comparison_identity = _comparison_identity(
            binary, args.workload, snapshot, args.graphics_settings, args.benchmark_window, args.benchmark_pacing,
            args.benchmark_resolution,
        )
    render_distance = args.graphics_settings["render_distance"] if args.graphics_settings is not None else RENDER_DISTANCE
    oracle = start_oracle(args.workload, render_distance)
    if args.workload == "showcase":
        prepare_showcase(oracle["rcon_port"])
    heavy_scene = None
    heavy_setup_datapack = None
    emitted_scale = 1 if args.smoke else args.heavy_scale
    try:
        if args.workload == "heavyweight":
            heavy_scene, heavy_setup_datapack = prepare_heavy_scene(
                oracle["rcon_port"], oracle["world"], HEAVY_SCENE_EMITTER,
                args.heavy_scenario, args.heavy_seed, emitted_scale, args.heavy_camera_plan,
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
                    artifact_dir=args.artifact_dir,
                    comparison_identity=comparison_identity,
                    settings=args.graphics_settings, benchmark_window=args.benchmark_window,
                    benchmark_pacing=args.benchmark_pacing,
                    benchmark_resolution=args.benchmark_resolution,
                )
                _print_trial(args.workload, result)
                if args.workload == "heavyweight":
                    record = _heavy_record(
                        args.workload, trial, binary, durations, debug_overlay, args.heavy_scale,
                        args.heavy_camera_plan, heavy_scene, result["segments"],
                    )
                    for name in ("render_distance_requested", "graphics_settings_requested", "options_sha256",
                                 "benchmark_window_requested", "benchmark_pacing_requested", "observed", "presentation"):
                        record[name] = result[name]
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
    finally:
        if heavy_setup_datapack is not None:
            heavy_setup_datapack.cleanup()
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as exc:
        print(f"benchmark failed: {exc}", file=sys.stderr)
        raise SystemExit(1)
