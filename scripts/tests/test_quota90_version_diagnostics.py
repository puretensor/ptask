"""Version mismatches must fail with their intended actionable diagnostic."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "ci-version-check.sh"


class VersionDiagnostics(unittest.TestCase):
    def test_zero_matching_dependency_versions_explains_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "scripts").mkdir()
            shutil.copy2(SCRIPT, root / "scripts" / SCRIPT.name)
            (root / "bin").mkdir()
            cargo = root / "bin" / "cargo"
            cargo.write_text("#!/bin/sh\nexit 0\n")
            cargo.chmod(0o700)
            deps = "\n".join(f'ptask-{name} = {{ path = "unused", version = "1.0.0" }}' for name in ["core", "tui", "server", "bot", "distill", "notify"])
            (root / "Cargo.toml").write_text('[workspace.package]\nversion = "2.0.0"\n[workspace.dependencies]\n' + deps + '\n')
            env = dict(os.environ, PATH=str(root / "bin") + os.pathsep + os.environ["PATH"])
            result = subprocess.run(["bash", str(root / "scripts" / SCRIPT.name)], env=env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("internal dependency requirements do not all match 2.0.0", result.stderr)


if __name__ == "__main__":
    unittest.main()
