#!/usr/bin/env python3
"""契約変更を同じコミットのコア実装・契約テスト更新へ結び付ける。"""

from __future__ import annotations

import pathlib
import subprocess
import sys


CONTRACT_DOCUMENT = "docs/contract.md"
CONTRACT_GUARD = "crates/kb-core/src/contract_guard.rs"
CORE_SOURCE_PREFIX = "crates/kb-core/src/"


def git(root: pathlib.Path, *args: str) -> bytes:
    result = subprocess.run(
        ["git", "-C", str(root), *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        message = result.stderr.decode("utf-8", "replace").strip()
        raise RuntimeError(message or f"git {' '.join(args)} failed")
    return result.stdout


def git_text(root: pathlib.Path, *args: str) -> str:
    return git(root, *args).decode("utf-8", "replace").strip()


def changed_paths(root: pathlib.Path, commit: str) -> set[str]:
    ancestry = git_text(root, "rev-list", "--parents", "-n", "1", commit).split()
    if not ancestry:
        raise RuntimeError(f"コミットを解決できない: {commit}")

    if len(ancestry) == 1:
        output = git(root, "diff-tree", "--root", "--no-commit-id", "--name-only", "-r", "-z", commit)
    else:
        # merge commit は第一親との差を検査する。branch 内の各 commit も別に検査するため、
        # PR 全体で帳尻を合わせても個々の契約変更 commit は免除されない。
        output = git(root, "diff", "--name-only", "-z", ancestry[1], commit)
    return {
        path.decode("utf-8", "surrogateescape")
        for path in output.split(b"\0")
        if path
    }


def violations(commit: str, paths: set[str]) -> list[str]:
    if CONTRACT_DOCUMENT not in paths:
        return []

    missing: list[str] = []
    if CONTRACT_GUARD not in paths:
        missing.append(CONTRACT_GUARD)

    implementation_changed = any(
        path.startswith(CORE_SOURCE_PREFIX) and path != CONTRACT_GUARD for path in paths
    )
    if not implementation_changed:
        missing.append(f"{CORE_SOURCE_PREFIX} 配下の強制実装（{CONTRACT_GUARD} 以外）")

    if not missing:
        return []
    short = commit[:12]
    return [
        f"{short}: {CONTRACT_DOCUMENT} を変更した同じコミットに "
        f"{', '.join(missing)} の変更が必要"
    ]


def check_range(root: pathlib.Path, base: str, head: str) -> list[str]:
    git(root, "rev-parse", "--verify", f"{base}^{{commit}}")
    git(root, "rev-parse", "--verify", f"{head}^{{commit}}")
    commits = git_text(root, "rev-list", "--reverse", f"{base}..{head}").splitlines()

    errors: list[str] = []
    for commit in commits:
        errors.extend(violations(commit, changed_paths(root, commit)))
    return errors


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print("usage: check-contract-cochange.py BASE HEAD", file=sys.stderr)
        return 2

    try:
        root = pathlib.Path(git_text(pathlib.Path.cwd(), "rev-parse", "--show-toplevel"))
        errors = check_range(root, argv[1], argv[2])
    except RuntimeError as error:
        print(f"契約 co-change gate を実行できない: {error}", file=sys.stderr)
        return 2

    if errors:
        print("契約文書だけの変更を検出しました:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        print(
            "docs/contract.md、対応する kb-core 実装、contract_guard.rs の fingerprint を "
            "同じコミットで更新してください。",
            file=sys.stderr,
        )
        return 1

    print("契約 co-change gate: OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
