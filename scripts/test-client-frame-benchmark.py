#!/usr/bin/env python3
"""Unit tests for the live client benchmark runner and summarizer."""

import importlib.util
import json
import os
import pathlib
import sys
import tempfile
import unittest
from unittest import mock


RUNNER = pathlib.Path(__file__).with_name("client-frame-benchmark.py")
SPEC = importlib.util.spec_from_file_location("client_frame_benchmark", RUNNER)
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class SummaryTests(unittest.TestCase):
    @staticmethod
    def segment_summary(frames=3):
        return {
            "frames": frames,
            "p50_ms": 4.0,
            "p95_ms": 5.0,
            "p99_ms": 6.0,
            "mean_ms": 4.5,
            "over_16_67": 0,
            "over_33_3": 0,
            "phases_ms": {},
            "workload_counts": {
                "world.model_sections_visited": {
                    "median": 2.0,
                    "p95": 2.0,
                    "max": 2.0,
                },
            },
        }

    @staticmethod
    def emitted_scene(**overrides):
        scene = {
            "schema": 1,
            "spec": {"scenario": "mixed", "seed": 17, "scale": 2},
            "commands": {
                "setup": ["setblock 0 64 0 minecraft:stone"],
                "after_join": [],
                "mutation": ["setblock 0 64 0 minecraft:air"],
            },
            "witnesses": [{
                "segment": "heavyweight.stationary",
                "column": "world.entities_drawn",
                "minimum": 1,
            }],
            "scene_hash": "0" * 64,
        }
        scene.update(overrides)
        return scene

    def test_emitter_consumer_forwards_identity_and_preserves_phase_order(self):
        payload = self.emitted_scene()
        completed = MODULE.subprocess.CompletedProcess([], 0, MODULE.json.dumps(payload), "")
        with mock.patch.object(MODULE.subprocess, "run", return_value=completed) as run:
            scene = MODULE._emit_heavy_scene(
                pathlib.Path("/tmp/heavy-scene-server"), "mixed", 17, 2, "orbit"
            )
        self.assertEqual(scene["commands"]["mutation"], ["setblock 0 64 0 minecraft:air"])
        self.assertIn("--emit-scene", run.call_args.args[0])

    def test_emitter_consumer_rejects_schema_identity_phase_blank_and_witness_controls(self):
        for scene, message in [
            ({**self.emitted_scene(), "schema": 2}, "schema"),
            ({**self.emitted_scene(), "spec": {"scenario": "mixed", "seed": 18, "scale": 2}}, "identity"),
            ({**self.emitted_scene(), "commands": {"after_join": [], "setup": [], "mutation": []}}, "phases"),
            ({**self.emitted_scene(), "commands": {"setup": [" "], "after_join": [], "mutation": []}}, "nonblank"),
            ({**self.emitted_scene(), "witnesses": [{"segment": "heavyweight.stationary", "column": "world.entities_drawn", "minimum": 0}]}, "invalid"),
        ]:
            with self.subTest(message=message):
                with self.assertRaisesRegex(RuntimeError, message):
                    MODULE._validate_emitted_scene(scene, "mixed", 17, 2)

    def test_heavy_setup_datapack_wraps_every_setup_producer_and_is_runner_owned(self):
        scene = self.emitted_scene()
        scene["commands"]["setup"] = [
            "setblock 0 64 0 minecraft:stone",
            "summon minecraft:pig 0 65 0",
        ]
        with tempfile.TemporaryDirectory() as directory:
            pack = MODULE._write_heavy_setup_datapack(pathlib.Path(directory), scene)
            self.assertTrue(pack.root.is_dir())
            self.assertTrue(pack.function_id.startswith("lodestone_heavy:setup_"))
            function_name = pack.function_id.split(":", 1)[1]
            body = (
                pack.root / "data" / "lodestone_heavy" / "function"
                / f"{function_name}.mcfunction"
            ).read_text(encoding="utf-8")
            for command in scene["commands"]["setup"]:
                self.assertIn(f"run {command}", body)
            self.assertIn(
                f"run return {len(scene['commands']['setup'])}",
                body,
            )
            metadata = json.loads((pack.root / "pack.mcmeta").read_text(encoding="utf-8"))
            self.assertEqual(metadata["pack"]["max_format"], MODULE.HEAVY_DATAPACK_FORMAT)
            pack.cleanup()
            self.assertFalse(pack.root.exists())

    def test_dense_setup_uses_one_function_call_but_requires_every_producer_success(self):
        commands = ["kill @e[tag=lodestone_heavy_scene]"] + [
            f"setblock {index} 64 0 minecraft:stone" for index in range(7_936)
        ]
        lines = MODULE._heavy_setup_function_lines(commands, "lh123456789abc")
        self.assertEqual(
            sum("execute store success score" in line for line in lines), 7_936
        )
        self.assertTrue(any(line.endswith("run return 7936") for line in lines))

    def test_heavy_setup_uses_three_bounded_rcon_requests_and_checks_success_count(self):
        scene = self.emitted_scene()
        scene["commands"]["setup"] = ["setblock 0 64 0 minecraft:stone"]
        with tempfile.TemporaryDirectory() as directory:
            with mock.patch.object(MODULE, "_emit_heavy_scene", return_value=scene), mock.patch.object(
                MODULE,
                "run_rcon_commands",
                side_effect=[
                    ["Reloaded"],
                    ["Function lodestone_heavy:setup returned 1"],
                    ["Removed"],
                ],
            ) as run:
                returned, pack = MODULE.prepare_heavy_scene(
                    25571, pathlib.Path(directory), pathlib.Path("/tmp/emitter"),
                    "mixed", 17, 1, "orbit",
                )
            self.assertIs(returned, scene)
            self.assertEqual(run.call_count, 3)
            self.assertEqual(run.call_args_list[0].args[2], ["reload"])
            self.assertEqual(run.call_args_list[1].args[2], [f"function {pack.function_id}"])
            self.assertEqual(
                run.call_args_list[2].args[2],
                [f"scoreboard objectives remove {pack.objective}"],
            )
            pack.cleanup()

    def test_heavy_setup_count_mismatch_removes_its_temporary_datapack(self):
        scene = self.emitted_scene()
        scene["commands"]["setup"] = ["setblock 0 64 0 minecraft:stone"]
        with tempfile.TemporaryDirectory() as directory:
            with mock.patch.object(MODULE, "_emit_heavy_scene", return_value=scene), mock.patch.object(
                MODULE,
                "run_rcon_commands",
                side_effect=[
                    ["Reloaded"],
                    ["Function lodestone_heavy:setup returned 0"],
                    ["Removed"],
                ],
            ):
                with self.assertRaisesRegex(RuntimeError, "reported 0 successful producers, expected 1"):
                    MODULE.prepare_heavy_scene(
                        25571, pathlib.Path(directory), pathlib.Path("/tmp/emitter"),
                        "mixed", 17, 1, "orbit",
                    )
            self.assertEqual(list((pathlib.Path(directory) / "datapacks").iterdir()), [])

    def test_setup_deadline_refuses_to_open_a_new_rcon_request_after_expiry(self):
        with mock.patch.object(MODULE.time, "monotonic", return_value=12.0), mock.patch.object(
            MODULE, "RconClient"
        ) as rcon:
            with self.assertRaisesRegex(TimeoutError, "setup RCON deadline expired before action 0"):
                MODULE.run_rcon_commands(25571, "setup", ["reload"], deadline=11.0)
        rcon.assert_not_called()

    def test_heavy_client_command_forwards_the_emitted_scene_and_mutation_duration(self):
        command = MODULE._client_command(
            pathlib.Path("/tmp/lodestone"), "heavyweight", 25570, (2, 7, 2, 3), "closed",
            camera_plan="orbit", heavy_scene=self.emitted_scene(),
        )
        self.assertEqual(command[command.index("--heavy-scenario") + 1], "mixed")
        self.assertEqual(command[command.index("--heavy-seed") + 1], "17")
        self.assertEqual(command[command.index("--heavy-scale") + 1], "2")
        self.assertEqual(command[command.index("--heavy-camera-plan") + 1], "orbit")
        self.assertEqual(command[command.index("--benchmark-mutation") + 1], "7")

    def test_heavy_witness_requires_a_real_maximum_and_record_keeps_scene_identity(self):
        scene = self.emitted_scene()
        with self.assertRaisesRegex(RuntimeError, "world.entities_drawn.*maximum 0"):
            MODULE._validate_heavy_witnesses(
                scene, {"heavyweight.stationary": {"workload_counts": {"world.entities_drawn": {"max": 0.0}}}}
            )
        with mock.patch.object(MODULE, "_git_sha", return_value="abc"):
            record = MODULE._heavy_record(
                "heavyweight", 1, pathlib.Path("/tmp/lodestone"), (2, 7, 2, 3), "closed",
                2, "orbit", scene, {"stationary": {}},
            )
        self.assertEqual(record["schema"], 2)
        self.assertEqual(record["scene_hash"], "0" * 64)
        self.assertEqual(record["requested_scale"], 2)
        self.assertEqual(record["camera_plan"], "orbit")
        self.assertNotIn("p50_ms", record)

    def test_heavy_profile_artifact_requires_matching_complete_sidecars(self):
        with tempfile.TemporaryDirectory() as directory:
            artifact = pathlib.Path(directory) / "heavyweight-closed-fixed.json.gz"
            artifact.write_bytes(b"capture")
            MODULE._samply_sidecar_path(artifact).write_text("symbols", encoding="utf-8")
            scene = self.emitted_scene()
            with mock.patch.object(MODULE, "_git_sha", return_value="abc"):
                record = MODULE._heavy_record(
                    "heavyweight", 1, pathlib.Path("/tmp/lodestone"), (2, 7, 2, 3), "closed",
                    2, "orbit", scene,
                    {"stationary": self.segment_summary(), "moving": self.segment_summary()},
                )
            MODULE._heavy_profile_record_path(artifact).write_text(
                json.dumps({**record, "capture": str(artifact)}), encoding="utf-8"
            )
            self.assertEqual(
                MODULE.validate_heavy_profile_artifact(artifact)["scene_hash"], scene["scene_hash"]
            )
            self.assertEqual(
                MODULE.validate_heavy_profile_artifact(pathlib.Path(os.path.relpath(artifact)))["scene_hash"],
                scene["scene_hash"],
            )

            MODULE._heavy_profile_record_path(artifact).write_text(
                json.dumps({**record, "capture": "/wrong/profile.json.gz"}), encoding="utf-8"
            )
            with self.assertRaisesRegex(RuntimeError, "does not describe this capture"):
                MODULE.validate_heavy_profile_artifact(artifact)

            MODULE._heavy_profile_record_path(artifact).write_text(
                json.dumps({**record, "capture": str(artifact)}), encoding="utf-8"
            )
            MODULE._samply_sidecar_path(artifact).unlink()
            with self.assertRaisesRegex(RuntimeError, "symbol sidecar"):
                MODULE.validate_heavy_profile_artifact(artifact)

    def test_heavy_profile_validator_is_a_no_workload_command(self):
        with tempfile.TemporaryDirectory() as directory:
            artifact = pathlib.Path(directory) / "profile.json.gz"
            artifact.write_bytes(b"capture")
            MODULE._samply_sidecar_path(artifact).write_text("symbols", encoding="utf-8")
            record = {
                "schema": 2,
                "workload": "heavyweight",
                "profile": "release",
                "capture": str(artifact),
                "scene_hash": "a" * 64,
                "scenario": "mixed",
                "seed": 17,
                "scale": 1,
                "requested_scale": 1,
                "camera_plan": "stationary",
                "durations_seconds": {"warmup": 2, "mutation": 1, "stationary": 2, "moving": 3},
                "segments": {
                    "stationary": self.segment_summary(),
                    "moving": self.segment_summary(),
                },
            }
            MODULE._heavy_profile_record_path(artifact).write_text(json.dumps(record), encoding="utf-8")
            self.assertEqual(MODULE.main(["--validate-heavy-profile", str(artifact)]), 0)

    def test_heavy_profile_validator_rejects_noop_segment_summary(self):
        """Negative controls: an empty or zero-frame phase is not a capture."""
        with tempfile.TemporaryDirectory() as directory:
            artifact = pathlib.Path(directory) / "profile.json.gz"
            artifact.write_bytes(b"capture")
            MODULE._samply_sidecar_path(artifact).write_text("symbols", encoding="utf-8")
            for moving, expected in (
                ({}, "moving.*missing summary fields.*frames"),
                (self.segment_summary(frames=0), "moving.*positive integer frames"),
            ):
                record = {
                    "schema": 2,
                    "workload": "heavyweight",
                    "profile": "release",
                    "capture": str(artifact),
                    "scene_hash": "a" * 64,
                    "scenario": "mixed",
                    "seed": 17,
                    "scale": 1,
                    "requested_scale": 1,
                    "camera_plan": "stationary",
                    "durations_seconds": {"warmup": 2, "mutation": 1, "stationary": 2, "moving": 3},
                    "segments": {
                        "stationary": self.segment_summary(),
                        "moving": moving,
                    },
                }
                MODULE._heavy_profile_record_path(artifact).write_text(json.dumps(record), encoding="utf-8")
                with self.subTest(moving=moving):
                    with self.assertRaisesRegex(RuntimeError, expected):
                        MODULE.validate_heavy_profile_artifact(artifact)

    def test_heavy_profile_validator_rejects_underidentified_camera_and_dense_scale(self):
        with tempfile.TemporaryDirectory() as directory:
            artifact = pathlib.Path(directory) / "profile.json.gz"
            artifact.write_bytes(b"capture")
            MODULE._samply_sidecar_path(artifact).write_text("symbols", encoding="utf-8")
            record = {
                "schema": 2,
                "workload": "heavyweight",
                "profile": "release",
                "capture": str(artifact),
                "scene_hash": "a" * 64,
                "scenario": "dense-mixed",
                "seed": 17,
                "scale": 1,
                "requested_scale": 1,
                "camera_plan": "orbit",
                "durations_seconds": {"warmup": 2, "mutation": 1, "stationary": 2, "moving": 3},
                "segments": {
                    "stationary": self.segment_summary(),
                    "moving": self.segment_summary(),
                },
            }
            record_path = MODULE._heavy_profile_record_path(artifact)
            record_path.write_text(json.dumps(record), encoding="utf-8")
            self.assertEqual(MODULE.main(["--validate-heavy-profile", str(artifact)]), 0)

            record_path.write_text(
                json.dumps({key: value for key, value in record.items() if key != "camera_plan"}),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(RuntimeError, "does not describe this capture"):
                MODULE.validate_heavy_profile_artifact(artifact)

            record_path.write_text(json.dumps({**record, "camera_plan": "flyby"}), encoding="utf-8")
            with self.assertRaisesRegex(RuntimeError, "does not describe this capture"):
                MODULE.validate_heavy_profile_artifact(artifact)

            record_path.write_text(json.dumps({**record, "scale": 2, "requested_scale": 2}), encoding="utf-8")
            with self.assertRaisesRegex(RuntimeError, "does not describe this capture"):
                MODULE.validate_heavy_profile_artifact(artifact)
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
        self.assertEqual(
            MODULE._samply_sidecar_path(artifact).name,
            "profile.json.syms.json",
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

    def test_render_submission_witness_is_required_in_both_segments(self):
        rows = [
            {"segment": "terrain.stationary", "frame_interval_ms": "10"},
            {"segment": "terrain.moving", "frame_interval_ms": "11"},
        ]
        with self.assertRaisesRegex(ValueError, "no world.model_sections_visited samples"):
            MODULE.validate_run(rows, self.fullscreen_log(), "terrain")

        rows[0]["world.model_sections_visited"] = "0"
        rows[1]["world.model_sections_visited"] = "1"
        with self.assertRaisesRegex(ValueError, "stationary.*zero"):
            MODULE.validate_run(rows, self.fullscreen_log(), "terrain")

    def test_render_submission_witness_accepts_positive_integer_counts(self):
        rows = [
            {
                "segment": "terrain.stationary",
                "frame_interval_ms": "10",
                "world.model_sections_visited": "17",
            },
            {
                "segment": "terrain.moving",
                "frame_interval_ms": "11",
                "world.model_sections_visited": "23",
            },
        ]
        self.assertEqual(
            MODULE.validate_run(rows, self.fullscreen_log(), "terrain"),
            (3024, 1898),
        )

    def test_render_submission_witness_rejects_non_count_values(self):
        for value, message in (
            ("1.5", "invalid"),
            ("-1", "invalid"),
            ("nan", "invalid"),
            ("inf", "invalid"),
            ("not-a-number", "non-numeric"),
        ):
            with self.subTest(value=value):
                rows = [
                    {
                        "segment": "terrain.stationary",
                        "frame_interval_ms": "10",
                        "world.model_sections_visited": value,
                    },
                    {
                        "segment": "terrain.moving",
                        "frame_interval_ms": "11",
                        "world.model_sections_visited": "2",
                    },
                ]
                with self.assertRaisesRegex(
                    ValueError, f"{message} world.model_sections_visited"
                ):
                    MODULE.validate_run(rows, self.fullscreen_log(), "terrain")

    def test_hardware_builtin_fullscreen_framebuffer_is_parsed(self):
        rows = [
            {
                "segment": "terrain.stationary",
                "frame_interval_ms": "10",
                "world.model_sections_visited": "17",
            },
            {
                "segment": "terrain.moving",
                "frame_interval_ms": "11",
                "world.model_sections_visited": "23",
            },
        ]
        self.assertEqual(
            MODULE.validate_run(rows, self.fullscreen_log(), "terrain"),
            (3024, 1898),
        )

    def test_external_or_nonfullscreen_window_is_rejected(self):
        rows = [
            {
                "segment": "terrain.stationary",
                "frame_interval_ms": "10",
                "world.model_sections_visited": "17",
            },
            {
                "segment": "terrain.moving",
                "frame_interval_ms": "11",
                "world.model_sections_visited": "23",
            },
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
