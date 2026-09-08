#!/usr/bin/env python3
"""新規Claude会話の開始をIDへ結び付ける。設定・台帳・会話本文は読まない。"""

import argparse
import os
import sys
import time
import uuid


def parser():
    result = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    result.add_argument("--model", help="省略時はClaudeの既存モデル設定を使う")
    result.add_argument("--name", help="新しい会話の表示名")
    result.add_argument("--diagnostic", action="store_true", help="通常利用の集計から除く")
    result.add_argument("prompt", nargs="?", help="新しい会話の最初の発話（省略可）")
    return result


def build_launch(options, inherited_env, session_id, started_at_ms):
    # resume等の任意オプションを転送すると、新規IDでも過去のON文脈を持ち込める。
    uuid.UUID(session_id)
    if started_at_ms < 0:
        raise ValueError("開始時刻が不正")
    command = ["claude", "--session-id", session_id]
    if options.model is not None:
        command.extend(["--model", options.model])
    if options.name is not None:
        command.extend(["--name", options.name])
    if options.prompt is not None:
        command.extend(["--", options.prompt])
    environment = dict(inherited_env)
    environment["KB_APP_OBSERVATION_PURPOSE"] = (
        "diagnostic" if options.diagnostic
        else inherited_env.get("KB_APP_OBSERVATION_PURPOSE", "normal")
    )
    environment["KB_APP_OBSERVATION_SESSION_ID"] = session_id
    environment["KB_APP_OBSERVATION_SESSION_STARTED_AT_MS"] = str(started_at_ms)
    # CLAUDE_CODE_SESSION_IDはhostが渡す証拠。ここでは補完せずcoreで一致を検査する。
    return command, environment


def main(argv=None):
    options = parser().parse_args(argv)
    command, environment = build_launch(
        options, os.environ, str(uuid.uuid4()), time.time_ns() // 1_000_000
    )
    try:
        os.execvpe(command[0], command, environment)
    except FileNotFoundError:
        print("claudeがPATHに見つかりません。Claude Codeを利用できるTerminalで実行してください。", file=sys.stderr)
        return 127
    except OSError:
        print("Claude Codeを起動できませんでした。", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
