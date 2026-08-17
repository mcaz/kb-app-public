#!/usr/bin/env python3
"""旧UserPromptSubmit配線の互換ラッパー。

KBを直接検索せず、kb-app本体のMCP自動retrievalモードへ入力を転送する。
新規導入では管理ポリシーのhookが本体を直接呼ぶため、このファイルは使わない。
"""

import os
import subprocess
import sys

APP_BIN = os.environ.get("KB_APP_BIN") or "/Applications/kb-app.app/Contents/MacOS/kb-app"


def main() -> None:
    try:
        completed = subprocess.run(
            [
                APP_BIN,
                "--hook-auto-retrieve",
                "--client",
                "claude-code/claude",
            ],
            input=sys.stdin.read(),
            capture_output=True,
            text=True,
            timeout=30,
        )
    except Exception as error:
        print(f"kb-app auto retrieval compatibility hook failed: {error}", file=sys.stderr)
        return
    if completed.stdout:
        print(completed.stdout, end="")
    if completed.stderr:
        print(completed.stderr, end="", file=sys.stderr)


if __name__ == "__main__":
    main()
