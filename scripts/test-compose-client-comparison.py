#!/usr/bin/env python3
"""Guard tests plus a small explicitly synthetic video composition control."""

import argparse
import importlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location(
    "comparison", Path(__file__).with_name("compose-client-comparison.py")
)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def configuration(directory):
    return {
        "scenario": "SYNTHETIC TEST ONLY",
        "world_identity": "synthetic color control, no game world",
        "action_identity": "synthetic transition control, no game action",
        "texture_pack_identity": "no textures; test colors only",
        "timing_control": "Independent source elapsed-time offsets; no game measurements",
        "duration_seconds": 0.5,
        **{
            name: {
                "source": str(directory / f"{name}.mp4"),
                "offset_seconds": 0.5 if name == "java" else 1.0,
                "label": f"{name} synthetic control", "release": "test-only",
                "mods": [], "settings": {"synthetic": True},
            }
            for name in MODULE.ARMS
        },
    }


class GuardTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.config = configuration(self.directory)
        for name in MODULE.ARMS:
            (self.directory / f"{name}.mp4").write_bytes(name.encode())
        self.output = self.directory / "out.mp4"

    def prepare(self):
        with mock.patch.object(MODULE, "probe", return_value={"duration_seconds": 2.0}):
            return MODULE.prepare(self.config, self.directory, self.output, 60, "ffprobe")

    def test_unequal_offsets_are_retained_and_short_source_rejected(self):
        duration, arms, _ = self.prepare()
        self.assertEqual(duration, 0.5)
        self.assertEqual(arms["java"]["offset_seconds"], 0.5)
        self.assertEqual(arms["lodestone"]["offset_seconds"], 1.0)
        self.config["lodestone"]["offset_seconds"] = 1.51
        with self.assertRaisesRegex(ValueError, "too short"):
            self.prepare()

    def test_existing_video_and_manifest_are_never_overwritten(self):
        for target in (self.output, self.output.with_suffix(".mp4.manifest.json")):
            with self.subTest(target=target):
                target.write_bytes(b"KEEP")
                with self.assertRaisesRegex(ValueError, "overwrite"):
                    self.prepare()
                self.assertEqual(target.read_bytes(), b"KEEP")
                target.unlink()

    def test_same_input_and_copied_input_are_rejected(self):
        self.config["lodestone"]["source"] = self.config["java"]["source"]
        with self.assertRaisesRegex(ValueError, "input identity"):
            self.prepare()
        self.config["lodestone"]["source"] = str(self.directory / "copy.mp4")
        (self.directory / "copy.mp4").write_bytes(b"java")
        with self.assertRaisesRegex(ValueError, "input identity"):
            self.prepare()

    def test_metrics_require_independent_provenance(self):
        self.config["java"]["measured_summary"] = {
            "mean_fps": 120, "method": "test-only", "source": self.config["java"]["source"],
        }
        with self.assertRaisesRegex(ValueError, "separate measurement"):
            self.prepare()
        report = self.directory / "synthetic-measurement.json"
        report.write_text("{}", encoding="utf-8")
        self.config["java"]["measured_summary"]["source"] = str(report)
        _, arms, _ = self.prepare()
        self.assertEqual(arms["java"]["measured_summary"]["source_sha256"], MODULE.sha256(report))
        self.config["java"]["measured_summary"]["mean_fps"] = float("nan")
        with self.assertRaisesRegex(ValueError, "finite"):
            self.prepare()

    def test_resources_keep_cpu_scope_and_unknown_gpu_memory_separate(self):
        report = self.directory / "synthetic-resources.json"
        report.write_text("{}", encoding="utf-8")
        resources = {
            "source": str(report), "method": "Synthetic process-tree intervals; 100% = one core",
            "interval": "Synthetic selected video interval only",
            "mean_cpu_percent": 179.4, "peak_cpu_percent": 291.1,
            "peak_rss_bytes": 1 << 30, "peak_gpu_allocated_bytes": 200 << 20,
            "peak_dedicated_vram_bytes": None,
            "unavailable": {"peak_dedicated_vram_bytes": "Unified memory; no residency instrument"},
        }
        self.config["java"]["resource_summary"] = resources
        _, arms, _ = self.prepare()
        retained = arms["java"]["resource_summary"]
        self.assertEqual(retained["source_sha256"], MODULE.sha256(report))
        self.assertEqual(retained["mean_cpu_percent"], 179.4)
        self.assertIsNone(retained["peak_dedicated_vram_bytes"])
        self.assertEqual(retained["peak_gpu_allocated_bytes"], 200 << 20)
        self.assertNotIn("peak_gpu_resident_bytes", retained)
        resources["unavailable"].clear()
        with self.assertRaisesRegex(ValueError, "unavailable.peak_dedicated_vram_bytes"):
            self.prepare()
        resources["peak_dedicated_vram_bytes"] = 0
        self.prepare()
        resources["mean_cpu_percent"] = -1
        with self.assertRaisesRegex(ValueError, "nonnegative"):
            self.prepare()
        resources["mean_cpu_percent"] = 0
        resources["source"] = self.config["java"]["source"]
        with self.assertRaisesRegex(ValueError, "separate measurement"):
            self.prepare()


@unittest.skipUnless(shutil.which("ffmpeg") and shutil.which("ffprobe"), "FFmpeg tools absent")
class SyntheticCompositionTest(unittest.TestCase):
    def test_elapsed_offsets_duration_dimensions_and_top_bottom_pixels(self):
        with tempfile.TemporaryDirectory(prefix="synthetic-client-comparison-") as directory:
            directory = Path(directory)
            for name, first, second, transition in (
                ("java", "red", "blue", 0.5),
                ("lodestone", "yellow", "lime", 1.0),
            ):
                subprocess.run([
                    "ffmpeg", "-v", "error", "-n",
                    "-f", "lavfi", "-i", f"color={first}:s=320x180:r=60:d={transition}",
                    "-f", "lavfi", "-i", f"color={second}:s=320x180:r=60:d=1",
                    "-filter_complex", "[0:v][1:v]concat=n=2:v=1:a=0[out]",
                    "-map", "[out]", "-c:v", "libx264", str(directory / f"{name}.mp4"),
                ], check=True, capture_output=True)
            config = directory / "synthetic.json"
            config.write_text(json.dumps(configuration(directory)), encoding="utf-8")
            output = directory / "synthetic-out.mp4"
            args = argparse.Namespace(
                config=config, output=output, width=320, panel_height=180, fps=30,
                threads=2, filter_complex_threads=1,
                overlay=False, font=None, ffmpeg="ffmpeg", ffprobe="ffprobe",
            )
            _, manifest = MODULE.compose(args)
            record = json.loads(manifest.read_text(encoding="utf-8"))
            self.assertEqual(record["output_video"]["duration_seconds"], 0.5)
            self.assertEqual((record["output_video"]["width"], record["output_video"]["height"]), (320, 360))
            self.assertNotIn("measured_summary", record["arms"]["java"])
            self.assertEqual(record["composition"]["codec_threads"], 2)
            self.assertEqual(record["composition"]["filter_complex_threads"], 1)
            self.assertEqual(record["command"].count("-threads"), 3)
            self.assertLess(sum(path.stat().st_size for path in directory.iterdir()), 5_000_000)
            pixels = subprocess.run([
                "ffmpeg", "-v", "error", "-i", str(output), "-f", "rawvideo",
                "-pix_fmt", "rgb24", "-",
            ], check=True, capture_output=True).stdout
            frame_size = 320 * 360 * 3
            self.assertEqual(len(pixels), frame_size * 15)
            for frame_offset in (0, frame_size * 14):
                for y, channel in ((90, 2), (270, 1)):
                    offset = frame_offset + (y * 320 + 160) * 3
                    color = pixels[offset:offset + 3]
                    self.assertGreater(color[channel], 220)
                    self.assertLess(max(color[index] for index in range(3) if index != channel), 30)
            font = Path("/System/Library/Fonts/Helvetica.ttc")
            if font.is_file() and importlib.util.find_spec("PIL") is not None:
                args.output = directory / "synthetic-overlay.mp4"
                args.width, args.panel_height = 1280, 720
                args.overlay, args.font = True, font
                _, manifest = MODULE.compose(args)
                record = json.loads(manifest.read_text(encoding="utf-8"))
                self.assertTrue(record["composition"]["overlay"])
                self.assertEqual(record["composition"]["font_sha256"], MODULE.sha256(font))
                self.assertEqual(record["output_video"]["duration_seconds"], 0.5)
                self.assertLess(sum(path.stat().st_size for path in directory.iterdir()), 5_000_000)

    def test_overlay_dependency_and_font_are_explicit(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            config = configuration(directory)
            with mock.patch.dict("sys.modules", {"PIL": None}):
                with self.assertRaisesRegex(ValueError, "requires Pillow"):
                    MODULE.make_overlay(directory / "overlay.png", config, config, 1280, 720, None)
            if importlib.util.find_spec("PIL") is not None:
                with self.assertRaisesRegex(ValueError, "requires --font"):
                    MODULE.make_overlay(directory / "overlay.png", config, config, 1280, 720, None)


if __name__ == "__main__":
    unittest.main()
