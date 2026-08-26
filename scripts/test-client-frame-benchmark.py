#!/usr/bin/env python3
"""Unit tests for the live client benchmark runner and summarizer."""

import importlib.util
import pathlib
import sys
import unittest


RUNNER = pathlib.Path(__file__).with_name("client-frame-benchmark.py")
SPEC = importlib.util.spec_from_file_location("client_frame_benchmark", RUNNER)
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class SummaryTests(unittest.TestCase):
    @staticmethod
    def fullscreen_log(width=3024, height=1898):
        return (
            "selected hardware built-in display for fullscreen benchmark "
            "native_id=Some(1)\n"
            f"benchmark window ready framebuffer_width={width} "
            f"framebuffer_height={height} fullscreen=true\n"
            "benchmark complete"
        )

    def test_percentiles_budget_misses_and_nonempty_phase_means(self):
        summary = MODULE.summarize_rows(
            [
                {
                    "frame_interval_ms": "8.0",
                    "segment": "terrain.stationary",
                    "setup": "1.0",
                },
                {
                    "frame_interval_ms": "17.0",
                    "segment": "terrain.stationary",
                    "setup": "",
                },
                {
                    "frame_interval_ms": "34.0",
                    "segment": "terrain.stationary",
                    "setup": "3.0",
                },
            ]
        )
        self.assertEqual(summary["frames"], 3)
        self.assertEqual(summary["p50_ms"], 17.0)
        self.assertEqual(summary["p95_ms"], 34.0)
        self.assertEqual(summary["p99_ms"], 34.0)
        self.assertEqual(summary["over_16_67"], 2)
        self.assertEqual(summary["over_33_3"], 1)
        self.assertEqual(summary["phases_ms"]["setup"], 2.0)

    def test_workload_counts_are_not_mislabeled_as_milliseconds(self):
        summary = MODULE.summarize_rows(
            [
                {
                    "frame_interval_ms": "8.0",
                    "segment": "megaworld.stationary",
                    "world_encode_submit": "3.0",
                    "world.model_sections_visited": "800",
                    "hud.debug_lines": "29",
                    "light.relight_cells_visited": "2048",
                },
                {
                    "frame_interval_ms": "9.0",
                    "segment": "megaworld.stationary",
                    "world_encode_submit": "4.0",
                    "world.model_sections_visited": "1000",
                    "hud.debug_lines": "31",
                    "light.relight_cells_visited": "4096",
                },
            ]
        )

        self.assertEqual(summary["phases_ms"]["world_encode_submit"], 3.5)
        self.assertNotIn("world.model_sections_visited", summary["phases_ms"])
        self.assertNotIn("light.relight_cells_visited", summary["phases_ms"])
        self.assertEqual(
            summary["workload_counts"]["world.model_sections_visited"],
            {"median": 900.0, "p95": 1000.0, "max": 1000.0},
        )
        self.assertEqual(
            summary["workload_counts"]["light.relight_cells_visited"],
            {"median": 3072.0, "p95": 4096.0, "max": 4096.0},
        )

    def test_megaworld_uses_its_oracle_and_preserves_authored_spawn(self):
        oracle = MODULE.ORACLES["megaworld"]
        self.assertEqual(oracle["game_port"], 25590)
        self.assertEqual(oracle["rcon_port"], 25591)
        commands = MODULE.joined_player_commands("megaworld", "BenchUser")
        self.assertIn("gamemode creative BenchUser", commands)
        self.assertFalse(any(command.startswith("tp ") for command in commands))

    def test_lovelier_uses_an_independent_oracle_and_open_air_waypoint(self):
        oracle = MODULE.ORACLES["lovelier"]
        self.assertEqual(oracle["game_port"], 25600)
        self.assertEqual(oracle["rcon_port"], 25601)
        commands = MODULE.joined_player_commands("lovelier", "BenchUser")
        self.assertIn("gamemode creative BenchUser", commands)
        self.assertIn("tp BenchUser 0 180 0 0 35", commands)

    def test_overlay_arms_default_to_both_only_for_megaworld(self):
        self.assertEqual(MODULE.overlay_arms("megaworld", None), ["closed", "open"])
        self.assertEqual(MODULE.overlay_arms("terrain", None), ["closed"])
        self.assertEqual(MODULE.overlay_arms("megaworld", "open"), ["open"])
        self.assertEqual(MODULE.overlay_arms("megaworld", "both"), ["closed", "open"])

    def test_client_command_names_the_overlay_arm_explicitly(self):
        command = MODULE._client_command(
            pathlib.Path("/tmp/lodestone"),
            "megaworld",
            25590,
            (2, 2, 3),
            "open",
        )
        index = command.index("--benchmark-debug-overlay")
        self.assertEqual(command[index + 1], "open")

    def test_samply_command_requests_presymbolication(self):
        artifact = pathlib.Path("/tmp/profile.json.gz")
        command = MODULE._samply_command(
            ["/tmp/lodestone", "--benchmark", "megaworld"], artifact
        )
        self.assertEqual(
            command[:4],
            ["samply", "record", "--save-only", "--unstable-presymbolicate"],
        )
        self.assertEqual(
            command[-4:],
            ["--", "/tmp/lodestone", "--benchmark", "megaworld"],
        )

    def test_nearest_rank_percentile_uses_the_observed_tail(self):
        self.assertEqual(MODULE.nearest_rank([1.0, 2.0, 8.0, 9.0], 0.95), 9.0)
        self.assertEqual(MODULE.nearest_rank([1.0, 2.0, 8.0, 9.0], 0.50), 2.0)

    def test_gpu_timestamp_samples_are_summarized_without_adding_passes(self):
        log = "\n".join(
            [
                "gpu: world_total=<no reading yet>, world=<no reading yet>",
                "gpu: world_total=0.10ms, world=0.40ms, first_person=0.20ms, hud_total=0.30ms",
                "gpu: world_total=0.20ms, world=0.60ms, first_person=0.30ms, hud_total=0.40ms",
                "gpu: world_total=0.30ms, world=0.80ms, first_person=0.40ms, hud_total=0.50ms",
            ]
        )

        summary = MODULE.summarize_gpu_log(log)
        self.assertEqual(summary["samples"], 3)
        self.assertEqual(summary["world"]["median_ms"], 0.6)
        self.assertEqual(summary["world"]["p95_ms"], 0.8)
        self.assertEqual(summary["first_person"]["median_ms"], 0.3)
        self.assertNotIn("combined", summary)

    def test_missing_complete_marker_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "completion marker"):
            MODULE.validate_run([], "log without marker", "terrain")

    def test_wrong_workload_metadata_is_rejected(self):
        rows = [
            {"segment": "showcase.stationary", "frame_interval_ms": "10"},
            {"segment": "showcase.moving", "frame_interval_ms": "11"},
        ]
        log = self.fullscreen_log()
        with self.assertRaisesRegex(ValueError, "workload metadata"):
            MODULE.validate_run(rows, log, "terrain")

    def test_incomplete_measured_segments_are_rejected(self):
        rows = [{"segment": "terrain.stationary", "frame_interval_ms": "10"}]
        log = self.fullscreen_log()
        with self.assertRaisesRegex(ValueError, "moving segment"):
            MODULE.validate_run(rows, log, "terrain")

    def test_hardware_builtin_fullscreen_framebuffer_is_parsed(self):
        rows = [
            {"segment": "terrain.stationary", "frame_interval_ms": "10"},
            {"segment": "terrain.moving", "frame_interval_ms": "11"},
        ]
        self.assertEqual(
            MODULE.validate_run(rows, self.fullscreen_log(), "terrain"),
            (3024, 1898),
        )

    def test_external_or_nonfullscreen_window_is_rejected(self):
        rows = [
            {"segment": "terrain.stationary", "frame_interval_ms": "10"},
            {"segment": "terrain.moving", "frame_interval_ms": "11"},
        ]
        external = (
            "benchmark window ready framebuffer_width=2560 "
            "framebuffer_height=1440 fullscreen=true\nbenchmark complete"
        )
        with self.assertRaisesRegex(ValueError, "hardware built-in"):
            MODULE.validate_run(rows, external, "terrain")

        windowed = self.fullscreen_log().replace("fullscreen=true", "fullscreen=false")
        with self.assertRaisesRegex(ValueError, "fullscreen"):
            MODULE.validate_run(rows, windowed, "terrain")


if __name__ == "__main__":
    unittest.main()
