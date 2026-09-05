"""Contract tests for the external-client runner, without pretending to be a client."""

from __future__ import annotations

import importlib.util
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
        self.assertIn(776, protocols)

    def test_evidence_requires_fresh_external_artifacts(self) -> None:
        row = RUNNER.ROWS[0]
        with tempfile.TemporaryDirectory() as temp:
            directory = pathlib.Path(temp)
            capture = directory / "screen.png"
            log = directory / "release-client.log"
            capture.write_bytes(b"external pixels")
            log.write_text("joined then broke target\n", encoding="utf-8")
            evidence = directory / "evidence.json"
            evidence.write_text(
                '{"schema":1,"protocol":5,"release":"1.7.10",'
                '"join":{"observed":true},'
                '"play_action":{"kind":"start_destroy_block","observed":true},'
                '"provenance":{"client_binary":"/Applications/Release.app",'
                '"client_build":"1.7.10","capture_method":"ui automation",'
                '"capture":"screen.png","client_log":"release-client.log"}}',
                encoding="utf-8",
            )
            result = RUNNER.validate_evidence(evidence, row, time.time() - 1)
            self.assertEqual(result["client_build"], "1.7.10")
            with self.assertRaisesRegex(RuntimeError, "no fresh evidence"):
                RUNNER.validate_evidence(evidence, row, time.time() + 1)


if __name__ == "__main__":
    unittest.main()
