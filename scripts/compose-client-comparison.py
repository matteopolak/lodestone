#!/usr/bin/env python3
"""Compose two recorded, independently measured client runs without changing speed."""

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import subprocess
import sys
import tempfile


ARMS = ("java", "lodestone")
RESOURCE_FIELDS = (
    "mean_cpu_percent", "peak_cpu_percent", "peak_rss_bytes",
    "peak_gpu_resident_bytes", "peak_dedicated_vram_bytes", "peak_gpu_allocated_bytes",
)


def run(command):
    return subprocess.run(command, check=True, capture_output=True, text=True).stdout


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def probe(path, ffprobe):
    payload = json.loads(run([
        ffprobe, "-v", "error", "-select_streams", "v:0", "-show_streams",
        "-show_format", "-of", "json", str(path),
    ]))
    streams = payload.get("streams", [])
    if not streams:
        raise ValueError(f"No video stream: {path}")
    stream = streams[0]
    duration = stream.get("duration", payload.get("format", {}).get("duration"))
    if duration is None or not math.isfinite(float(duration)) or float(duration) <= 0:
        raise ValueError(f"Cannot establish video duration: {path}")
    return {
        "duration_seconds": float(duration),
        **{key: stream.get(key) for key in (
            "width", "height", "start_time", "avg_frame_rate", "r_frame_rate", "time_base",
        )},
    }


def nonblank(value, name):
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{name} must be nonblank text")
    return value


def number(value, name, positive=False):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError(f"{name} must be a finite number")
    if not math.isfinite(value) or value < 0 or (positive and value == 0):
        raise ValueError(f"{name} must be {'positive' if positive else 'nonnegative'} and finite")
    return value


def file_path(value, base, name):
    path = (base / nonblank(value, name)).resolve(strict=True)
    if not path.is_file():
        raise ValueError(f"{name} must refer to a file")
    return path


def prepare(config, base, output, fps, ffprobe):
    if not isinstance(config, dict):
        raise ValueError("Config must be a JSON object")
    duration = number(config.get("duration_seconds"), "duration_seconds", positive=True)
    if abs(duration * fps - round(duration * fps)) > 1e-6:
        raise ValueError("duration_seconds must be a whole number of output frames")
    for name in ("scenario", "world_identity", "action_identity", "texture_pack_identity", "timing_control"):
        nonblank(config.get(name), name)
    if output.exists() or output.is_symlink():
        raise ValueError(f"Refusing to overwrite output: {output}")
    manifest = output.with_suffix(output.suffix + ".manifest.json")
    if manifest.exists() or manifest.is_symlink():
        raise ValueError(f"Refusing to overwrite manifest: {manifest}")
    arms = {}
    for name in ARMS:
        if not isinstance(config.get(name), dict):
            raise ValueError(f"{name} must be an object")
        arm = dict(config[name])
        for field in ("label", "release"):
            nonblank(arm.get(field), f"{name}.{field}")
        if not isinstance(arm.get("settings"), dict) or not arm["settings"]:
            raise ValueError(f"{name}.settings must contain the actual capture settings")
        if not isinstance(arm.get("mods"), list) or any(
            not isinstance(mod, str) or not mod.strip() for mod in arm["mods"]
        ):
            raise ValueError(f"{name}.mods must be a list of mod names and versions (empty for none)")
        source = file_path(arm.get("source"), base, f"{name}.source")
        if source in (output, manifest):
            raise ValueError("An output cannot be an input")
        offset = number(arm.get("offset_seconds"), f"{name}.offset_seconds")
        info = probe(source, ffprobe)
        if offset + duration > info["duration_seconds"] + 1e-6:
            raise ValueError(f"{name} source is too short for the requested offset and duration")
        arm.update(source=str(source), source_sha256=sha256(source), video=info)
        measured = arm.get("measured_summary")
        if measured is not None:
            if not isinstance(measured, dict):
                raise ValueError(f"{name}.measured_summary must be an object")
            measured = dict(measured)
            nonblank(measured.get("method"), f"{name}.measured_summary.method")
            provenance = file_path(measured.get("source"), base, f"{name}.measured_summary.source")
            if provenance in (source, output, manifest):
                raise ValueError("Metrics provenance must be a separate measurement file")
            fields = ("mean_fps", "p95_frame_time_ms", "p99_frame_time_ms")
            if not any(field in measured for field in fields):
                raise ValueError("measured_summary must supply at least one supported metric")
            for field in fields:
                if field in measured:
                    number(measured[field], f"{name}.measured_summary.{field}", positive=True)
            if all(field in measured for field in fields[1:]):
                if measured[fields[1]] > measured[fields[2]]:
                    raise ValueError("p95 frame time cannot exceed p99 frame time")
            measured.update(source=str(provenance), source_sha256=sha256(provenance))
            arm["measured_summary"] = measured
        resources = arm.get("resource_summary")
        if resources is not None:
            if not isinstance(resources, dict):
                raise ValueError(f"{name}.resource_summary must be an object")
            resources = dict(resources)
            nonblank(resources.get("method"), f"{name}.resource_summary.method")
            nonblank(resources.get("interval"), f"{name}.resource_summary.interval")
            provenance = file_path(resources.get("source"), base, f"{name}.resource_summary.source")
            if provenance in (source, output, manifest):
                raise ValueError("Resource provenance must be a separate measurement file")
            if not any(field in resources for field in RESOURCE_FIELDS):
                raise ValueError("resource_summary must supply at least one supported metric")
            unavailable = resources.get("unavailable", {})
            if not isinstance(unavailable, dict):
                raise ValueError("resource_summary.unavailable must map metric names to reasons")
            for field in RESOURCE_FIELDS:
                if field not in resources:
                    continue
                if resources[field] is None:
                    nonblank(unavailable.get(field), f"{name}.resource_summary.unavailable.{field}")
                else:
                    number(resources[field], f"{name}.resource_summary.{field}")
            resources.update(source=str(provenance), source_sha256=sha256(provenance))
            arm["resource_summary"] = resources
        arms[name] = arm
    if arms["java"]["source_sha256"] == arms["lodestone"]["source_sha256"]:
        raise ValueError("The two arms must be distinct recordings; input identity matches")
    return duration, arms, manifest


def make_overlay(path, config, arms, width, height, font_path):
    try:
        from PIL import Image, ImageDraw, ImageFont
    except ImportError as error:
        raise ValueError("Overlay requires Pillow in this Python runtime; omit --overlay or use an existing Pillow runtime") from error
    if font_path is None or not font_path.is_file():
        raise ValueError("Overlay requires --font pointing to an existing TrueType/OpenType font")
    image = Image.new("RGBA", (width, height * 2))
    draw = ImageDraw.Draw(image)
    font = ImageFont.truetype(str(font_path), max(12, width // 50))
    margin = max(8, width // 80)
    line_height = max(18, width // 36)
    for index, name in enumerate(ARMS):
        arm = arms[name]
        texts = [
            f"{arm['label']} | {config['scenario']}",
            f"Release: {arm['release']}",
            "Mods: " + (", ".join(arm["mods"]) or "none"),
        ]
        measured = arm.get("measured_summary", {})
        metrics = []
        for field, label, unit in (
            ("mean_fps", "Mean", "FPS"),
            ("p95_frame_time_ms", "p95", "ms"),
            ("p99_frame_time_ms", "p99", "ms"),
        ):
            if field in measured:
                metrics.append(f"{label}: {measured[field]:.2f} {unit}")
        if metrics:
            texts.append("Measured run summary | " + " | ".join(metrics))
        resources = arm.get("resource_summary", {})
        cpu = [f"{label}: {resources[field]:.1f}%" for field, label in (
            ("mean_cpu_percent", "mean"), ("peak_cpu_percent", "peak"),
        ) if resources.get(field) is not None]
        if cpu:
            texts.append("CPU (100% = one core) | " + " | ".join(cpu))
        for fields in (
            (("peak_rss_bytes", "RAM peak RSS"),),
            (("peak_gpu_resident_bytes", "GPU resident peak"), ("peak_dedicated_vram_bytes", "Dedicated VRAM peak")),
            (("peak_gpu_allocated_bytes", "Tracked GPU allocations peak"),),
        ):
            memory = []
            for field, label in fields:
                if field in resources:
                    value = resources[field]
                    memory.append(f"{label}: unavailable" if value is None else f"{label}: {value / (1 << 20):.1f} MiB")
            if memory:
                texts.append(" | ".join(memory))
        lines = []
        for text in texts:
            current = ""
            for word in text.split():
                candidate = (current + " " + word).strip()
                if draw.textbbox((0, 0), candidate, font=font)[2] > width - 2 * margin:
                    if not current:
                        raise ValueError("Overlay text contains a word too wide; shorten the label/release/mod name")
                    lines.append(current)
                    current = word
                else:
                    current = candidate
            lines.append(current)
        banner_height = len(lines) * line_height + 2 * margin
        if banner_height > height * 0.4:
            raise ValueError("Overlay text covers too much video; shorten labels or omit --overlay")
        top = index * height
        draw.rectangle((0, top, width, top + banner_height), fill=(0, 0, 0, 200))
        for line_index, line in enumerate(lines):
            draw.text((margin, top + margin + line_index * line_height), line, font=font, fill="white")
    image.save(path)


def compose(args):
    config_path = args.config.resolve(strict=True)
    config = json.loads(config_path.read_text(encoding="utf-8"))
    output = args.output.resolve()
    if output.suffix.lower() != ".mp4":
        raise ValueError("Output must have an .mp4 extension")
    width, height = args.width, args.panel_height
    if width <= 0 or height <= 0 or width % 2 or height % 2 or width * 9 != height * 16:
        raise ValueError("Panel dimensions must be positive, even, and exactly 16:9")
    if args.threads <= 0 or args.filter_complex_threads <= 0:
        raise ValueError("Encoder and filter thread counts must be positive")
    duration, arms, manifest_path = prepare(config, config_path.parent, output, args.fps, args.ffprobe)
    if not output.parent.is_dir():
        raise ValueError("Output parent directory must exist")
    version = run([args.ffmpeg, "-version"]).splitlines()[0]
    with tempfile.TemporaryDirectory(prefix="client-comparison-", dir=output.parent) as scratch:
        scratch = Path(scratch)
        encoded = scratch / "comparison.mp4"
        command = [
            args.ffmpeg, "-hide_banner", "-loglevel", "error", "-n",
            "-filter_complex_threads", str(args.filter_complex_threads),
        ]
        for name in ARMS:
            command.extend(["-threads", str(args.threads), "-i", arms[name]["source"]])
        filters = []
        for index, name in enumerate(ARMS):
            filters.append(
                f"[{index}:v:0]setpts=PTS-STARTPTS,trim=start={arms[name]['offset_seconds']}:duration={duration},"
                f"setpts=PTS-STARTPTS,fps={args.fps},"
                f"scale={width}:{height}:force_original_aspect_ratio=decrease:force_divisible_by=2,"
                f"pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:black,setsar=1,format=yuv420p[arm{index}]"
            )
        filters.append("[arm0][arm1]vstack=inputs=2:shortest=1[stack]")
        if args.overlay:
            overlay = scratch / "overlay.png"
            make_overlay(overlay, config, arms, width, height, args.font)
            command.extend(["-i", str(overlay)])
            filters.append("[stack][2:v:0]overlay=eof_action=repeat:shortest=0[out]")
        else:
            filters.append("[stack]null[out]")
        command.extend([
            "-filter_complex", ";".join(filters), "-map", "[out]", "-an",
            "-frames:v", str(round(duration * args.fps)), "-c:v", "libx264",
            "-threads", str(args.threads), "-preset", "medium",
            "-crf", "18", "-pix_fmt", "yuv420p", "-movflags", "+faststart", str(encoded),
        ])
        run(command)
        info = probe(encoded, args.ffprobe)
        if (info["width"], info["height"]) != (width, height * 2):
            raise ValueError("Encoded panel dimensions differ from requested dimensions")
        if abs(info["duration_seconds"] - duration) > 1e-3:
            raise ValueError("Encoded duration differs from requested duration; a source ended early")
        for arm in arms.values():
            if sha256(Path(arm["source"])) != arm["source_sha256"]:
                raise ValueError("Source recording changed during composition")
            measured = arm.get("measured_summary")
            if measured and sha256(Path(measured["source"])) != measured["source_sha256"]:
                raise ValueError("Measurement source changed during composition")
            resources = arm.get("resource_summary")
            if resources and sha256(Path(resources["source"])) != resources["source_sha256"]:
                raise ValueError("Resource source changed during composition")
        record = {
            "schema": 1, "config": config, "config_sha256": sha256(config_path), "arms": arms,
            "timing_control": config["timing_control"], "ffmpeg_version": version,
            "output": str(output), "output_sha256": sha256(encoded), "output_video": info,
            "composition": {
                "top": "java", "bottom": "lodestone", "duration_seconds": duration,
                "panel_width": width, "panel_height": height, "encoding_fps": args.fps,
                "codec_threads": args.threads, "filter_complex_threads": args.filter_complex_threads,
                "speed": 1, "scaling": "fit with black letterboxing; no crop", "audio": "omitted",
                "overlay": args.overlay,
                "font_sha256": sha256(args.font) if args.overlay else None,
                "performance_note": "Encoding FPS is not measured game FPS. Summaries are supplied measurements; world/action equivalence is declared externally.",
            },
            "command": command,
        }
        recorded = scratch / "manifest.json"
        recorded.write_text(json.dumps(record, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        os.link(encoded, output)
        try:
            os.link(recorded, manifest_path)
        except OSError:
            if output.stat().st_ino == encoded.stat().st_ino:
                output.unlink()
            raise
    return output, manifest_path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--width", type=int, default=1280)
    parser.add_argument("--panel-height", type=int, default=720)
    parser.add_argument("--fps", type=int, choices=(30, 60), default=60, help="Encoding rate; never a performance measurement")
    parser.add_argument("--threads", type=int, default=2, help="Threads per video decoder and encoder (default: 2)")
    parser.add_argument("--filter-complex-threads", type=int, default=1, help="Filter graph threads (default: 1)")
    parser.add_argument("--overlay", action="store_true", help="Draw labels and supplied measured summaries using Pillow")
    parser.add_argument("--font", type=Path)
    parser.add_argument("--ffmpeg", default="ffmpeg")
    parser.add_argument("--ffprobe", default="ffprobe")
    try:
        output, manifest = compose(parser.parse_args())
    except (ValueError, OSError, subprocess.CalledProcessError, json.JSONDecodeError) as error:
        detail = error.stderr if isinstance(error, subprocess.CalledProcessError) else str(error)
        print(f"comparison: {detail}", file=sys.stderr)
        return 1
    print(output)
    print(manifest)
    return 0


if __name__ == "__main__":
    sys.exit(main())
