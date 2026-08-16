#!/usr/bin/env python3
"""契約 co-change gate 自体の回帰テスト。"""

from __future__ import annotations

import pathlib
import subprocess
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "check-contract-cochange.py"


class ContractCochangeTests(unittest.TestCase):
    def make_repo(self) -> tuple[tempfile.TemporaryDirectory[str], pathlib.Path, str]:
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = pathlib.Path(temp.name)
        subprocess.run(["git", "init", "-q", "-b", "main", str(root)], check=True)
        subprocess.run(["git", "-C", str(root), "config", "user.name", "Contract Test"], check=True)
        subprocess.run(
            ["git", "-C", str(root), "config", "user.email", "contract@example.invalid"],
            check=True,
        )

        self.write(root, "docs/contract.md", "contract v1\n")
        self.write(root, "crates/kb-core/src/contract_guard.rs", "fingerprint v1\n")
        self.write(root, "crates/kb-core/src/tags.rs", "implementation v1\n")
        self.write(root, "README.md", "fixture\n")
        base = self.commit(root, "fixture")
        return temp, root, base

    def write(self, root: pathlib.Path, relative: str, content: str) -> None:
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    def commit(self, root: pathlib.Path, message: str) -> str:
        subprocess.run(["git", "-C", str(root), "add", "."], check=True)
        subprocess.run(["git", "-C", str(root), "commit", "-qm", message], check=True)
        return subprocess.check_output(
            ["git", "-C", str(root), "rev-parse", "HEAD"], text=True
        ).strip()

    def run_gate(self, root: pathlib.Path, base: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["python3", str(SCRIPT), base, "HEAD"],
            cwd=root,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
        )

    def test_unrelated_change_passes(self) -> None:
        _, root, base = self.make_repo()
        self.write(root, "README.md", "unrelated\n")
        self.commit(root, "unrelated")
        self.assertEqual(self.run_gate(root, base).returncode, 0)

    def test_document_only_change_fails(self) -> None:
        _, root, base = self.make_repo()
        self.write(root, "docs/contract.md", "contract v2\n")
        self.commit(root, "document only")
        result = self.run_gate(root, base)
        self.assertEqual(result.returncode, 1)
        self.assertIn("contract_guard.rs", result.stderr)
        self.assertIn("強制実装", result.stderr)

    def test_document_and_implementation_without_guard_fails(self) -> None:
        _, root, base = self.make_repo()
        self.write(root, "docs/contract.md", "contract v2\n")
        self.write(root, "crates/kb-core/src/tags.rs", "implementation v2\n")
        self.commit(root, "guard missing")
        result = self.run_gate(root, base)
        self.assertEqual(result.returncode, 1)
        self.assertIn("contract_guard.rs", result.stderr)

    def test_document_and_guard_without_implementation_fails(self) -> None:
        _, root, base = self.make_repo()
        self.write(root, "docs/contract.md", "contract v2\n")
        self.write(root, "crates/kb-core/src/contract_guard.rs", "fingerprint v2\n")
        self.commit(root, "implementation missing")
        result = self.run_gate(root, base)
        self.assertEqual(result.returncode, 1)
        self.assertIn("強制実装", result.stderr)

    def test_document_implementation_and_guard_in_one_commit_pass(self) -> None:
        _, root, base = self.make_repo()
        self.write(root, "docs/contract.md", "contract v2\n")
        self.write(root, "crates/kb-core/src/contract_guard.rs", "fingerprint v2\n")
        self.write(root, "crates/kb-core/src/tags.rs", "implementation v2\n")
        self.commit(root, "co-change")
        self.assertEqual(self.run_gate(root, base).returncode, 0)

    def test_later_commit_cannot_repair_document_only_commit(self) -> None:
        _, root, base = self.make_repo()
        self.write(root, "docs/contract.md", "contract v2\n")
        self.commit(root, "document first")
        self.write(root, "crates/kb-core/src/contract_guard.rs", "fingerprint v2\n")
        self.write(root, "crates/kb-core/src/tags.rs", "implementation v2\n")
        self.commit(root, "implementation later")
        result = self.run_gate(root, base)
        self.assertEqual(result.returncode, 1)


if __name__ == "__main__":
    unittest.main()
