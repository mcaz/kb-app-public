# ADR-0006: AI の生ファイルアクセスを管理 OS sandbox で閉じる

- 日付: 2026-08-17
- 状態: 採用
- 関連: [contract.md](../contract.md) 契約8 / [requirements.md](../requirements.md) FR-A9

## 背景

MCP の `tools/list` と `tools/call` を OFF にしても、Codex と Claude Code は汎用 shell と
組み込み file tool を持つ。同じ OS user で動くため、instructions に「直読みしない」と書くだけでは
比較条件を機構で確定できない。設定ファイルだけを閉じても Vault の Markdown は読め、Vault だけを
閉じても設定を ON に書き換えられる。

## 決定

生ファイルの拒否は ON/OFF と分離して常時有効にする。ON/OFF が変えるのは kb-app MCP と
Claude Code の信頼済み lifecycle hook だけで、Vault の OS sandbox deny は緩めない。

- Codex 0.138.0 以降: `/etc/codex/requirements.toml` に管理 custom permission profile を定義し、
  `allowed_permission_profiles` を kb-app の read-only / workspace profile だけに限定する。両 profile は
  `:read-only` / `:workspace` を継承し、保護 path を `deny` にする。`:danger-full-access` は選択肢に
  含めない。管理 `permissions.filesystem.deny_read` も同じ path へ重ね、full-access 指定を
  requirements の段階で拒否する。legacy sandbox mode も read-only / workspace-write だけに限定する。
- Claude Code: system-level `managed-settings.d` に `sandbox.enabled`、
  `failIfUnavailable`、`allowUnsandboxedCommands: false`、`allowManagedReadPathsOnly` を置く。
  `denyRead` / `denyWrite` は shell と子 process に、管理 `permissions.deny` は組み込み Read / Edit に
  適用する。`bypassPermissions` も管理設定で無効にする。
- 拒否対象は登録 Vault、各 canonical path、既定の `~/kb`、kb-app の端末設定 directory とする。
- ポリシー内容を毎回再生成して完全一致で検査する。未導入・登録 Vault 追加による古さ・競合・
  非対応のどれでも MCP は fail-closed。policy file から root までの所有者と mode も検査する。
  UI も保護完了まで switch を操作させない。
- macOS は設定 Modal から AppleScript の標準管理者認証を出し、root 管理領域へ固定ファイルを置く。
  既存の Codex requirements が kb-app 所有でなければ上書きせず conflict とする。Claude Code は
  公式の drop-in directory に kb-app 専用ファイルを置き、他の管理設定と分離する。

## 理由

Codex の permission profile は macOS Seatbelt で、spawn した command にも同じ filesystem deny を
継承する。管理 requirements は user config や CLI override で解除できない。Claude Code の sandbox
も macOS Seatbelt / Linux bubblewrap を使い、管理 settings は user / project / CLI より優先される。
したがって `cat`、Python、別名 CLI、子 process の選択に依存せず、能力の不在として強制できる。

- Codex Permissions: https://learn.chatgpt.com/docs/permissions
- Codex Managed configuration: https://learn.chatgpt.com/docs/enterprise/managed-configuration
- Claude Code Sandboxing: https://code.claude.com/docs/en/sandboxing
- Claude Code Settings: https://code.claude.com/docs/en/settings

## 却下した案

- **instructions のみ**: モデルの理解と追従に依存し、今回の比較目的を満たさない。
- **OFF のたびに policy を差し替える**: 更新途中の窓、管理者認証の反復、ON 時の直読みを残す。
- **Vault の Unix mode / ACL を変更する**: 管理アプリと AI client が同じ OS user なので区別できない。
- **アプリ独自の常駐 broker user へ全 storage を移す**: client sandbox より強いが、既存の Markdown / Git
  storage contract と配布・復旧手順を全面変更する。公式 client の管理 sandbox で同じ境界を作れる
  現段階では採らない。

## 限界

現在の自動導入 UI は macOS 向け。Windows の native Claude Code は filesystem sandbox 非対応のため、
完全保護を有効にしない。本人が管理者権限で policy 自体を削除した場合は次回検査で即座に
fail-closedへ戻るが、OS administrator 本人を攻撃者とは扱わない。すでに会話へ渡った内容は
filesystem deny では取り消せないため、比較では client 再起動と新規会話が必要である。
