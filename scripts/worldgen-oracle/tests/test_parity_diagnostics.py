import hashlib
import importlib.util
import tempfile
import unittest
from pathlib import Path


HERE = Path(__file__).parents[1]
SCRIPT = HERE / "parity-diagnostics.py"
SPEC = importlib.util.spec_from_file_location("parity_diagnostics", SCRIPT)
diagnostics = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(diagnostics)

MANIFEST_SPEC = importlib.util.spec_from_file_location(
    "large_parity_manifest", HERE / "large-parity-manifest.py"
)
manifest = importlib.util.module_from_spec(MANIFEST_SPEC)
assert MANIFEST_SPEC.loader is not None
MANIFEST_SPEC.loader.exec_module(manifest)


def write_v6(root):
    frozen = hashlib.sha256(b"diagnostic-world").digest()
    cx0 = cx1 = cz0 = cz1 = 0
    body = b"reference-packet"
    audit = manifest.raw_packet_full_digest(body)
    prefix = audit[:2]
    main = root / "one.lwp"
    main.write_bytes(
        manifest.make_header(
            6,
            "overworld",
            cx0,
            cx1,
            cz0,
            cz1,
            1,
            frozen,
            hashlib.sha256(prefix).digest(),
        )
        + prefix
    )
    header, _payload, dimension = manifest.read(main)
    manifest.packet_audit_path(main).write_bytes(
        manifest.make_packet_audit_header(header, dimension, hashlib.sha256(audit).digest()) + audit
    )
    return main, audit.hex()


def write_inventory(path, manifest_path, expected):
    path.write_text(
        "\n".join(
            [
                "schema=lodestone-large-parity-mismatch-v1",
                f"manifest_sha256={hashlib.sha256(manifest_path.read_bytes()).hexdigest()}",
                f"sidecar_sha256={hashlib.sha256(manifest.packet_audit_path(manifest_path).read_bytes()).hexdigest()}",
                "semantic_version=6",
                "dimension=overworld",
                f"frozen_world_sha256={manifest.read(manifest_path)[0][19].hex()}",
                "geometry=0..0:0..0",
                "record_width=2",
                "kind=raw-packet",
                "mismatch_count=1",
                "index\tcx\tcz\texpected_full_sha256\tactual_full_sha256",
                f"0\t0\t0\t{expected}\t{'00' * 32}",
                "",
            ]
        ),
        encoding="ascii",
    )


class ParityDiagnosticsTests(unittest.TestCase):
    def test_authenticated_inventory_writes_only_coordinates(self):
        with tempfile.TemporaryDirectory(prefix="parity-diagnostics-") as directory:
            root = Path(directory)
            manifest_path, expected = write_v6(root)
            inventory = root / "mismatches.tsv"
            coordinates = root / "coordinates.tsv"
            write_inventory(inventory, manifest_path, expected)
            frozen = root / "sealed"
            frozen.mkdir()
            frozen.joinpath("lodestone-large-parity-materialization-v6-overworld.freeze.sha256").write_text(
                manifest.read(manifest_path)[0][19].hex() + "\n", encoding="ascii"
            )
            _fields, _geometry, rows = diagnostics.authenticated_rows(
                manifest_path, inventory, coordinates, frozen_root=frozen
            )
            self.assertEqual(len(rows), 1)
            self.assertEqual(coordinates.read_text(encoding="ascii"), "index\tcx\tcz\n0\t0\t0\n")

    def test_packet_selection_is_bounded_to_authenticated_rows(self):
        with tempfile.TemporaryDirectory(prefix="parity-diagnostics-") as directory:
            root = Path(directory)
            manifest_path, expected = write_v6(root)
            inventory = root / "mismatches.tsv"
            coordinates = root / "coordinates.tsv"
            source = root / "all-packets"
            selected = root / "mismatch-packets"
            source.mkdir()
            actual = b"lodestone-actual-packet"
            (source / "x0_z0.packet").write_bytes(actual)
            write_inventory(inventory, manifest_path, expected)
            text = inventory.read_text(encoding="ascii").replace("00" * 32, hashlib.sha256(actual).hexdigest())
            inventory.write_text(text, encoding="ascii")
            diagnostics.authenticated_rows(
                manifest_path, inventory, coordinates, source, selected
            )
            self.assertEqual((selected / "x0_z0.packet").read_bytes(), actual)

    def test_manifest_identity_tampering_is_rejected(self):
        with tempfile.TemporaryDirectory(prefix="parity-diagnostics-") as directory:
            root = Path(directory)
            manifest_path, expected = write_v6(root)
            inventory = root / "mismatches.tsv"
            coordinates = root / "coordinates.tsv"
            write_inventory(inventory, manifest_path, expected)
            manifest_path.write_bytes(manifest_path.read_bytes()[:-1] + b"\x01")
            with self.assertRaisesRegex(ValueError, "checksum"):
                diagnostics.authenticated_rows(manifest_path, inventory, coordinates)

    def test_sidecar_identity_tampering_is_rejected(self):
        with tempfile.TemporaryDirectory(prefix="parity-diagnostics-") as directory:
            root = Path(directory)
            manifest_path, expected = write_v6(root)
            inventory = root / "mismatches.tsv"
            coordinates = root / "coordinates.tsv"
            write_inventory(inventory, manifest_path, expected)
            sidecar = manifest.packet_audit_path(manifest_path)
            sidecar.write_bytes(sidecar.read_bytes()[:-1] + b"\x01")
            with self.assertRaisesRegex(ValueError, "checksum"):
                diagnostics.authenticated_rows(manifest_path, inventory, coordinates)

    def test_sealed_root_identity_tampering_is_rejected(self):
        with tempfile.TemporaryDirectory(prefix="parity-diagnostics-") as directory:
            root = Path(directory)
            manifest_path, expected = write_v6(root)
            inventory = root / "mismatches.tsv"
            coordinates = root / "coordinates.tsv"
            frozen = root / "sealed"
            frozen.mkdir()
            frozen.joinpath("lodestone-large-parity-materialization-v6-overworld.freeze.sha256").write_text(
                "00" * 32 + "\n", encoding="ascii"
            )
            write_inventory(inventory, manifest_path, expected)
            with self.assertRaisesRegex(ValueError, "sealed root identity"):
                diagnostics.authenticated_rows(
                    manifest_path, inventory, coordinates, frozen_root=frozen
                )


if __name__ == "__main__":
    unittest.main()
