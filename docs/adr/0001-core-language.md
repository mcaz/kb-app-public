# ADR-0001: コアは Rust、UI は TypeScript

- 状態: **採用**(2026-08-09 本人決定)
- 関連: [requirements.md](../requirements.md) 未決「コアの実装言語」/ [okf-conformance.md](../okf-conformance.md)

> 旧 KB の当初実装は設計判断の記録が消失し「なぜこの構成か」を後から引けなくなった
> (個人 vault: missing-adr-recovery)。kb-app は同じ轍を踏まない — 設計判断はこの
> ADR 系列に必ず残す。これが第1号。

## 文脈

- コア(ノートストア+索引+ハイブリッド検索+ライフサイクル API+MCP サーバー)は
  ヘッドレスで完結し、GUI・CLI・AI ツールから呼ばれる(requirements システム構成)
- 外殻は Tauri で決定済み(NFR-M 系の経緯)。**非エンジニアの単体インストール**
  (NFR-2: 3歩セットアップ)が製品の存在理由
- 参照実装は Python(旧 KB: SQLite FTS5+sqlite-vec+RRF+bge-m3。閾値・golden queries
  まで較正済み)
- 実測障害: 埋め込みを ollama 外部プロセスに依存した結果、**ベクトル検索が4日間
  沈黙停止**した記録がある(旧 KB の運用実測)。埋め込みのアプリ内蔵化(ONNX)は
  この障害クラスを構造的に消す

## 決定

1. **コア = Rust の単一 crate(kb-core)**。Tauri アプリへ直接リンクし、同じコアから
   CLI バイナリ(`kb`。`kb mcp` で MCP サーバー起動)を出す
2. **UI = Tauri v2 + TypeScript**(エディタは CodeMirror 6 系を想定。UI は使い捨ての
   外殻 — 原則6)
3. **Python 参照実装は照合オラクルとして使う**: 同じ golden queries を新旧両実装に流し、
   検索品質の同等性を検証しながら移植する(資産は捨てず、依存もしない)

## 検討した代替案

| 案 | 内容 | 判定 |
|---|---|---|
| A | Python コア+Tauri sidecar(PyInstaller 等でランタイム同梱) | **棄却**。資産流用は最速だが、ランタイム同梱・署名・公証が恒常的な壊れどころになり、非エンジニア配布という存在理由と衝突 |
| B | TypeScript/Node コア | **棄却**。MCP SDK の成熟は最良だが、Node ランタイム同梱問題が A と同型 |
| C | Rust コア+TS UI | **採用**。Tauri と同一言語で1バイナリ・ランタイム依存ゼロ。ONNX(ort)・SQLite(rusqlite)・git(git2)・キーチェーン(keyring)・MCP(公式 rmcp)と部品が揃う |

## スタックの方向(PoC で確定させる)

| 層 | 第一候補 | 備考 |
|---|---|---|
| 埋め込み | ort(ONNX Runtime)+ **bge-m3 int8 同梱** | モデル据え置きなら旧 KB の較正資産(距離閾値 0.95/0.85・golden queries・関連/無関係の距離帯実測)がそのまま移る。サイズ超過時は multilingual-e5-small へ後退 |
| 索引 | SQLite 1ファイル+FTS5+sqlite-vec | ハイブリッド検索の優位は旧 KB で実測済み |
| 日本語 FTS | lindera(形態素)vs trigram+2文字語補助 | 旧 KB の「CJK 2文字語が空振り」欠陥を設計段階で殺す。PoC で比較 |
| MCP | 公式 Rust SDK(rmcp)、stdio | 将来 Streamable HTTP でリモート化ステージへ接続 |
| Git | git2(libgit2) | システム git 非依存 |
| GitHub 認証 | OAuth デバイスフロー+OS キーチェーン(keyring) | トークン平文保存の事故クラスを機構で排除。gh CLI 依存は不採用 |

## 判定条件(kill criteria)

着手 PoC 3本。**いずれかが落ちたら案 A(sidecar)を再評価する**:

1. **埋め込み内蔵**: ort+bge-m3 int8 が品質(golden queries で旧実装同等)・速度
   (実用レイテンシ)・サイズ(「数百MB」要件)を満たすか
2. **日本語トークナイズ**: 2文字語 golden queries で lindera / trigram+補助 を比較し、
   旧欠陥が再発しない方式を確定できるか
3. **索引 DB**: FTS5+sqlite-vec の1DB 同居、busy_timeout・fail-open(複数クライアント
   同時アクセス)が成立するか

## 設計に最初から組み込む(旧 KB で後から踏んだ穴5点)

1. CJK 2文字語の FTS 空振り(トークナイズ設計で回避)
2. 埋め込みの複合バージョニング(`書式版|provider:model:dim` +チャンク規則版)
3. DB ロック時の fail-open(busy_timeout・全ツールが sync 成功に依存しない)
4. 関連判定は RRF でなく生ベクトル距離(RRF はランク支配で分離不能 — 実測済み)
5. 増分 sync による鮮度保証(書いてすぐ検索に反映。変更なし時 ~0.06 秒の実績)
