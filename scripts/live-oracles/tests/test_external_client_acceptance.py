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


class EvidenceContractTests(unittest.TestCase):
    def test_matrix_has_one_release_per_hosted_protocol(self) -> None:
        protocols = [row.protocol for row in RUNNER.ROWS]
        self.assertEqual(len(protocols), len(set(protocols)))
        self.assertEqual(protocols, sorted(protocols))
        self.assertIn(5, protocols)
        self.assertIn(766, protocols)
        self.assertIn(774, protocols)
        self.assertIn(776, protocols)
        self.assertEqual(RUNNER.GATE_PROTOCOLS, (766, 774))
        self.assertEqual(
            RUNNER.STAGES,
            (
                "configuration",
                "chunk_batch_acknowledgement",
                "join",
                "movement",
                "play_action",
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
                    "stages": [
                        {"name": "configuration", "observed": True, "observation": "finish accepted"},
                        {
                            "name": "chunk_batch_acknowledgement",
                            "observed": True,
                            "observation": "initial batch acknowledged",
                            "batch_count": 1,
                        },
                        {"name": "join", "observed": True, "observation": "world entered"},
                        {
                            "name": "movement",
                            "observed": True,
                            "observation": "one deliberate position update",
                            "movement_count": 1,
                        },
                        {
                            "name": "play_action",
                            "observed": True,
                            "observation": "block break result captured",
                            "kind": RUNNER.ACTION,
                            "action_count": 1,
                            "result_observed": True,
                        },
                        {
                            "name": "disconnect",
                            "observed": True,
                            "observation": "client closed the session and saw EOF",
                            "clean": True,
                            "initiated_by": "client",
                        },
                    ],
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
            with self.assertRaisesRegex(RuntimeError, "no fresh evidence"):
                RUNNER.validate_evidence(evidence, row, time.time() + 1)

    def test_evidence_rejects_missing_session_stage(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            directory = pathlib.Path(temp)
            evidence = directory / "evidence.json"
            evidence.write_text(
                json.dumps({"schema": RUNNER.SCHEMA, "protocol": 766, "release": "1.20.6", "stages": []}),
                encoding="utf-8",
            )
            row = next(row for row in RUNNER.ROWS if row.protocol == 766)
            with self.assertRaisesRegex(RuntimeError, "exactly 6 ordered entries"):
                RUNNER.validate_evidence(evidence, row, time.time() - 1)

    def test_stage_contract_rejects_runner_forced_disconnect(self) -> None:
        stages = [
            {"name": "configuration", "observed": True, "observation": "finish accepted"},
            {
                "name": "chunk_batch_acknowledgement",
                "observed": True,
                "observation": "batch acknowledged",
                "batch_count": 1,
            },
            {"name": "join", "observed": True, "observation": "world entered"},
            {"name": "movement", "observed": True, "observation": "position update", "movement_count": 1},
            {
                "name": "play_action",
                "observed": True,
                "observation": "result captured",
                "kind": RUNNER.ACTION,
                "action_count": 1,
                "result_observed": True,
            },
            {
                "name": "disconnect",
                "observed": True,
                "observation": "runner stopped server",
                "clean": True,
                "initiated_by": "runner",
            },
        ]
        with self.assertRaisesRegex(RuntimeError, "initiated_by must be 'client'"):
            RUNNER.validate_stages(stages)


if __name__ == "__main__":
    unittest.main()
