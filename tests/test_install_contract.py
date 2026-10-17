import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("verify_install", Path(__file__).parents[1] / "scripts/verify-install.py")
verify_install = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify_install)


class InstallContract(unittest.TestCase):
    def test_missing_assets_fail_the_gate(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "bin").mkdir()
            (root / "bin/rustle").write_text("placeholder")
            with self.assertRaisesRegex(AssertionError, "missing installed file"):
                verify_install.verify(root)
