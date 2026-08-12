#!/usr/bin/env bash
# LFS 転送の実測ハーネス — ADR-0003 の「乗り換え条件」を判定する。
#
# ADR-0003 は full 転送に同一 origin の Git LFS を採ると決めているが、
# 次のいずれかが倒れたら第2リポジトリ方式へ切り替える、と条件を付けている:
#
#   1. 手で普通に clone したときに実体が勝手に落ちてこないこと
#   2. 同梱 git-lfs が既存の GitHub 認証を再利用できること
#   3. 枠の超過・停止をアプリが説明・分類して出せること
#
# ここはその判定材料を採る場所。推測ではなく実測で決める。
#
# 使い方:
#   ./run.sh                      # ネットワーク不要の項目だけ
#   ./run.sh --remote <git-url>   # 全項目(空のリポジトリを1つ用意すること)
#
# 注意: --remote を付けると LFS の保管容量と通信量を消費する。

set -euo pipefail

REMOTE=""
if [[ "${1:-}" == "--remote" ]]; then
  REMOTE="${2:-}"
  [[ -n "$REMOTE" ]] || { echo "--remote には URL が要る"; exit 2; }
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
RESULTS=()

pass() { RESULTS+=("PASS|$1|$2"); echo "  PASS  $1 — $2"; }
fail() { RESULTS+=("FAIL|$1|$2"); echo "  FAIL  $1 — $2"; }
skip() { RESULTS+=("SKIP|$1|$2"); echo "  SKIP  $1 — $2"; }

sha256() { shasum -a 256 "$1" | awk '{print $1}'; }

echo "== 前提 =="
if ! command -v git-lfs >/dev/null 2>&1; then
  echo "git-lfs が無い。'brew install git-lfs' のあと再実行すること。"
  exit 1
fi
echo "  git      $(git --version | awk '{print $3}')"
echo "  git-lfs  $(git lfs version | awk '{print $1}' | cut -d/ -f2)"

# ───────────────────────────────────────────────────────────────
# 1. 照合値が LFS の OID と一致するか(ネットワーク不要)
#
# ADR-0003 決定2 は content_hash を raw bytes の SHA-256 に固定し、
# 「LFS の OID と一致するので検証が二重にならない」を根拠にしている。
# ここが崩れると、その根拠ごと崩れる。
# ───────────────────────────────────────────────────────────────
echo
echo "== 1. content_hash == LFS の OID =="
REPO="$WORK/oid"
git init -q "$REPO"
cd "$REPO"
git lfs install --local >/dev/null 2>&1
# 置き場の指定は「最初の実体を作る前」でなければ効かない(下の項目2で実測)
SIDECAR="$WORK/sidecar"
mkdir -p "$SIDECAR"
git config lfs.storage "$SIDECAR"
git lfs track "*.bin" >/dev/null
head -c 3000000 /dev/urandom > blob.bin
WANT="$(sha256 blob.bin)"
git add .gitattributes blob.bin
POINTER="$(git cat-file -p :blob.bin)"
GOT="$(echo "$POINTER" | awk -F'sha256:' '/^oid/{print $2}')"
if [[ "$WANT" == "$GOT" ]]; then
  pass "OID 一致" "sha256=${WANT:0:16}…"
else
  fail "OID 一致" "期待 ${WANT:0:16}… / 実際 ${GOT:0:16}…"
fi

POINTER_BYTES="$(echo "$POINTER" | wc -c | tr -d ' ')"
if (( POINTER_BYTES < 1024 )); then
  pass "保管庫に入るのは印だけ" "${POINTER_BYTES} バイト"
else
  fail "保管庫に入るのは印だけ" "${POINTER_BYTES} バイト"
fi

# ───────────────────────────────────────────────────────────────
# 2. 実体の置き場を保管庫の外へ移せるか(ネットワーク不要)
#
# 決定2 は「LFS の local storage は既定の .git/lfs ではなく repo 外 sidecar」と
# している。lfs.storage が効かないと、実体が保管庫の中に戻ってしまう。
# ───────────────────────────────────────────────────────────────
echo
echo "== 2. 実体の置き場を保管庫の外へ =="
git -c user.email=poc@localhost -c user.name=poc commit -qm "blob" >/dev/null
INSIDE="$(find .git/lfs -type f 2>/dev/null | wc -l | tr -d ' ')"
OUTSIDE="$(find "$SIDECAR" -type f 2>/dev/null | wc -l | tr -d ' ')"
if (( OUTSIDE > 0 && INSIDE == 0 )); then
  pass "lfs.storage が効く" "外 ${OUTSIDE} 件 / 保管庫内 ${INSIDE} 件"
else
  fail "lfs.storage が効く" "外 ${OUTSIDE} 件 / 保管庫内 ${INSIDE} 件"
fi

# 順序の罠: 実体を作った後に設定しても、既にある実体は移らない。
# 製品では「保管庫を作る/clone した直後」に張る必要がある
LATE="$WORK/late"
git init -q "$LATE"
(
  cd "$LATE"
  git lfs install --local >/dev/null 2>&1
  git lfs track "*.bin" >/dev/null
  head -c 1000000 /dev/urandom > late.bin
  git add .gitattributes late.bin
  mkdir -p "$WORK/late-side"
  git config lfs.storage "$WORK/late-side" # ← 後から設定
)
LATE_INSIDE="$(find "$LATE/.git/lfs" -type f 2>/dev/null | wc -l | tr -d ' ')"
if (( LATE_INSIDE > 0 )); then
  pass "後から設定しても移らない(順序の罠)" "保管庫内に ${LATE_INSIDE} 件が残る"
else
  fail "後から設定しても移らない(順序の罠)" "想定と違う挙動"
fi

# ───────────────────────────────────────────────────────────────
# 3〜5 は本物の remote が要る
# ───────────────────────────────────────────────────────────────
MODE="remote"
if [[ -z "$REMOTE" ]]; then
  # 実体の取得・除外は転送方式によらず同じ経路(smudge と fetch 設定)を通るので、
  # ローカルの bare リポジトリで代用できる。認証だけは代用できない
  MODE="local"
  REMOTE="$WORK/origin.git"
  # 既定ブランチを揃えないと clone 時に HEAD が解決できない
  git init -q --bare -b main "$REMOTE"
  echo
  echo "(remote 未指定 — ローカルの bare で代用する。認証の項目だけ測れない)"
fi

  echo
  echo "== 3. push =="
  cat > .lfsconfig <<'EOF'
[lfs]
	fetchexclude = *
EOF
  git add .lfsconfig
  git commit -qm "lfs: 既定では実体を取らない"
  git remote add origin "$REMOTE"
  git branch -M main
  if GIT_TERMINAL_PROMPT=0 git push -q -u origin main 2>"$WORK/push.err"; then
    if [[ "$MODE" == "remote" ]]; then
      pass "既存の認証を対話なしで使えるか" "push 成功(資格情報の入力なし)"
    else
      skip "既存の認証を対話なしで使えるか" "ローカル代用では測れない"
    fi
  else
    fail "既存の認証を対話なしで使えるか" "$(tail -2 "$WORK/push.err" | tr '\n' ' ')"
  fi

  echo
  echo "== 4. 手で普通に clone したとき =="
  # ここが乗り換え条件の本丸。ユーザーが素の git clone をしたときに
  # 実体まで落ちてくるなら、保管庫の外に置いた意味が薄れる
  PLAIN="$WORK/plain"
  GIT_TERMINAL_PROMPT=0 git clone -q "$REMOTE" "$PLAIN"
  if head -c 40 "$PLAIN/blob.bin" | grep -q "version https://git-lfs"; then
    pass "手動 clone で実体が落ちてこないか" ".lfsconfig の除外が効き、印のまま"
  else
    SIZE="$(wc -c < "$PLAIN/blob.bin" | tr -d ' ')"
    fail "手動 clone で実体が落ちてこないか" "実体が落ちてきた(${SIZE} バイト)"
  fi

  echo
  echo "== 5. 必要な実体だけ後から取る =="
  cd "$PLAIN"
  # 素の clone には LFS のフィルタが張られていない。アプリ側で有効化する
  git lfs install --local >/dev/null 2>&1
  git config lfs.storage "$WORK/sidecar2"
  # -X "" が要る。.lfsconfig の fetchexclude=* は -I だけでは上書きされない
  if GIT_TERMINAL_PROMPT=0 git lfs pull -I "blob.bin" -X "" >/dev/null 2>&1; then
    GOT2="$(sha256 blob.bin)"
    if [[ "$GOT2" == "$WANT" ]]; then
      pass "必要な実体だけ後から取れるか" "取得後に照合一致"
    else
      fail "必要な実体だけ後から取れるか" "照合不一致"
    fi
  else
    fail "必要な実体だけ後から取れるか" "git lfs pull が失敗"
  fi

echo
echo "== まとめ =="
printf '%-6s %-32s %s\n' "結果" "項目" "実測"
for r in "${RESULTS[@]}"; do
  IFS='|' read -r st name note <<< "$r"
  printf '%-6s %-32s %s\n' "$st" "$name" "$note"
done

if printf '%s\n' "${RESULTS[@]}" | grep -q '^FAIL'; then
  echo
  echo "FAIL がある。ADR-0003 の乗り換え条件に照らして、第2リポジトリ方式への切り替えを検討すること。"
  exit 1
fi
