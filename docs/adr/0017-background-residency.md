# ADR-0017: 管理アプリを常駐させ、trayアイコンとログイン自動起動を持たせる

## 状態

採用（2026-09-04）

## 背景

kb-app の管理アプリは、会話の最中に「いまの状態を見る」「KB利用を切り替える」「バックアップ
を確認する」ために短時間だけ開かれる。ところが実行ファイルは Rust コア・SQLite・Lindera 辞書・
埋め込み実行時を抱えるため、cold start が体感できるほど長い。開くたびに待つ形は、AI クライアント
側の会話テンポと合わない。

同じ実行ファイルは `--mcp` で MCP サーバーにもなるが（`mcp_mode`）、そちらは AI クライアントが
子プロセスとして起動するので GUI の常駐とは独立している。**常駐で速くなるのは人間が開く画面の
方だけ**で、MCP の応答時間は変わらない。

## 決定

### 1. 窓を閉じてもプロセスを残し、trayアイコンから復帰する

閉じるボタンは `CloseRequested` を握って窓を隠すだけにする。窓を破棄すると次に開くときに
起動待ちが戻り、常駐の意味が無くなる。**終了の入口は tray メニューだけ**にする
（macOS の Cmd+Q と `osascript -e 'quit app "kb-app"'` は従来どおり終了する。後者は
`scripts/deploy-macos-bundle.sh` が更新時に使う）。

左クリックは画面を開き、メニューは右クリックへ寄せる。依頼が「アイコンクリックで UI 表示」で、
Windows の通知領域もこの作法のため。Linux の indicator はクリック事象を配らないので、
そこではメニューが唯一の入口になる。

macOS では Dock アイコンを残す（`ActivationPolicy::Accessory` にしない）。管理アプリは
menu bar 常駐ユーティリティではなく、Launchpad / Spotlight / Cmd+Tab から開く普通のアプリ
として非エンジニアへ届ける（KB「Windows テスター導入を前倒しする」）。窓を隠している間の
Dock クリックは `RunEvent::Reopen` で受けて同じ復帰経路へ流す。

### 2. ログイン項目はアプリが自分で管理する

`tauri-plugin-autostart` は採らない。**技術的には plugin の方が優れている**
（3 OS の実装が保守済みで、macOS は LaunchAgent と AppleScript login item を選べる）。
それでも自前にしたのは、この機能の実体が「plist・desktop entry・Run 値を書いて読み戻す」
だけで、依存を 1 つ増やす対価として得られる量が小さいこと、そして生成物を単体テストできる形に
した方が OS ごとの差（引用の要否、XML escape、空白入り path）を見落とさずに済むためである
（`docs/coding-guidelines.md` §7 の判断軸）。sandbox 下の開発環境で新しい crate を取得できない
という制約も、この判断を後押しした。

置き場は OS 別 provider として `kb_core::autostart` に閉じる。分離の形は `ai_guard` と揃える。

| OS | 置き場 | 実装状況 |
| --- | --- | --- |
| macOS | `~/Library/LaunchAgents/app.kb.desktop.plist`（`RunAtLoad`、`KeepAlive` なし） | 実機検証済み |
| Linux | `~/.config/autostart/kb-app.desktop` | 単体テストのみ |
| Windows | `HKCU\...\CurrentVersion\Run`（`reg.exe` 経由） | 単体テストのみ・実機未検証 |

`KeepAlive` を置かないのは、tray の「終了」で切ったものを launchd が起こし直すと、ユーザーの
操作が無効になるため。次回ログインからの復帰は `RunAtLoad` だけで足りる。

### 3. 既定は有効。ただし適用は初回の1回だけ

依頼が「PC 起動時に自動起動」なので、既定を無効にすると機能が届かない。一方で、毎回の起動で
登録を書き直すと、ユーザーが OS のログイン項目設定で外した判断を打ち消してしまう。

そこで **OS のログイン項目を正本**とし、`Settings.launch_at_login_initialized` は
「一度でも既定を書いたか」だけを覚える。初回だけ登録し、以後はアプリ内 switch と OS 設定の
どちらの操作にも従う。

例外は実行ファイルの移動で、登録が残ったまま別の path を指している場合だけ書き直す
（`~/Applications` と `/Applications` の行き来で、無言のまま起動しなくなるため）。
設定画面の switch は、path がずれた登録を「有効」と表示しない。

開発ビルド（`debug_assertions`）では初回登録を行わない。`npm run tauri dev` の実行ファイルを
ログイン項目に登録しても、次の build で消えるだけで害しかない。

### 4. ログイン起動は `--hidden` で窓を出さない

ログイン直後に窓が開くのは邪魔なので、登録には `--hidden` を渡し、その場合は tray だけを出す。
窓は `tauri.conf.json` で `visible: false` にしておき、通常起動のときだけ `setup` で表示する
（起動時の一瞬のちらつきを避けるため、隠す側ではなく出す側に回す）。

### 5. tray メニューの文言は画面から渡す

言語設定は webview 側の端末設定にしかない。native 側は既定の日本語で常駐を始め、画面が
立ち上がった時点と言語切り替えのたびに `tray_set_labels` で差し替える。ログイン直後の
無表示起動から webview が読み込まれるまでの間だけ、既定の日本語が出る。

## 見送ったもの

- **単一インスタンス化（`tauri-plugin-single-instance`）**: macOS では LaunchServices が
  同じ bundle の二重起動を防ぎ、Dock / Spotlight からの起動は `Reopen` になるので不要。
  Windows では実行ファイルを 2 回起動すると tray アイコンが 2 つ出るため必要になるが、
  実機検証ができない状態で untested な IPC を足さない。Windows 実機検証の回で扱う。
- **macOS の template アイコン**: menu bar は本来モノクロの template 画像を使う。現状は
  アプリアイコンをそのまま出しており、機能はするが macOS の作法からは外れる。専用の
  モノクロ素材を用意する回で直す。

## 影響

- `tauri` に `tray-icon` feature を足す。`tray-icon` crate は既に依存グラフにあり、
  Linux CI も `libappindicator3-dev` を導入済みなので、取得する依存は増えない。
- `deploy-macos-bundle.sh` の受入は変わらない。GUI の停止は Apple Event の quit で行っており、
  閉じるボタンの握り込みは quit を妨げない。
- 常駐するぶん、アイドル時のメモリが残る。埋め込みモデルは既にアイドルでアンロードする
  （`kb_core::embed`）ので、常駐分は Tauri と SQLite 接続が中心になる。

## 関連

- `docs/requirements.md` FR-A10
- ADR-0002（層・ディレクトリ・状態・エラーの決定）
- ADR-0006（AI raw access guard。OS 別 provider の先例）
- KB「kb-app は macOS と Windows を並行検証し、Windows テスター導入を前倒しする」
