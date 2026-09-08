import hashlib
import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).parents[1] / "large-parity-manifest.py"
SPEC = importlib.util.spec_from_file_location("large_parity_manifest", SCRIPT)
manifest = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(manifest)


def write_shard(path, version, sx0, sx1, sz0, sz1, frozen, salt):
    width = manifest.RAW_WIDTH if version == 6 else manifest.WIDTH
    records = []
    for cz in range(sz0, sz1 + 1):
        for cx in range(sx0, sx1 + 1):
            seed = f"{salt}:{cx}:{cz}".encode()
            digest = hashlib.sha256(seed).digest()
            records.append((digest * ((width + len(digest) - 1) // len(digest)))[:width])
    payload = b"".join(records)
    path.write_bytes(manifest.make_header(
        version, "overworld" if version == 3 else "nether",
        sx0, sx1, sz0, sz1, len(records), frozen,
        hashlib.sha256(payload).digest(),
    ) + payload)


def reference_merge(paths):
    """The former dictionary implementation, used only as a test oracle."""
    slots = {}
    frozen = None
    version = None
    dimension = None
    width = None
    for path in paths:
        header, payload, shard_dimension = manifest.read(path)
        if version is None:
            version, dimension, width = header[1], shard_dimension, header[16]
            grid_min, grid_max = (
                (manifest.RAW_GRID_MIN, manifest.RAW_GRID_MAX)
                if version == 6 else (manifest.GRID_MIN, manifest.GRID_MAX)
            )
        if frozen is None:
            frozen = header[19]
        for index, (cz, cx) in enumerate(
            ( (cz, cx)
              for cz in range(header[13], header[14] + 1)
              for cx in range(header[11], header[12] + 1) )
        ):
            slots[(cx, cz)] = payload[index * width:(index + 1) * width]
    payload = b"".join(
        slots[(cx, cz)]
        for cz in range(grid_min, grid_max + 1)
        for cx in range(grid_min, grid_max + 1)
    )
    return manifest.make_header(
        version, dimension, grid_min, grid_max, grid_min, grid_max,
        len(slots), frozen, hashlib.sha256(payload).digest(),
    ) + payload


class LargeParityManifestTests(unittest.TestCase):
    def test_old_sized_fixture_is_byte_identical_and_order_independent(self):
        with tempfile.TemporaryDirectory(prefix="large-parity-test-") as directory:
            root = Path(directory)
            frozen = hashlib.sha256(b"fixture-world").digest()
            middle = 0
            left = root / "left.lwp"
            right = root / "right.lwp"
            write_shard(left, 3, manifest.GRID_MIN, middle, manifest.GRID_MIN,
                        manifest.GRID_MAX, frozen, "left")
            write_shard(right, 3, middle + 1, manifest.GRID_MAX, manifest.GRID_MIN,
                        manifest.GRID_MAX, frozen, "right")
            expected = reference_merge([right, left])
            output = root / "merged.lwp"
            manifest.merge(output, [right, left])
            self.assertEqual(output.read_bytes(), expected)

    def test_overlap_and_gap_controls_fail_closed(self):
        with tempfile.TemporaryDirectory(prefix="large-parity-test-") as directory:
            root = Path(directory)
            frozen = hashlib.sha256(b"control-world").digest()
            left = root / "left.lwp"
            right = root / "right.lwp"
            write_shard(left, 6, manifest.RAW_GRID_MIN, 0, manifest.RAW_GRID_MIN,
                        manifest.RAW_GRID_MAX, frozen, "left")
            write_shard(right, 6, 1, manifest.RAW_GRID_MAX, manifest.RAW_GRID_MIN,
                        manifest.RAW_GRID_MAX, frozen, "right")
            with self.assertRaisesRegex(ValueError, "overlap"):
                manifest.merge(root / "overlap.lwp", [left, right, left])
            with self.assertRaisesRegex(ValueError, "incomplete merge"):
                manifest.merge(root / "gap.lwp", [left])

    def test_large_p06_fixture_preserves_x_fastest_z_order(self):
        with tempfile.TemporaryDirectory(prefix="large-parity-test-") as directory:
            root = Path(directory)
            frozen = hashlib.sha256(b"p06-world").digest()
            left = root / "left.lwp"
            right = root / "right.lwp"
            write_shard(left, 6, manifest.RAW_GRID_MIN, 0, manifest.RAW_GRID_MIN,
                        manifest.RAW_GRID_MAX, frozen, "left")
            write_shard(right, 6, 1, manifest.RAW_GRID_MAX, manifest.RAW_GRID_MIN,
                        manifest.RAW_GRID_MAX, frozen, "right")
            output = root / "merged.lwp"
            manifest.merge(output, [right, left])
            header, payload, _ = manifest.read(output)
            self.assertEqual(header[15], manifest.RAW_GRID_COUNT)
            first = hashlib.sha256(f"left:{manifest.RAW_GRID_MIN}:{manifest.RAW_GRID_MIN}".encode()).digest()[:2]
            next_x = hashlib.sha256(f"left:{manifest.RAW_GRID_MIN + 1}:{manifest.RAW_GRID_MIN}".encode()).digest()[:2]
            next_z = hashlib.sha256(f"left:{manifest.RAW_GRID_MIN}:{manifest.RAW_GRID_MIN + 1}".encode()).digest()[:2]
            self.assertEqual(payload[:2], first)
            self.assertEqual(payload[2:4], next_x)
            self.assertEqual(payload[manifest.RAW_GRID_SIDE * 2:manifest.RAW_GRID_SIDE * 2 + 2], next_z)


if __name__ == "__main__":
    unittest.main()
