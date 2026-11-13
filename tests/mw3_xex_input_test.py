"""Input gate regression checks using synthetic data only."""
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "check_mw3_xex", Path(__file__).resolve().parents[1] / "tools/check_mw3_xex.py")
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)


class XexInputTest(unittest.TestCase):
    def test_input_gate(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "input.xex"
            with self.assertRaises(OSError):
                checker.verify(path)
            path.write_bytes(b"version https://git-lfs.github.com/spec/v1\n")
            with self.assertRaisesRegex(ValueError, "LFS pointer"):
                checker.verify(path)
            path.write_bytes(b"not an executable")
            with self.assertRaisesRegex(ValueError, "XEX2 header"):
                checker.verify(path)
            # Cross the streaming chunk boundary to check full-file hashing.
            data = b"XEX2" + b"synthetic" * 140000
            path.write_bytes(data)
            expected = hashlib.sha256(data).hexdigest()
            checker.verify(path, expected)
            with self.assertRaisesRegex(ValueError, "Unsupported"):
                checker.verify(path)
            with path.open("ab") as stream:
                stream.write(b"changed")
            with self.assertRaisesRegex(ValueError, "Unsupported"):
                checker.verify(path, expected)


if __name__ == "__main__":
    unittest.main()
