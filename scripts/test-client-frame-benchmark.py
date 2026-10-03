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
                "gpu: no fresh sample",
                "gpu: timer=1 frame=3 age_frames=2 world=0.40ms first_person=0.20ms",
                "gpu: timer=1 frame=4 age_frames=2 world=0.60ms first_person=0.30ms",
                "gpu: timer=1 frame=5 age_frames=2 world=0.80ms first_person=0.40ms",
                "gpu: timer=1 frame=5 age_frames=8 world=0.80ms first_person=0.40ms",
            ]
        )

        summary = MODULE.summarize_gpu_log(log)
        self.assertEqual(summary["samples"], 3)
        self.assertEqual(summary["world"]["median_ms"], 0.6)
        self.assertEqual(summary["world"]["p95_ms"], 0.8)
        self.assertEqual(summary["first_person"]["median_ms"], 0.3)
        self.assertNotIn("combined", summary)
        self.assertNotIn("world_total", summary)
        self.assertFalse(summary["calibration_verified"])
        absent = MODULE.summarize_gpu_log("gpu: timer=2 frame=5 age_frames=2 world=0.80ms first_person=not_run")
        self.assertEqual(absent["world"]["samples"], 1)
        self.assertNotIn("first_person", absent)

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


class ArtifactIdentityTests(unittest.TestCase):
    ABC_SHA256 = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"

    def test_retention_keeps_original_raw_files_and_excludes_account_data(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            record = {"schema": 1, "status": "incomplete"}
            with MODULE._trial_workspace("trial-", root, record) as workspace:
                (workspace / "frames.csv").write_text("frame,frame_interval_ms,segment,present\n1,17,terrain.moving,0.2\n", encoding="utf-8")
                (workspace / "client.log").write_bytes(b"original log\r\n")
                (workspace / "resources.json").write_text('{"synthetic":true}', encoding="utf-8")
                (workspace / "data").mkdir()
                (workspace / "data" / "offline.json").write_text("account state", encoding="utf-8")
                record["status"] = "complete"
            self.assertFalse(workspace.exists())
            retained = next(root.iterdir())
            self.assertEqual({path.name for path in retained.iterdir()}, {"frames.csv", "client.log", "resources.json", "trial.json"})
            self.assertEqual((retained / "resources.json").read_text(), '{"synthetic":true}')
            self.assertEqual((retained / "client.log").read_bytes(), b"original log\r\n")
            metadata = json.loads((retained / "trial.json").read_text(encoding="utf-8"))
            self.assertEqual(metadata["status"], "complete")
            self.assertEqual(metadata["observed"]["segments"]["terrain.moving"]["present_rows"], 1)
            self.assertEqual(metadata["artifacts"]["client.log"]["bytes"], 14)
            with MODULE._trial_workspace("trial-", root, {}) as second:
                self.assertTrue(second.is_dir())
            self.assertEqual(len(list(root.iterdir())), 2)

    def test_retention_preserves_failure_evidence_without_changing_exception(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            with self.assertRaisesRegex(RuntimeError, "controlled failure"):
                with MODULE._trial_workspace("trial-", root, {"status": "incomplete"}) as workspace:
                    (workspace / "client.log").write_text("failed before CSV", encoding="utf-8")
                    raise RuntimeError("controlled failure")
            retained = next(root.iterdir())
            metadata = json.loads((retained / "trial.json").read_text(encoding="utf-8"))
            self.assertEqual(metadata["status"], "failed")
            self.assertEqual(set(metadata["artifacts"]), {"client.log"})

    def snapshot_fixture(self, directory, files=None, **extra):
        root = pathlib.Path(directory)
        world = root / "oracle" / "world"
        (world / "region").mkdir(parents=True)
        (world / "level.dat").write_bytes(b"abc")
        (world / "region" / "r.0.0.mca").write_bytes(b"abc")
        declaration = root / "snapshot.json"
        declaration.write_text(json.dumps({"schema": 1, "files": files or ["level.dat"], **extra}), encoding="utf-8")
        return declaration, {"world": world.parent, "game_port": 25580, "rcon_port": 25581}

    def test_snapshot_hashes_actual_declared_bytes_not_archive_marker(self):
        with tempfile.TemporaryDirectory() as directory:
            declaration, oracle = self.snapshot_fixture(directory)
            (oracle["world"] / ".lodestone-benchmark-world.json").write_text('{"archive_sha256":"irrelevant"}', encoding="utf-8")
            expected = MODULE.hashlib.sha256(json.dumps({"schema": 1, "files": [{
                "path": "level.dat", "bytes": 3, "sha256": self.ABC_SHA256,
            }]}, sort_keys=True, separators=(",", ":")).encode("utf-8")).hexdigest()
            with mock.patch.object(MODULE, "_require_offline_oracle"):
                identity = MODULE._world_snapshot_identity(declaration, oracle)
                self.assertEqual(identity["sha256"], expected)
                self.assertFalse(identity["trial_restore_verified"])
                (oracle["world"] / "world" / "level.dat").write_bytes(b"changed")
                changed = MODULE._world_snapshot_identity(declaration, oracle)
            self.assertNotEqual(changed["sha256"], expected)

    def test_snapshot_rejects_digest_mismatch_paths_symlinks_and_bounds(self):
        with tempfile.TemporaryDirectory() as directory:
            declaration, oracle = self.snapshot_fixture(directory, snapshot_sha256="0" * 64)
            with mock.patch.object(MODULE, "_require_offline_oracle"):
                with self.assertRaisesRegex(ValueError, "digest mismatch"):
                    MODULE._world_snapshot_identity(declaration, oracle)
                for name in ("../escape.dat", "/level.dat", "region//r.0.0.mca", "region/../level.dat"):
                    declaration.write_text(json.dumps({"schema": 1, "files": [name]}), encoding="utf-8")
                    with self.subTest(name=name), self.assertRaisesRegex(ValueError, "canonical and relative"):
                        MODULE._world_snapshot_identity(declaration, oracle)
                (oracle["world"] / "world" / "linked.dat").symlink_to("level.dat")
                declaration.write_text('{"schema":1,"files":["linked.dat"]}', encoding="utf-8")
                with self.assertRaisesRegex(ValueError, "symbolic link"):
                    MODULE._world_snapshot_identity(declaration, oracle)
                declaration.write_text('{"schema":1,"files":["level.dat"]}', encoding="utf-8")
                with mock.patch.object(MODULE, "MAX_SNAPSHOT_BYTES", 2), self.assertRaisesRegex(ValueError, "exceeds"):
                    MODULE._world_snapshot_identity(declaration, oracle)

    def test_live_oracle_is_refused_before_snapshot_hashing(self):
        with tempfile.TemporaryDirectory() as directory:
            declaration, oracle = self.snapshot_fixture(directory)
            with mock.patch.object(MODULE.socket, "create_connection") as connect, mock.patch.object(MODULE, "_sha256_file") as hash_file:
                with self.assertRaisesRegex(RuntimeError, "stop the oracle"):
                    MODULE._world_snapshot_identity(declaration, oracle)
                hash_file.assert_not_called()
                connect.return_value.close.assert_called_once()

    def test_identity_records_explicit_resource_bytes_and_unverified_settings(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            binary = root / "lodestone"
            binary.write_bytes(b"abc")
            (root / "lodestone-resources.zip").write_bytes(b"abc")
            with mock.patch.dict(MODULE.os.environ, {"LODESTONE_ASSETS": str(root)}), mock.patch.object(MODULE, "_git_sha", return_value="a" * 40):
                identity = MODULE._comparison_identity(binary, "terrain", None)
            self.assertEqual(identity["binary_sha256"], self.ABC_SHA256)
            self.assertEqual(identity["resources"]["files"]["lodestone-resources.zip"], self.ABC_SHA256)
            self.assertIsNone(identity["resources"]["files"]["client.jar"])
            self.assertFalse(identity["resources"]["official_provenance_verified"])
            for name in ("world_snapshot", "gpu_adapter", "actual_present_mode", "effective_graphics_settings", "binary_build_profile_verified"):
                self.assertIsNone(identity[name])
            self.assertEqual(identity["sha256"], MODULE._identity_digest({key: value for key, value in identity.items() if key != "sha256"}))

    def test_observation_distinguishes_redraw_rows_from_presented_work(self):
        metadata = MODULE._observed_trial_metadata([
            {"segment": "terrain.moving", "present": ""},
            {"segment": "terrain.moving", "present": "0.1"},
        ], "framebuffer_width=1920 framebuffer_height=1080 fullscreen=true render_distance=24 present_mode=AutoNoVsync benchmark window ready\n")
        self.assertEqual(metadata["window"]["framebuffer_width"], 1920)
        self.assertEqual(metadata["segments"]["terrain.moving"], {"redraw_rows": 2, "present_rows": 1})
        self.assertIsNone(metadata["actual_present_mode"])
        self.assertFalse(metadata["displayed_frame_cadence_verified"])

    def test_nonfinite_and_nonpositive_frame_intervals_are_rejected(self):
        for interval in ("nan", "inf", "-1", "0"):
            with self.subTest(interval=interval), self.assertRaisesRegex(ValueError, "positive and finite"):
                MODULE.summarize_rows([{"frame_interval_ms": interval}])

    def test_snapshot_flag_requires_retained_identity(self):
        with self.assertRaises(SystemExit):
            MODULE.parse_args(["--workload", "terrain", "--world-snapshot-manifest", "snapshot.json"])
        args = MODULE.parse_args(["--workload", "terrain", "--artifact-dir", "artifacts", "--world-snapshot-manifest", "snapshot.json"])
        self.assertEqual(args.artifact_dir, pathlib.Path("artifacts"))


class ComparisonRunnerTests(unittest.TestCase):
    @staticmethod
    def settings(**overrides):
        settings = {
            "framerate_limit": 60, "enable_vsync": False, "inactivity_fps_limit": "minimized",
            "fov": 70, "render_distance": 17, "graphics_preset": "custom", "cloud_status": "off",
            "cutout_leaves": True, "biome_blend_radius": 2, "entity_shadows": True, "particles": "all",
        }
        return {**settings, **overrides}

    @staticmethod
    def rows():
        return [
            {"segment": "terrain.stationary", "frame_interval_ms": "10", "world.model_sections_visited": "17"},
            {"segment": "terrain.moving", "frame_interval_ms": "11", "world.model_sections_visited": "23"},
        ]

    def log(self, settings=None, window="windowed", pacing="options", cap="60", vsync="false", mode="Some(AutoNoVsync)"):
        settings = self.settings() if settings is None else settings
        fields = " ".join(f"{key}={json.dumps(value)}" for key, value in settings.items())
        return (
            f"benchmark window ready framebuffer_width=2560 framebuffer_height=1440 fullscreen=false {fields} "
            f"benchmark_window={window} benchmark_pacing={pacing} target_fps={cap} effective_vsync={vsync} "
            f"configured_present_mode={mode}\nsegment=terrain.warmup\nbenchmark complete"
        )

    @staticmethod
    def capture():
        return {
            "schema": 2, "metric": "successful-presentation-submission", "clock": "portable-monotonic",
            "timeUnit": "microseconds", "rowScope": "retained-prefix", "aggregateScope": "whole-capture",
            "maxRows": 4096, "elapsedUs": 200000, "attempts": 4, "submissions": 3,
            "skippedAttempts": 1, "menuSubmissions": 0, "worldSubmissions": 3, "droppedRows": 0,
            "rejectedSubmissions": 0, "intervalCount": 2, "intervalSumUs": 140000,
            "intervalMinUs": 40000, "intervalMaxUs": 100000,
            "columns": ["attempt", "startedUs", "finishedUs", "outcome", "submission", "intervalUs",
                        "targetFps", "vsync", "gpuCompletionCallbackUs"],
            "outcomes": {"0": "not-submitted", "1": "menu", "2": "world"},
            "rows": [[1, 0, 10000, 2, 1, None, 60, 0, None],
                     [2, 20000, 25000, 0, None, None, None, None, None],
                     [3, 30000, 50000, 2, 2, 40000, 60, 0, 1234],
                     [4, 60000, 150000, 2, 3, 100000, 60, 0, None]],
        }

    def test_settings_require_complete_known_fields_and_typed_ranges(self):
        self.assertEqual(MODULE.validate_settings(self.settings()), self.settings())
        bad_values = {
            "framerate_limit": [9, 261, True, 60.0], "render_distance": [1, 257],
            "fov": [29, 111], "biome_blend_radius": [-1, 8],
            "enable_vsync": [0, "false"], "cutout_leaves": [1], "entity_shadows": [None],
            "inactivity_fps_limit": ["none"], "graphics_preset": ["Fancy"],
            "cloud_status": ["none", []], "particles": ["some"],
        }
        for name, values in bad_values.items():
            for value in values:
                with self.subTest(name=name, value=value), self.assertRaisesRegex(ValueError, name):
                    MODULE.validate_settings(self.settings(**{name: value}))
        for declaration in ({}, self.settings(entity_distance=1), {name: value for name, value in self.settings().items() if name != "fov"}):
            with self.subTest(declaration=declaration), self.assertRaisesRegex(ValueError, "exactly"):
                MODULE.validate_settings(declaration)
        for values in ({"framerate_limit": 10, "fov": 30, "render_distance": 2, "biome_blend_radius": 0},
                       {"framerate_limit": 260, "fov": 110, "render_distance": 256, "biome_blend_radius": 7}):
            self.assertEqual(MODULE.validate_settings(self.settings(**values)), self.settings(**values))

    def test_duplicate_settings_fields_are_rejected_before_launch(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "settings.json"
            path.write_text(json.dumps(self.settings())[:-1] + ', "fov": 90}', encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "duplicate JSON field"):
                MODULE._read_settings(path)

    def test_cli_requires_settings_for_new_policies_and_validates_durations(self):
        with mock.patch.object(MODULE.sys, "stderr"):
            for arguments in (["--benchmark-window", "windowed"], ["--benchmark-pacing", "options"],
                              ["--warmup-seconds", "-1"], ["--stationary-seconds", "0"], ["--moving-seconds", "0"]):
                with self.subTest(arguments=arguments), self.assertRaises(SystemExit):
                    MODULE.parse_args(["--workload", "terrain", *arguments])
            with self.assertRaises(SystemExit):
                MODULE.parse_args(["--workload", "heavyweight", "--heavy-scenario", "mixed", "--stationary-seconds", "100"])
        args = MODULE.parse_args(["--workload", "terrain", "--smoke", "--warmup-seconds", "7", "--stationary-seconds", "4"])
        self.assertEqual(args.durations, (7, 4, 3))
        self.assertEqual(args.benchmark_window, "builtin-fullscreen")
        self.assertEqual(args.benchmark_pacing, "uncapped")
        args = MODULE.parse_args(["--workload", "heavyweight", "--heavy-scenario", "mixed", "--smoke", "--moving-seconds", "5"])
        self.assertEqual(args.durations, (2, 1, 2, 5))
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "settings.json"
            path.write_text(json.dumps(self.settings()), encoding="utf-8")
            args = MODULE.parse_args(["--workload", "terrain", "--settings", str(path), "--benchmark-window", "windowed", "--benchmark-pacing", "options"])
            self.assertEqual(args.graphics_settings, self.settings())

    def test_windowed_resolution_is_forwarded_and_witnessed(self):
        self.assertEqual(MODULE._physical_resolution("1280x720"), (1280, 720))
        command = MODULE._client_command(
            pathlib.Path("lodestone"), "terrain", 25565, (20, 3, 1), "closed",
            benchmark_window="windowed", benchmark_resolution=(1280, 720),
        )
        self.assertEqual(command[command.index("--benchmark-resolution") + 1], "1280x720")
        log = self.log().replace("framebuffer_width=2560 framebuffer_height=1440",
                                 "framebuffer_width=1280 framebuffer_height=720")
        self.assertEqual(MODULE.validate_run(self.rows(), log, "terrain", "windowed", "options",
                                            self.settings(), (1280, 720)), (1280, 720))
        with self.assertRaisesRegex(ValueError, "2560x1440"):
            MODULE.validate_run(self.rows(), log, "terrain", "windowed", "options", self.settings())
        for value in ("1280", "1280x720x2", "0x720", "1280x0", "8193x720", "1280x8193"):
            with self.subTest(value=value), self.assertRaises(MODULE.argparse.ArgumentTypeError):
                MODULE._physical_resolution(value)
        with mock.patch.object(MODULE.sys, "stderr"), self.assertRaises(SystemExit):
            MODULE.parse_args(["--workload", "terrain", "--benchmark-resolution", "1280x720"])

    def test_options_pacing_preserves_cap_and_unlimited_sentinel(self):
        self.assertEqual(MODULE._requested_pacing(self.settings(), "options"), (60, False))
        self.assertEqual(MODULE._requested_pacing(self.settings(framerate_limit=260, enable_vsync=True), "options"), (None, True))
        self.assertEqual(MODULE._requested_pacing(self.settings(enable_vsync=True), "uncapped"), (None, False))

    def test_windowed_validation_requires_exact_physical_size_and_effective_settings(self):
        self.assertEqual(MODULE.validate_run(self.rows(), self.log(), "terrain", "windowed", "options", self.settings()), (2560, 1440))
        for replacement in (("framebuffer_height=1440", "framebuffer_height=1439"),
                            ("framebuffer_width=2560", "framebuffer_width=2559"), ("fullscreen=false", "fullscreen=true")):
            with self.subTest(replacement=replacement), self.assertRaisesRegex(ValueError, "physical 2560x1440"):
                MODULE.validate_run(self.rows(), self.log().replace(*replacement), "terrain", "windowed", "options", self.settings())
        with self.assertRaisesRegex(ValueError, "declared settings"):
            MODULE.validate_run(self.rows(), self.log(), "terrain", "windowed", "options")
        for log in (self.log().replace("fov=70", "fov=71"), self.log().replace("fov=70", "")):
            with self.assertRaisesRegex(ValueError, "effective graphics"):
                MODULE.validate_run(self.rows(), log, "terrain", "windowed", "options", self.settings())

    def test_observed_policy_cap_vsync_and_present_request_must_match(self):
        controls = (("benchmark_window=windowed", "benchmark_window=builtin-fullscreen", "benchmark policy"),
                    ("benchmark_pacing=options", "benchmark_pacing=uncapped", "benchmark policy"),
                    ("target_fps=60", "target_fps=90", "effective pacing"),
                    ("effective_vsync=false", "effective_vsync=true", "effective pacing"),
                    ("configured_present_mode=Some(AutoNoVsync)", "", "observation is missing"),
                    ("configured_present_mode=Some(AutoNoVsync)", "configured_present_mode=Some(Fifo)", "no-VSync request"))
        for original, replacement, message in controls:
            with self.subTest(replacement=replacement), self.assertRaisesRegex(ValueError, message):
                MODULE.validate_run(self.rows(), self.log().replace(original, replacement), "terrain", "windowed", "options", self.settings())
        uncapped = self.log(pacing="uncapped", cap="none")
        MODULE.validate_run(self.rows(), uncapped, "terrain", "windowed", "uncapped", self.settings())
        with self.assertRaisesRegex(ValueError, "effective pacing"):
            MODULE.validate_run(self.rows(), uncapped.replace("target_fps=none", ""), "terrain", "windowed", "uncapped", self.settings())
        settings = self.settings(enable_vsync=True)
        MODULE.validate_run(self.rows(), self.log(settings=settings, vsync="true", mode="Some(Fifo)"), "terrain", "windowed", "options", settings)

    def test_colored_and_message_last_metadata_preserves_exact_declared_settings(self):
        log = self.log().replace("benchmark window ready ", "", 1).replace("\nsegment=", " benchmark window ready\nsegment=", 1)
        log = log.replace("framerate_limit=60", "\x1b[3mframerate_limit\x1b[0m\x1b[2m=\x1b[0m60")
        self.assertEqual(MODULE.validate_run(self.rows(), log, "terrain", "windowed", "options", self.settings()), (2560, 1440))
        observed = MODULE._observed_trial_metadata(self.rows(), log)
        self.assertEqual(observed["effective_graphics_settings"], self.settings())
        self.assertEqual(observed["window"]["configured_present_mode"], "AutoNoVsync")

    def test_client_and_server_distances_follow_declared_settings(self):
        command = MODULE._client_command(pathlib.Path("/tmp/lodestone"), "terrain", 25580, (7, 4, 3), "closed",
                                         render_distance=17, benchmark_window="windowed", benchmark_pacing="options")
        for flag, value in (("--render-distance", "17"), ("--protocol", "776"), ("--benchmark-window", "windowed"),
                            ("--benchmark-pacing", "options"), ("--benchmark-warmup", "7"), ("--benchmark-stationary", "4")):
            self.assertEqual(command[command.index(flag) + 1], value)
        with mock.patch.object(MODULE, "_server_view_distance") as view, mock.patch.object(MODULE.subprocess, "run"), mock.patch.object(MODULE, "RconClient"):
            MODULE.start_oracle("terrain", 17)
        view.assert_called_once_with(MODULE.ORACLES["terrain"]["world"], 18)

    def test_requested_identity_changes_with_settings_window_and_pacing(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = pathlib.Path(directory) / "lodestone"
            binary.write_bytes(b"abc")
            with mock.patch.object(MODULE, "_git_sha", return_value="a" * 40), mock.patch.dict(MODULE.os.environ, {}, clear=True):
                identity = MODULE._comparison_identity(binary, "terrain", None, self.settings(), "windowed", "options")
                changed = [MODULE._comparison_identity(binary, "terrain", None, settings, window, pacing) for settings, window, pacing in
                           ((self.settings(framerate_limit=90), "windowed", "options"),
                            (self.settings(), "builtin-fullscreen", "options"), (self.settings(), "windowed", "uncapped"))]
                vsync = MODULE._comparison_identity(binary, "terrain", None, self.settings(enable_vsync=True), "windowed", "options")
            self.assertEqual(identity["render_distance_requested"], 17)
            self.assertEqual(identity["server_view_distance_requested"], 18)
            self.assertEqual(identity["requested_frame_cap"], 60)
            self.assertFalse(identity["requested_vsync"])
            self.assertEqual(identity["requested_present_mode"], "AutoNoVsync")
            self.assertIsNone(identity["effective_graphics_settings"])
            self.assertIsNone(identity["actual_present_mode"])
            self.assertTrue(all(other["sha256"] != identity["sha256"] for other in changed))
            self.assertIsNone(vsync["requested_present_mode"])
            self.assertEqual(vsync["requested_present_mode_policy"], "surface default")

    def test_graphics_only_options_and_presentation_are_retained_with_hashes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            record = {}
            with MODULE._trial_workspace("trial-", root, record) as workspace:
                (workspace / "data").mkdir()
                (workspace / "data" / "offline.json").write_text("private", encoding="utf-8")
                digest = MODULE._write_trial_options(workspace, self.settings())
                self.assertEqual((workspace / "options.json").read_bytes(), (workspace / "data" / "options.json").read_bytes())
                (workspace / "presentation.json").write_text(json.dumps(self.capture()), encoding="utf-8")
            retained = next(root.iterdir())
            self.assertEqual({path.name for path in retained.iterdir()}, {"options.json", "presentation.json", "trial.json"})
            self.assertEqual(json.loads((retained / "options.json").read_text()), self.settings())
            self.assertEqual(record["artifacts"]["options.json"]["sha256"], digest)
            self.assertEqual(record["artifacts"]["presentation.json"]["sha256"], MODULE._sha256_file(retained / "presentation.json"))

    def test_presentation_summary_counts_real_handoffs_and_stall_intervals(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "presentation.json"
            path.write_text(json.dumps(self.capture()), encoding="utf-8")
            summary = MODULE.summarize_presentation_capture(path, self.settings(), "options")
        self.assertEqual(summary["elapsed_seconds"], 0.2)
        self.assertEqual(summary["submissions"], 3)
        self.assertEqual(summary["skipped_attempts"], 1)
        self.assertEqual(summary["successful_handoffs_per_second"], 15)
        self.assertEqual((summary["p50_ms"], summary["p95_ms"], summary["p99_ms"]), (40, 100, 100))
        self.assertFalse(summary["displayed_frame_cadence_verified"])

    def test_presentation_negative_controls_reject_missing_truncated_noop_and_mislabeled_data(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "presentation.json"
            with self.assertRaisesRegex(RuntimeError, "presentation capture"):
                MODULE.summarize_presentation_capture(path, self.settings(), "options")
            for field, value, message in (("schema", 1, "schema 2"), ("droppedRows", 1, "droppedRows"),
                                           ("worldSubmissions", 0, "world handoffs"), ("elapsedUs", 0, "elapsed"),
                                           ("submissions", 4, "aggregates"), ("attempts", 5, "every attempt"),
                                           ("columns", ["wrong"], "columns"), ("rejectedSubmissions", 1, "rejected submissions")):
                capture = self.capture()
                capture[field] = value
                path.write_text(json.dumps(capture), encoding="utf-8")
                with self.subTest(field=field), self.assertRaisesRegex(ValueError, message):
                    MODULE.summarize_presentation_capture(path, self.settings(), "options")
            for index, column, value, message in ((0, 6, 90, "effective pacing"), (0, 7, 1, "effective pacing"),
                                                  (3, 5, 99000, "submission cadence"), (1, 4, 1, "skipped submission"),
                                                  (0, 2, 200001, "attempt timing")):
                capture = self.capture()
                capture["rows"][index][column] = value
                path.write_text(json.dumps(capture), encoding="utf-8")
                with self.subTest(index=index, column=column), self.assertRaisesRegex(ValueError, message):
                    MODULE.summarize_presentation_capture(path, self.settings(), "options")

    def test_microsecond_rounding_does_not_reject_valid_presentation_intervals(self):
        capture = self.capture()
        capture["rows"][2][2] += 1
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "presentation.json"
            path.write_text(json.dumps(capture), encoding="utf-8")
            self.assertEqual(MODULE.summarize_presentation_capture(path, self.settings(), "options")["p50_ms"], 40)

    def test_run_trial_wires_declared_options_capture_and_observed_metadata(self):
        settings = self.settings()
        resources = mock.Mock()
        resources.take_sample.return_value = {"rss_bytes": None}
        resources.sample_if_due.return_value = None
        resources.report.return_value = {"summary": {}, "gpu_memory": {}}
        process = mock.Mock(pid=12345)
        process.poll.side_effect = [None, 0]
        process.wait.return_value = 0
        launch = {}

        def fake_launch(command, **kwargs):
            launch.update(command=command, env=kwargs["env"])
            env = kwargs["env"]
            data = pathlib.Path(env["LODESTONE_DATA_DIR"])
            self.assertEqual(json.loads((data / "options.json").read_text()), settings)
            self.assertEqual({path.name for path in data.iterdir()}, {"offline.json", "options.json"})
            self.assertEqual(env["LODESTONE_PRESENTATION_CAPTURE_SEGMENT"], "terrain.stationary")
            kwargs["stdout"].write(self.log())
            kwargs["stdout"].flush()
            pathlib.Path(env["LODESTONE_FRAME_PROFILE_DUMP"]).write_text(
                "frame,frame_interval_ms,segment,world.model_sections_visited\n"
                "1,10,terrain.stationary,17\n2,11,terrain.moving,23\n", encoding="utf-8",
            )
            pathlib.Path(env["LODESTONE_PRESENTATION_CAPTURE"]).write_text(json.dumps(self.capture()), encoding="utf-8")
            return process

        with tempfile.TemporaryDirectory() as directory, mock.patch.object(MODULE.subprocess, "Popen", side_effect=fake_launch), mock.patch.object(MODULE, "_resource_sampler", return_value=resources), mock.patch.object(MODULE, "configure_joined_player") as joined, mock.patch.object(MODULE.time, "sleep"):
            root = pathlib.Path(directory)
            result = MODULE.run_trial("terrain", 1, pathlib.Path("/tmp/lodestone"), MODULE.ORACLES["terrain"],
                                      (7, 4, 3), "closed", artifact_dir=root, settings=settings,
                                      benchmark_window="windowed", benchmark_pacing="options")
            retained = next(root.iterdir())
            record = json.loads((retained / "trial.json").read_text())
            self.assertEqual(record["status"], "complete")
            self.assertEqual(record["observed"]["effective_graphics_settings"], settings)
            self.assertEqual(record["options_sha256"], record["artifacts"]["options.json"]["sha256"])
            self.assertEqual(record["presentation"]["sha256"], record["artifacts"]["presentation.json"]["sha256"])
            self.assertEqual(record["presentation"]["segment"], "terrain.stationary")
            self.assertFalse((retained / "offline.json").exists())
            with mock.patch.object(MODULE, "RESULTS", root / "history.jsonl"), mock.patch.object(MODULE, "_git_sha", return_value="a" * 40):
                MODULE._append_records("terrain", result, (7, 4, 3), pathlib.Path("/tmp/lodestone"))
            history = [json.loads(line) for line in (root / "history.jsonl").read_text().splitlines()]
            self.assertEqual({entry["render_distance"] for entry in history}, {17})
            self.assertEqual(history[0]["presentation"]["p95_ms"], 100)
            self.assertEqual(history[0]["p95_ms"], 10)
        self.assertEqual(launch["command"][launch["command"].index("--render-distance") + 1], "17")
        self.assertEqual(launch["command"][launch["command"].index("--benchmark-window") + 1], "windowed")
        joined.assert_called_once()


if __name__ == "__main__":
    unittest.main()
