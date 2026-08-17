#!/usr/bin/env python3
"""Claude Code Stop フック — kb-app 安全網(検索忘れの差し戻し)。

前出しフックが「このセッションに関連ノートがある」と記録していたのに、
セッション中で一度も kb-app を引いていない場合、終了をブロックして
「最後に search で確認してから終えること」を差し戻す。

- 発火は厳しめの条件のみ(関連あり AND kb-app 完全未使用)— 鳴りすぎは信号を殺す
- stop_hook_active(差し戻し後の再終了)では絶対にブロックしない(無限ループ防止)
- fail-open: 判定に失敗したら黙って通す(会話を止めない)
"""

import json
import os
import shutil
import subprocess
import sys

STATE_DIR = os.path.expanduser("~/.claude/state/kb-app")
INSTALLED_KB_BIN = os.path.expanduser("~/Library/Application Support/kb-app/bin/kb")
KB_BIN = os.environ.get("KB_BIN") or shutil.which("kb") or (
    INSTALLED_KB_BIN if os.path.exists(INSTALLED_KB_BIN)
    else "/path/to/kb-app/target/release/kb"
)
CLIENT = "claude-code/claude"


def kb_enabled() -> bool:
    """OFF 時は過去の relevant フラグが残っていても終了を差し戻さない。"""
    try:
        out = subprocess.run(
            [KB_BIN, "settings", "ai-enabled", "--client", CLIENT],
            capture_output=True,
            text=True,
            timeout=3,
        )
        return out.returncode == 0 and json.loads(out.stdout).get("enabled") is True
    except Exception:
        return False


def main() -> None:
    try:
        payload = json.load(sys.stdin)
    except Exception:
        return
    if not kb_enabled():
        return
    if payload.get("stop_hook_active"):
        return  # 差し戻し後の終了は必ず通す
    session_id = str(payload.get("session_id") or "")
    if not session_id:
        return
    flag = os.path.join(STATE_DIR, f"{session_id}.relevant")
    if not os.path.exists(flag):
        return  # このセッションに KB 関連の前出しは無かった
    transcript = payload.get("transcript_path") or ""
    try:
        with open(transcript, "r", errors="ignore") as f:
            used = any("mcp__kb-app__" in line for line in f)
    except Exception:
        return
    if used:
        try:
            os.remove(flag)
        except Exception:
            pass
        return
    # 関連ノートが前出しされたのに一度も引いていない → 一度だけ差し戻す
    print(
        "[kb-app 安全網] このセッションでは関連ノートが前出しされていたが、kb-app を一度も"
        "引いていない。終了前に結論の主題語で search を1回引き、結果が結論に影響するなら"
        "反映すること。確認の結果 無関係なら、その旨を一言添えて終了してよい。",
        file=sys.stderr,
    )
    sys.exit(2)


if __name__ == "__main__":
    main()
