"""Contract tests for the external-client runner, without pretending to be a client."""

from __future__ import annotations

import importlib.util
import json
import pathlib
import sys
import tempfile
import time
import unittest


SCRIPT = pathlib.Path(__file__).parents[1] / "external-client-acceptance.py"
SPEC = importlib.util.spec_from_file_location("external_client_acceptance", SCRIPT)
assert SPEC and SPEC.loader
RUNNER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = RUNNER
SPEC.loader.exec_module(RUNNER)


def valid_stages(row: object) -> list[dict[str, object]]:
    return [
        {
            "name": "join",
            "observed": True,
            "observation": "world entered",
            "configuration_mode": row.configuration_mode,
        },
        {
            "name": "chunks",
            "observed": True,
            "observation": "initial chunk delivery observed",
            "batch_mode": row.chunk_batch_mode,
            "batch_count": 1 if row.chunk_batch_mode == "acknowledged" else 0,
        },
        {
            "name": "movement",
            "observed": True,
            "observation": "position update sent",
            "movement_count": 1,
        },
        {
            "name": "break_place",
            "observed": True,
            "observation": "break and place results captured",
            "break_count": 1, "break_result_observed": True,
            "place_count": 1, "place_result_observed": True,
        },
        {"name": "chat", "observed": True, "observation": "message appeared", "message_count": 1, "result_observed": True},
        {"name": "inventory_select", "observed": True, "observation": "slot changed", "selected_slot": 4, "result_observed": True},
        {"name": "keepalive", "observed": True, "observation": "matched exchange", "exchange_count": 1, "id_matched": True},
        {
            "name": "disconnect",
            "observed": True,
            "observation": "client observed EOF",
            "clean": True,
            "initiated_by": "client",
        },
    ]


class EvidenceContractTests(unittest.TestCase):
    def test_matrix_has_one_release_per_hosted_protocol(self) -> None:
        protocols = [row.protocol for row in RUNNER.ROWS]
        self.assertEqual(len(protocols), len(set(protocols)))
        self.assertEqual(protocols, sorted(protocols))
        self.assertEqual(
            protocols,
            [5, 47, 110, 210, 316, 340, 404, 498, 578, 754, 756, 758, 762, 766, 774, 776],
        )
        self.assertEqual(RUNNER.GATE_PROTOCOLS, tuple(protocols))
        gated = {
            row.protocol: (row.release, row.configuration_mode, row.chunk_batch_mode)
            for row in RUNNER.ROWS
            if row.protocol in RUNNER.GATE_PROTOCOLS
        }
        self.assertEqual(
            gated,
            {
                5: ("1.7.10", "login_to_play", "unbatched"),
                47: ("1.8.9", "login_to_play", "unbatched"),
                110: ("1.9.4", "login_to_play", "unbatched"),
                210: ("1.10.2", "login_to_play", "unbatched"),
                316: ("1.11.2", "login_to_play", "unbatched"),
                340: ("1.12.2", "login_to_play", "unbatched"),
                404: ("1.13.2", "login_to_play", "unbatched"),
                498: ("1.14.4", "login_to_play", "unbatched"),
                578: ("1.15.2", "login_to_play", "unbatched"),
                754: ("1.16.5", "login_to_play", "unbatched"),
                756: ("1.17.1", "login_to_play", "unbatched"),
                758: ("1.18.2", "login_to_play", "unbatched"),
                762: ("1.19.4", "login_to_play", "unbatched"),
                766: ("1.20.6", "configuration", "acknowledged"),
                774: ("1.21.11", "configuration", "acknowledged"),
                776: ("26.2", "configuration", "acknowledged"),
            },
        )
        self.assertEqual(
            RUNNER.STAGES,
            (
                "join",
                "chunks",
                "movement",
                "break_place",
                "chat",
                "inventory_select",
                "keepalive",
                "disconnect",
            ),
        )

    def test_evidence_requires_fresh_external_artifacts(self) -> None:
        row = next(row for row in RUNNER.ROWS if row.protocol == 766)
        with tempfile.TemporaryDirectory() as temp:
            directory = pathlib.Path(temp)
            capture = directory / "screen.png"
            log = directory / "release-client.log"
            capture.write_bytes(b"external pixels")
            log.write_text("joined then broke target\n", encoding="utf-8")
            evidence = directory / "evidence.json"
            evidence.write_text(json.dumps(
                {
                    "schema": RUNNER.SCHEMA,
                    "protocol": row.protocol,
                    "release": row.release,
                    "stages": valid_stages(row),
                    "provenance": {
                        "client_binary": "/Applications/Release.app",
                        "client_build": row.release,
                        "capture_method": "ui automation",
                        "capture": "screen.png",
                        "client_log": "release-client.log",
                    },
                },
            ), encoding="utf-8")
            result = RUNNER.validate_evidence(evidence, row, time.time() - 1)
            self.assertEqual(result["client_build"], row.release)
            self.assertEqual([stage["name"] for stage in result["stages"]], list(RUNNER.STAGES))
            self.assertEqual(result["stages"][0]["configuration_mode"], "configuration")
            self.assertEqual(result["stages"][1]["batch_mode"], "acknowledged")
            with self.assertRaisesRegex(RuntimeError, "no fresh evidence"):
                RUNNER.validate_evidence(evidence, row, time.time() + 1)
            evidence_value = json.loads(evidence.read_text(encoding="utf-8"))
            evidence_value["provenance"]["client_build"] = "26.2"
            evidence.write_text(json.dumps(evidence_value), encoding="utf-8")
            with self.assertRaisesRegex(RuntimeError, "client_build must be '1.20.6'"):
                RUNNER.validate_evidence(evidence, row, time.time() - 1)

    def test_exact_client_build_provenance_is_checked_for_every_gate_row(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            directory = pathlib.Path(temp)
            (directory / "screen.png").write_bytes(b"external pixels")
            (directory / "release-client.log").write_text("joined then disconnected\n", encoding="utf-8")
            for row in RUNNER.ROWS:
                with self.subTest(protocol=row.protocol):
                    evidence = directory / f"evidence-{row.protocol}.json"
                    evidence.write_text(
                        json.dumps(
                            {
                                "schema": RUNNER.SCHEMA,
                                "protocol": row.protocol,
                                "release": row.release,
                                "stages": valid_stages(row),
                                "provenance": {
                                    "client_binary": "/Applications/Release.app",
                                    "client_build": row.release,
                                    "capture_method": "UI automation plus client log",
                                    "capture": "screen.png",
                                    "client_log": "release-client.log",
                                },
                            },
                        ),
                        encoding="utf-8",
                    )
                    result = RUNNER.validate_evidence(evidence, row, time.time() - 1)
                    self.assertEqual(result["client_build"], row.release)

    def test_pre_configuration_rows_record_direct_play_and_unbatched_chunks(self) -> None:
        for protocol in (5, 47, 110, 210, 316, 340, 404, 498, 578, 754, 756, 758, 762):
            with self.subTest(protocol=protocol):
                row = next(row for row in RUNNER.ROWS if row.protocol == protocol)
                stages = valid_stages(row)
                result = RUNNER.validate_stages(stages, row)
                self.assertEqual(result[0]["configuration_mode"], "login_to_play")
                self.assertEqual(result[1]["batch_mode"], "unbatched")
                self.assertEqual(result[1]["batch_count"], 0)

    def test_configuration_and_batch_modes_are_row_specific(self) -> None:
        row = next(row for row in RUNNER.ROWS if row.protocol == 762)
        stages = valid_stages(row)
        stages[0]["configuration_mode"] = "configuration"
        with self.assertRaisesRegex(RuntimeError, "join.configuration_mode must be 'login_to_play'"):
            RUNNER.validate_stages(stages, row)

    def test_evidence_rejects_missing_session_stage(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            directory = pathlib.Path(temp)
            evidence = directory / "evidence.json"
            evidence.write_text(
                json.dumps({"schema": RUNNER.SCHEMA, "protocol": 766, "release": "1.20.6", "stages": []}),
                encoding="utf-8",
            )
            row = next(row for row in RUNNER.ROWS if row.protocol == 766)
            with self.assertRaisesRegex(RuntimeError, "exactly 8 ordered entries"):
                RUNNER.validate_evidence(evidence, row, time.time() - 1)

    def test_every_minimum_play_capability_is_enforced(self) -> None:
        row = next(row for row in RUNNER.ROWS if row.protocol == 766)
        corruptions = (
            (3, "place_count", 0, "exactly one observed place"),
            (4, "message_count", 0, "exactly one observed message"),
            (5, "selected_slot", 9, "must be in 0..=8"),
            (6, "id_matched", False, "exactly one matched exchange"),
        )
        for index, field, value, error in corruptions:
            with self.subTest(stage=RUNNER.STAGES[index], field=field):
                stages = valid_stages(row)
                stages[index][field] = value
                with self.assertRaisesRegex(RuntimeError, error):
                    RUNNER.validate_stages(stages, row)

    def test_stage_contract_rejects_runner_forced_disconnect(self) -> None:
        row = next(row for row in RUNNER.ROWS if row.protocol == 766)
        stages = valid_stages(row)
        stages[7]["observation"] = "runner stopped server"
        stages[7]["initiated_by"] = "runner"
        with self.assertRaisesRegex(RuntimeError, "initiated_by must be 'client'"):
            RUNNER.validate_stages(stages, row)


if __name__ == "__main__":
    unittest.main()
