# kb-app 要件定義 v0(仮名 — 命名は未決)

2026-08-09 起草。ゼロベース。既存実装への依存・言及なしで書く。

## 一言で

**そのままでも使えるナレッジベース。AI を繋ぐと、会話で育つ外部記憶になる。**
アプリ単体でノートの作成・整理・検索が完結し、使い慣れた AI アプリ(Claude / ChatGPT /
Grok / Gemini)を繋ぐと会話の裏に自分の知識が差し込まれ、会話から得た知見が
ノートとして保存され、AIの継続蒸留で正本・記録・候補へ整理されながら育っていく。

## 段階的エンハンス(この製品の構造)

前提を持たない。各段はそれぞれ独立に成立し、上の段は下の段を置き換えず強化する:

| 段 | 前提 | できること |
|---|---|---|
| 0 | **なし**(アプリのみ) | KB の新規作成・ノート作成・編集・閲覧・全文検索 — KB も AI も持っていない人がここから始められる |
| 1 | +ローカル埋め込み(任意) | 意味検索・関連ノート(アプリが導入を代行) |
| 2 | +AI アプリ接続(任意) | 会話で引く・会話から育てる(propose → 自律メンテナンス) |
| 3 | +GitHub(任意) | バックアップ・復元・(将来)チーム共有 |

**価値仮説(2026-08-09 → 2026-08-11 訂正)**: 段0+段1 だけで独立した製品価値がある —
「**AI(チャット・サブスク)がなくても、意味で見つかるローカルのメモ帳**」を、非エンジニアが
インストールだけで持てる形は依然として見当たらない(既存のローカル意味検索はエンジニア向け
セットアップが前提。クラウド系はプライバシーと引き換え)。段2(AI 接続)はアップセルであり
前提ではない。入る理由=メモ帳、手放せなくなる理由=意味検索、育ち始める理由=AI 接続、の三段。

**訂正(2026-08-11)**: 起草時の「市場に事実上存在しない」は誇大だった。2026-04-04 の
Karpathy「LLM Wiki」公開以降、同種の製品が同時多発している(OpenKnowledge・SwarmVault・
scribe・Esment。一次情報での実測は KB ノート「kb-app 競合地図 2026-08 改訂」)。
**カテゴリは既に存在する**。空いているのは「非エンジニアがインストールだけで使える」
「日本語第一級」「所有の対称」の組み合わせであって、ローカル意味検索そのものではない。
対外的な言い方も「市場にない」ではなく「この組み合わせがない」に揃える。

## 誰のためか

- **一次ユーザー: 非エンジニア**。ディレクトリ・git・設定ファイルを見せない。
  「アプリを入れる → 使い始める」だけで KB が持てること(**KB も AI も未経験でよい**。
  AI アプリとの接続は任意のステップで、後からいつでも「繋ぐ」)
- 二次ユーザー: エンジニア(CLI・設定ファイル・自動化の口も塞がない)
- **原則「隠すが封印しない」**: vault の実体は標準の git リポジトリであり、エンジニアは
  CLI・GitHub・任意の git ツールでそのまま見て・操作してよい。アプリは外部からの git 操作を
  壊さず、外部変更後も索引・表示が自己修復する
- **前提知識ゼロ**: プログラミング能力・KB の仕組みの理解・AI のライフサイクル運用の知識を
  一切前提にしない。**そこのハードルを消すことがこの製品の存在理由**(仕組みを理解した人だけが
  運用できる KB は、既に存在する)

## コンセプト — 育つ外部記憶

知識ベースの価値は検索窓ではなくループにある:

1. **引く** — AI との会話の中で、過去の知見・判断・記録が自然に参照される
2. **育てる** — 会話で生まれた知見を保存し、AIが正本・記録・候補を判定して更新・統合する
3. **統治する** — 何が保存され、何が外に出るかを、ユーザーが機構として制御できる

## 製品原則

1. **正本は保存形式でなく Storage Contract で定義**する。ユーザーが内容を確認でき、
   repository の clone から同じ論理状態と派生機能を再構築でき、決定的 export で移行前後を
   比較できることが不変条件。現行の Markdown + OKF + Git は最初の保存 adapter であり、
   DB・イベントログ等がこの条件をよりよく満たすなら交換してよい
2. **非公開が既定**。ノートは手元に留まるのがデフォルトで、共有・公開は常に明示操作
3. ~~**書き込みは下書きまで**~~ → **撤回(2026-08-11 本人決定)**。draft 状態を廃止し、
   暫定の扱いはタグ+会話の運用へ。人間の統治は承認キューではなく、読む・タグで整理する・
   会話で指示する・削除することで行う(下記「下書き状態の廃止」)
4. **壊れたら見える**。劣化(索引停止・同期失敗)は握りつぶさず、アプリの画面に必ず出す
   続行可能な部分失敗は `data + Degradation[]` の型で返し、UIはcodeから翻訳、MCPはcode付きで
   報告する。補助データの取得失敗を空配列へ変換して「本当に0件」と同じ表示にしない
5. **境界はパスで守る**。秘匿領域はディレクトリ実体で分離し、メタデータの自己申告を信用しない
6. **外殻は使い捨て**。UI(アプリ)は派生ビューであり、コアはヘッドレスで完結する。
   AI ベンダーにも依存しない(接続は MCP 標準のみ)
7. **仕組みを学ばせない**。ユーザーが覚える概念は「ノート・つながり・バックアップ」
   程度に留める。機構の語彙(frontmatter・索引・埋め込み・MCP・scope 等)は UI に露出しない
   (エンジニア向けの詳細表示は別レイヤとして開放)。既定値で正しく動き、命名・置き場・整理を
   ユーザーに設計させない
8. **維持作業はアプリが回す**。整理・重複・リンク・鮮度といった知識の手入れをユーザーに
   計画させない。接続されたAIがユーザーの方針内で検出・更新・統合・対象固定型削除まで実行し、
   重要な結果と劣化を平易に報告する
9. **ノートは2種類 — 所有は「生まれ」で決まり、両方向に対称**(2026-08-09 決定 →
   2026-08-10 改定・本人決定)。
   - **メモ(origin: human)= 人間の領分**: AI は読む・つなげる・気づきを知らせるまで。
     本文の変更・削除は不可
   - **育つノート(origin: agent)= AI の領分**: AI が update と対象固定型の二段階削除で自律的に手入れする
     (更新・統合・陳腐化の削除)。**人間は GUI からは読むだけ**(編集・削除ボタンなし)
   - 越境は明示操作のみ: 「自分のメモにする」(agent → human。以後 AI は読むだけ)。
     逆方向(メモを AI に任せる)は将来
   - 旧・共同管理案(編集は常に受信箱の承諾経由)は置き換え。以下は原記録:
   手書きメモと AI の自己編集は相性が悪い、という洞察を構造で解く:
   - **メモ**(ユーザーが自分で書いた): AI は読む・つなげる・気づきを知らせるまで。
     **本文は不可侵**(書き換え・統合の提案をしない)
   - **育つノート**(会話の下書きを受信箱で承諾して生まれた): AI が編纂・統合・更新を
     提案できる(実行は常に受信箱の承諾経由)
   - **種類は「生まれ」が決め、作成時にユーザーへ聞かない**。越境は明示操作のみ
     (メモを「AI に任せる」/ 育つノートを「自分のメモにする」)
   - 先行知見: 境界の悩みは書く時点の規律で消す(自 KB の note-placement-rubric と同じ解法)
10. **authorityは機械管理する**。新規AIノートはpathと独立した不変`note_uid`、共通6namespace、
    `canonical` / `record` / `proposal` role、`active` / `historical` / `superseded` status、
    安定scopeを持つ。同じnamespace+scopeのactive canonicalは1件だけにし、根拠・更新・矛盾・後継は
    `note_uid`を端点にするtyped relationで表す。`proposal`は下書きや人間承認待ちを意味せず、
    AIが正本候補を区別する内部分類である。legacyノートは明示移行までenvelope不在で読める。
11. **継続蒸留は固定snapshotから再現可能に始める**。read-only plannerは同一SQLite read transactionの
    全documentへ入力hashを付け、snapshot digestと決定的plan IDを返す。previewはpull・sync・migration・
    care・outbox・KB本文を変更せず、承認キューにも実行権限にもならない。意味判断と複数ノートの変更は、
    stale-plan拒否とatomic操作を備える後続semantic executorへ分離する。

## システム構成(3層)

| 層 | 実体 | 作るか |
|---|---|---|
| 会話面 | 公式 AI アプリ(Claude / ChatGPT / Grok / Gemini) | 作らない(MCP で差し込む) |
| コア | ノートストア+索引+検索+ライフサイクル API+MCP サーバー | **本体** |
| 管理面 | デスクトップアプリ(Tauri) | コアの薄い GUI |

外観のたたき(5画面: オンボーディング / メイン / 受信箱 / 繋ぐ / つながりグラフ):
[ui-draft.html](ui-draft.html)

## 機能要件

### コア(ヘッドレス)

- **FR-C1 ストア**: vault = clone 可能な1 repository。コアは物理形式を直接公開せず、
  ノート・出典・ファイル台帳・監査痕跡という論理モデルと Storage Contract の
  `verify` / `export` を公開する。**現行 runtime** はSQLite、交換・バックアップadapterは
  Markdown + 最小 frontmatter + Git。通常の読み書きはDB transactionを起点にし、同じtransactionの
  durable outboxからMarkdownを生成する。Obsidianは表示用途で、外部編集は明示import以外では
  DBへ取り込まない。DB binaryはGitへ入れず、fresh cloneではMarkdownから再構築する。
  規約はアプリが生成・検証し、人間に暗記させない
  - **形式は OKF(Open Knowledge Format)互換を基本方針とする(2026-08-09 決定)**:
    Google Cloud が 2026-06 に公開したベンダー中立仕様(Markdown+YAML・1概念=1ファイル・
    パス=ID・type 必須の最小規約)。互換にすることで「ロックインしない」が
    標準準拠という外部証明になり、OKF ツール群とバンドルを相互運用できる。
    **spec 精読済み(2026-08-09)— 現行は v0.2 で、draft ライフサイクル・書き手 actor・
    承諾(verified)・出所(sources)が標準語彙になっている。app 固有拡張は所有境界の`origin`と、
    正本判定に必要な`note_uid` / `authority` / `relations`**。詳細は
    [okf-conformance.md](okf-conformance.md)。
    ハードロックせず追随余地を残す方針は維持(v0.1→v0.2 で実際に breaking 変更があった)
  - **アプリ契約(2026-08-10 本人決定)**: アプリが強制する最低限の不変ルール
    (タグ1〜4個必須・authorityの一意性・形式のアプリ管理 等)は **KB の外**=
    [contract.md](contract.md) に定義し、コアの検証で機構強制する。KB 内のノート
    (タグ運用等)は可変の運用合意 — 両者を混同しない(契約を KB に置くと KB の裁量で
    壊せてしまい、UI と AI の前提が崩れる)
  - **タグの統治(2026-08-10 本人決定)**: タグの種類・役割はアプリが決めず、
    **AI とユーザーの会話で合意して育てる**(アプリが状態タグ等を作るとタグ体系の強要になる)。
    合意済みのタグ・役割は「タグ運用」ノートに記録され、AI はユーザー決定を変更しない。
    **それ以外のタグは AI の裁量で付与・統合・改名・整理する**(update 経由・まとまった
    整理は会話で報告)。実装は server instructions の規律として配布
  - **分類の方針(2026-08-09 決定)**: カテゴリ体系をユーザーに設計・学習させない。
    大分類=vault / 中分類=タグ(AI・お手入れが提案、ユーザーは承諾のみ。本文外なので
    手書きメモにも原則9 と両立)/ 微細構造=つながり+検索。**フォルダ階層は UI 概念に
    しない**(ディスク実体はエンジニアが git で見える)。PARA 等の分類方法論はコアに
    焼き込まず、将来のテンプレート/プラグイン(FR-A8)として提供余地を残す
- **FR-C2 マルチ vault レジストリ**: vault の作成・登録・削除(アーカイブ)を最初から複数前提で
  設計。用途別(仕事・プライベート・趣味…)に気軽に増減できる
- **FR-C3 検索**: 現行戦略はハイブリッド(全文+ベクトル+リンク近傍)。検索方式と索引は
  正本ではなく交換可能な派生実装とする。**依存ゼロの初期状態は全文検索のみで完結**
  (これは劣化ではなく段0の正常形)。ローカル埋め込みは任意導入で意味検索が有効化、
  外部 API はオプトイン。導入後の埋め込み停止は劣化として明示する
- **FR-C4 ライフサイクル API**: propose / update / prepare_remove / commit_remove。
  新規proposeはauthority envelopeを必須化し、legacy updateでは同時にenvelopeを付与して移行できる。
  `note_uid`は作成後に変更できず、同じnamespace+scopeのactive canonical重複、参照切れrelation、
  typed relationで参照中の削除をcoreで拒否する。複数ノートを一括遷移するatomic supersedeと
  path移動は次のsemantic executor段で実装し、それまでは半端なsuperseded状態を作らない
- **FR-C5 MCP サーバー**: search / get / recent / plan_distillation / propose に加え、**update / prepare_remove /
  commit_remove
  (origin: agent のノート限定 — 原則9 改定)**と **attach(content-only・既存ノートへの
  新規添付・16MiB上限)**を公開。confirm / draft状態は持たない。
  所有ガードは UI でなくコアで強制。server instructions で「まず引く・終わりに起票を提案・
  所有の領分・会話で生まれたファイルはpathでなくattachへ」の規律を配る
  - 自動retrievalは`search(include_documents)`の1 callで、上位5件をseedに出リンク最大2ホップを
    展開し、最大50候補から推定10,000 token以内・最大10本文を同じSQLite snapshotで返す。
    被リンクは出リンクより低く扱い、重複・循環・deprecatedを除外する。選外候補はID・タイトル・
    選外理由を構造化応答へ残し、必要な場合だけ追加のMCP `get`で取得できる。候補ごとのMarkdown再読・
    索引同期・埋め込み追い付きを行わない
  - `propose` / `update`のMCP schemaは既存タグだけを受け、`allow_new_tags`を公開しない。未知引数として
    渡されても書込前に拒否し、新語追加はtrusted UI / CLIの別承認経路に限定する
  - `propose`はnamespace / role / authority status / scopeを必須入力にし、`update`はlegacy移行または
    authority変更時だけ同じenvelopeを受ける。`get` / search / recentは`note_uid`とauthorityを返す。
    typed relationはpathでなく26文字ULIDの`note_uid`を端点にする
  - note read / create / update / attach、degradation、KB OFF、tool errorは
    `structuredContent.conversation_events` v1へ`required=true`で返す。対応hostはこれを会話へ
    決定論的に描画し、モデルがリンク・警告を言い直すかどうかを保証点にしない
  - 直接`remove`は公開しない。`prepare_remove`は対象IDと内容指紋へ固定した5分token、対象名、
    `removal_prepared` eventを返す。`commit_remove`はdestructive annotationを持ち、同じnoteと未使用token
    だけを受理する。AIは蒸留・メンテナンス方針の範囲内で個別の人間承認なしに両toolを続けて実行できる。
    期限切れ、対象差し替え、準備後変更、二重実行は削除前に拒否し、対象・理由・履歴を報告する
  - `plan_distillation`は準備済みDBをread-onlyで開き、同一snapshotの全ノートへinput hashを付けた
    mechanical-v1候補planを返す。remote pull、索引同期、schema migration、care/outbox更新を行わず、
    MCP annotationもread-only / idempotentに固定する。同じsnapshotのJSONはbyte-identicalとする
- **FR-C6 プロバイダ別プロファイル**: instructions・ツール説明をクライアント別に出し分けられる
  構造(2026-08-19実装)。`ClientSurface`はClaude Code / Codex CLI / Claude Desktop /
  ChatGPT / 評価harness / unknownをactor先頭segmentから厳密に判定し、同じmodel familyでも
  OS guard・自動retrieval・current-note schemaを分ける。Codex CLI / Claude Codeの`get.note`は
  必須、アプリが現在ノートを記録できるClaude Desktopだけ省略可能。initializeの
  `capabilities.experimental.kbApp`へsurfaceと保証レベルを構造化して返し、unknownはfail-closedとする
- **FR-C8 ファイル(2026-08-10 添付として実装 → 2026-08-12 Artifact へ改定 →
  2026-08-13 コアとノート内のファイル欄まで実装、旧経路は読み取り専用に。
  2026-08-18 MCP content-only添付とgetのArtifact一覧を実装。
  [ADR-0003](adr/0003-artifact-storage-and-transport.md) が正本)**: ノートは画像・
  原本ファイル等を「所有」できる。UI 語彙は「ファイル」(manifest・CAS・policy といった
  内部語も、ディレクトリという言葉も見せない — 原則7)。
  - **実体は Vault Git の外**に置き、Vault Git に入るのは manifest・参照・
    full 転送用の pointer まで。`sensitivity`(非公開/共有)と `sync_policy`
    (このPCのみ/別PCでも復元)の**二軸**を持ち、client repo 由来は `local_only` 固定で
    プロンプトから緩和できない。availability は同期せず端末ごとに導出する
  - **full 転送は同一 origin の Git LFS**(`git-lfs` は公式releaseのchecksum固定sidecarとして
    同梱。配布CIはambient版なしの実処理とpackage内容を検査し、ユーザーに追加設定を求めない)。
    manifest の取得と blob の取得を分離し、取得失敗は端末ごとの「この端末にない」として出す
  - **旧方式 `<id>.files/`(同名サイドカー)は legacy transport**。ノート=1ファイルの
    OKF 互換を壊さない利点(`index.md` 予約名衝突の A 案・ID が汚れる B 案を棄却した理由)は
    そのままだが、**新規の保存先にはしない**(2026-08-13 に書き込み経路を削除済み。
    読み取りだけ残す)。既存分は blob を動かさず manifest を重ね、
    LFS へ上げて成功を確認してから参照を切り替え、旧ファイルは fallback として残す
    (MVP では削除しない)。**移行前に全端末から取得できた添付は、移行で取得不能にしない**
  - 「1ノート=1単位」の対の管理(rename/move/archive 時の同伴)はアプリが保証する。
    検索対象は manifest まで(blob の中身は既定で対象外、transcript は既定で除外)
  - サイズは `local_only` に固定上限を置かず、`full` のみ 100MB 警告・
    2GB 拒否。**旧来の 10MB / 50MB は GitHub 同期の保全が根拠で、実体が Vault Git を
    出た時点で失効する**。path取り込みはstreaming。MCP content取り込みだけはJSON-RPCの
    Base64を使うため16MiBで先に拒否する
  - 出典は OKF `sources[].resource` に `kb-artifact:<artifact_id>` を書いて**版を固定**する
    (標準語彙の provenance)。本文リンクは参照名で最新版を追い、既存の `/…files/…` は
    本文を書き換えず alias で解決する
  - 画像はペーストで自動取り込み+リンク挿入、プレビューで表示。path を扱う
    picker / drop / paste / CLI は同じコア API に合流させる。MCP は `attach` のcontent経路だけを
    公開し、path・policy・role・media type・origin・by・at・supersedesを受け取らない。
    既存ノートの実在を確認してから、server管理の一時file経由で同じstore / ledgerへ合流する
  - ファイルの中身検索(PDF 抽出等)、dataset の複数ファイル管理、実削除を伴う GC は将来
- **FR-C7 お手入れ(ライフサイクルの自動運転)— 2026-08-20 semantic executor v1段まで実装**:
  authorityとtyped relationから、正本更新・記録抽出・proposal統合・legacy未解決・description正規化の
  候補をsnapshot固定で列挙する。executor v1は候補ノートを全文取得したAIから構造化targetを受け、
  plan schema/profile/ID、snapshot、全input hashとoperationを同じwrite transactionで再照合する。
  既存authority付きAIノートのnormalize、active canonical revise、record lineage extractを全件成功または0件で
  実行し、request hashによる二重実行拒否、実行前後document監査、後続変更前のatomic rollbackを備える。
  record本文・UID・authorityは不変とし、create、semantic merge、atomic supersede、新規extract／split、
  legacy backfill、別端末自動rollbackは次段。planとexecutionは人間の承認キューを作らない。方針内のAI管理ノートは
  update・atomic supersede・対象固定型二段階削除で自律メンテナンスし、ユーザーへwaveごとの承認作業を
  戻さない。legacy `origin: human`は互換読み取り専用の所有境界を維持する。リンク切れ・契約違反など
  自動修復できない劣化は、該当なしへ潰さず結果とともに報告する

### 管理アプリ(Tauri)

- **FR-A1 オンボーディング**: 初回起動で最初の vault を自動作成し、そのままノートが書ける
  (段0の実体)。AI アプリ接続(まず Claude Desktop / Claude Code)・ローカル埋め込みの導入は
  **任意の「繋ぐ」ボタン**としてアプリが代行。スキップしても全機能の段0が成立
- **FR-A2 ホーム**: vault 一覧+健全性(ノート数・索引状態・劣化警告・未バックアップ)
- **FR-A3 受信箱**: 廃止。下書き・承認キューを持たず、ノートのauthorityはAIが機械管理する。
  ユーザーへはメンテナンス結果と劣化を通知し、内部role/statusの操作を要求しない
- **FR-A4 ノートビュー+エディタ**: 一覧・本文閲覧・メタ・リンク表示に加え、**作成・編集**
  (Markdown の素朴なエディタ+プレビュー。執筆環境の再発明はしない — 高度な編集は
  外部エディタに委譲可)。アプリ内検索 UI もここ(AI なしで検索が完結)
- **FR-A5 起動ランチャ**: 登録済みの AI を、**ノート一覧・ノート画面からそのノートの文脈で
  直接起動**できる。起動形態は AI ごとに事前設定 — デスクトップアプリ / ターミナル
  (既定はアプリ。ターミナルはエンジニア向けで、Claude Code 等を vault ディレクトリで開く)。
  ノート文脈の受け渡しは、コアが「いま見ているノート」状態を持ち MCP 側から参照可能にする。
  v0ではClaude Desktopだけ`get`のnote省略を公開し、Codex CLI / Claude Code / ChatGPTは
  schema上必須にして、存在しない現在ノートを暗黙参照しない
- **FR-A6 GitHub 連携(同期型へ改定 — 2026-08-10 本人決定)**: ユーザーが設定した
  GitHub リポジトリを唯一のバックアップ先とし、**ノートの変更は随時 push**(明示のみ →
  自動へ変更)、**複数デバイス運用を想定して、AI とのメッセージのやり取り(MCP ツール
  呼び出し)や画面更新の際に pull** して他端末の変化を取り込む。非エンジニアには
  「バックアップ / 復元」の語彙で見せ、エンジニアには remote 設定の実体をそのまま開放する。
  安全機構: push 先は紐付けたリポ(origin)に固定 / **push 対象はノートと Git 管理される
  正本(Full Artifact の台帳・参照・LFS pointer を含む)のみ**。索引・キャッシュ等の派生物と
  `local_only` は構造的に除外し、Full の実体は private 確認後に Git LFS へ送る / push 前の
  private 実確認は認証済み GitHub API で行う / 同期の失敗は握りつぶさず劣化として表示(原則4)/
  pull は時間スロットリング(会話のたび全呼び出しで同期待ちしない — 旧 KB の
  レイテンシ実測の教訓)/ 資格情報プロンプトでハングさせない(非対話モード強制)/
  **生成ファイルは競合させない**(実装時に多デバイステストで実証した必須条件:
  index.md = merge ours+pull 後に再生成で自己修復、log.md = union merge。
  ノート本文の競合は rebase 失敗 → 劣化表示に倒し、自動解決しない)
  **2026-08-16 補足**: 初回導線は「アプリが private repository を新規作成」と
  「既存 Vault の private repository を使う」に分ける。後者は clone 後に Storage Contract と
  `.kb-workspace` を検査し、同じ ID の Vault だけを同期対象として接続する。別 ID を自動で
  merge・上書きしない。privacy と push 権限は接続時および各 upload 直前に認証済み GitHub API
  で確認し、判定不能を含めて fail-closed とする。失敗は正常なpullで消えないlatchとして残し、
  private + push権限の再確認に成功したときだけ解除する。rebase後のretry pushも再検査する。
  fresh clone は全 `full` LFS object を明示取得し、
  hash 照合が終わるまで復元完了としない。検査・clone・object検証・登録の進捗を表示し、再試行では
  workspace単位の端末storeに残る検証済みobjectを再利用する。認証・権限・privacy・通信・remote欠損・
  quota・LFS欠損・hash不一致・競合等はtyped reasonとして区別する(詳細: ADR-0005)
- **FR-A7 グラフビュー(v1 実装済み 2026-08-10)**: Obsidian ライクな力学グラフ。実現性は確認済み —
  つながりは索引に保存済みで、描画層(Canvas / WebGL の力学グラフライブラリ)を被せるだけ。
  NFR-3 の規模(10^3〜10^4)は既存ライブラリの守備範囲。差別化は「操作できるグラフ」:
  メモ=緑 / 育つノート=琥珀の2色で原則9 を可視化し、お手入れの「つながり提案」を
  点線エッジとして表示してその場で承諾できる(受信箱と同一の提案を別の面から見せる)
- FR-A8(将来)プラグイン機構(パネル・フック・コマンド)
- **FR-A9 AIでのKB利用ON／OFF(2026-08-17 本人決定)**: 設定Modalから、AIとの会話で
  KBを使うかを端末単位で切り替えられる。OFFでも管理アプリと保存済みノートは利用でき、
  データを削除しない。MCPはOFF時もtools capabilityとON時と同じtool setを公開し、promptsは公開しない。
  instructionsはKBの内容・保存先を含まない迂回禁止ルールだけを返す。既存processを含む全tool
  callは、引数やtool名にかかわらずpull・索引更新・Vault読み書きより前に、同じ構造化終端結果
  `kb_disabled`（`authoritative=true` / `retryable=false` / 空data）で拒否する。会話へすでに渡った文脈は
  取り除けないため、比較時はAIアプリを再起動して新しい会話を使う。全体switchに加え、
  ClaudeとGPT／Codexを個別にON／OFFでき、全体OFFは個別設定より優先する。Claude Codeの
  前出し・Stopフックも同じ設定に従う。生ファイルはON/OFFにかかわらず、Codexの管理
  permission profileとClaude Codeの管理sandboxを通じてOSレベルで読み書きを拒否する。
  Claude Desktop / ChatGPT通常チャット面はkb-app MCPに生path入力を公開しないbroker境界で分離し、
  coding agent用OS policyやmanaged hookの存在を通常チャットの保証として流用しない。
  ONはMCPからデータを返せるようにし、OFFは接続とtool schemaを残したままデータ経路を閉じる。
  coding agentで管理ポリシーの導入状態と登録Vaultのpathが一致しない場合は、設定switchを操作不能にしてMCPも
  同じ終端結果でfail-closedとする。既存の管理者ポリシーは自動で
  上書きしない。macOSでは設定Modalから管理者認証を経て導入・更新できる。CodexとClaude Codeの
  managed `UserPromptSubmit` hookは、モデルが自発的にtoolを選ぶ前にkb-app MCPの
  `initialize → search(any, include_documents)`を実行し、同じDB snapshotで検索seed・リンク候補・
  予算内本文を選んで文脈へ注入する。OFFはsearchの
  `kb_disabled`終端結果を検知して注入せず、失敗・劣化は該当なしと区別してクライアントへ返す。旧Claude Python hookの
  CLI直検索と、MCP未使用をtranscriptで推測するStop hookは廃止する。

### 将来(ステージ外)

- リモート MCP(公開 HTTPS + OAuth)→ モバイル・ChatGPT / Grok / Gemini 接続
- チーム共有(レビュー付き昇格 — GitHub PR ベースを想定)/ 配布(.dmg 署名・.mcpb)

## 非機能要件

- **NFR-1 ローカルファースト**: 既定構成で外部送信ゼロ。クラウドは全てオプトイン
- **NFR-2 3歩セットアップ**: インストール → 接続 → 利用開始まで、技術知識ゼロで 10 分以内
- **NFR-3 スケール目標**: vault あたり 10^3〜10^4 ノートで検索が体感即時。
  10k deterministic fixtureに対するMarkdownからの初回DB復元・カテゴリ・ページ一覧・全文検索・
  semantic KNN・ノート詳細の時間予算をrelease CIで検査する
  ([性能回帰gate](performance-gate.md))
- **NFR-4 検索品質**: 汎用 RAG(フォルダ丸読み型)を上回ること — リンク構造・メタデータを
  活かした検索が製品の差別化点。本人が管理するGolden Queryを使い、評価専用の旧top3方式と
  本番のリンク連鎖retrievalを同じDB snapshot・同じ検索順位で比較する。本文候補／選択recall、
  precision、明示除外、hop、本文量、時間をJSON／Markdownへ記録し、実発話は自動収集しない
  ([Retrieval効果測定](retrieval-evaluation.md))
- **NFR-5 多言語**: 日本語第一級(検索・UI とも)
- **NFR-6 概念の上限**: チュートリアルなしで使い始められること。ユーザーが理解すべき概念は
  「ノート・つながり・バックアップ」の3つ程度を上限とし、専門概念の理解を
  いかなる操作の前提にもしない
- **NFR-7 保存の可搬性**: 保存 backend の採否は、(a) ユーザーが内容を確認できる、
  (b) fresh clone と必要な公開依存だけで再構築できる、(c) 決定的な論理 export の digest が
  移行前後で一致する、(d) 索引・キャッシュを削除しても情報を失わない、を自動テストで判定する。
  形式の読みやすさだけを理由に性能・利便性を犠牲にしない

## ロードマップ(案)

| 版 | 中身 | 検証すること |
|---|---|---|
| v0.1 | コア最小(ストア・レジストリ・検索・propose)+ MCP。CLI のみ | Claude Desktop 接続で「引く・育てる」ループが回るか |
| v0.2 | 管理アプリ骨格(オンボーディング・ホーム・ノートビュー+エディタ・検索 UI) | **AI なしで KB として完結するか(段0)** |
| v0.3 | 受信箱・ランチャ・GitHub 連携・「繋ぐ」体験 | 段1〜3 の任意接続が非エンジニアに成立するか |
| v1.0 | 配布形態(.dmg)・ドキュメント | 第三者が導入できるか |

## 大改定(2026-08-10 本人決定): 用途の一本化 — 「AI の知識ベースを人間が統治するアプリ」へ

- **個人ノート(メモ)機能を廃止**。書き物は本アプリの守備範囲外(実務では Obsidian+KB が担当)。
  価値仮説「段0=AI なしのメモ帳」を放棄する(原記録は上に残す)。既存の human ノートは
  削除し、移行由来の知識ノートは AI 管理(origin: agent)へ移譲 — **全ノートが AI の領分**になった
- **受信箱を廃止**。下書きの承諾・お手入れ提案は**ノートの場**で(開くと承諾バー)。
  絞り込みは**状態(下書き・提案)+タグのフィルタチップ**で行う(分類方針「中分類=タグ」が
  UI に現れた形)。FR-A3 はこの形に置き換え
- 人間の操作面: 読む・検索する・承諾する・添付する・Claude に指示する(書き換えは Claude 経由)。
  新規作成・編集・退役・削除の GUI / CLI は撤去。通常の書き込みは AI クライアントの
  propose / update / prepare_remove / commit_remove に限定し、移植の raw write はコア内部の互換経路に閉じる
- タグ契約は propose / update / 旧Markdown import の全入口で同じcore validationを通す。
  importでも1〜4個・語形は常時強制し、語彙外は明示フラグがある場合だけ許可する。
  外部編集や既存データの違反は読み込み時に黙って修正・破棄せず、careで可視化する
- グラフの2色は「ノート(緑)/下書き(琥珀)」の状態表示に再定義

### 追加改定(2026-08-11 本人決定): ノート間の関連づけは AI の領分

「つなげるボタンは不要。ノートの関連性を決めるのは AI であり、ユーザーが独断で決めるのは
方針に反する」との判断により、お手入れ(FR-C7)の**「同じ話題に見えます。つなげますか?」
提案とその承諾機構を撤去**した。代わりに:

- 人間には「✨ 近いノート」パネル(閲覧・並べて開く用)で近さを見せるだけにする
- **AI には get の応答に「近いノート(まだリンクされていない)」を添える** — 関連づけの
  判断材料を機構で渡し、実行は AI が update でリンクを足す形にする(可否を人に尋ねない)
- お手入れに残る検知は「リンク切れ」「タグ無し(契約違反状態)」の**気づき**のみ(確認するだけ)

### 追加改定(2026-08-11 本人決定): 下書き状態の廃止

「下書きはただのタグでしかなく、そのためだけに知識ベースへ機能を作るのは NG。
どう扱うかはユーザーが AI と話し合って決めるべき」との判断により:

- **status: draft を廃止**(propose は通常ノートを作る)。confirm / draft_reject と
  承諾バー・下書きバッジ・下書きタイル・グラフの状態2色をすべて撤去。既存 draft 8本は移行
- **原則3(書き込みは下書きまで)は撤回**。所有の対称(AI ノートは AI の領分)により
  実質的な役割は終えていた。人間の統治手段は「読む・タグで整理する・会話で指示する・
  削除する」であって、承認キューではない
- 契約(docs/contract.md)から draft 条項を削除し、「契約に入れないもの」として明記

## 受け入れ記録(追記式)

- 2026-08-09 v0.1 検証(部分合格): Claude Desktop 再起動後、kb-app の4ツール
  (search/get/recent/propose。confirm は意図どおり非公開)が表示され、
  **既存 vault / team-vault サーバーの動作に変化なし**を実戦クエリ(「認証まわりのメモ」)で
  確認。server instructions による自発検索の規律は3サーバー並立でも成立。
  ただしこのクエリでヒットしたのは既存 vault 側で、**kb-app 自身の「引く・育てる
  (propose → confirm)」一周は未観察** — 残る受け入れ項目
- 2026-08-10 **v0.1 検証合格**: Desktop の実会話(株の勉強プラン策定)から propose が発火し、
  OKF 語彙どおりの draft(generated.by=claude-desktop/claude・sources=conversation
  descriptor・origin=agent・タグ自動付与)が起票され、CLI confirm で
  stable+verified(human:owner)化。git に propose → confirm の監査痕跡。
  **「引く・育てる」ループが Claude Desktop で一周成立**
- 観察メモ: 移行期は kb-app と既存 vault が並立するため、「どちらに引きに行くか」の
  routing はモデル任せになる。本移行の段階で instructions での役割宣言(または旧側の停止)を
  検討

- 2026-08-10 **v0.2 検証(基本合格)**: 実機(Tauri)で作成・閲覧・編集・検索の4動作を
  本人確認。作成は window.prompt が WKWebView で無効という Tauri 特有の穴を踏み、
  アプリ内モーダルに置換して解決(教訓: ダイアログは必ず自前実装。ブラウザ検証は
  この穴を素通りするため、実機確認を省略しない)。受信箱(最小)もブラウザ検証済み。
  段0 の細かい使用感(エディタの物足りなさ・並び順・検索の当たり方)は運用継続で収集

- 2026-08-10 **kb-app へ一本化(本人決定)**: 旧 KB(personal+team)の50本移植を受けて、
  以後の正本は kb-app 側(~/kb/try → mcaz/private-vault 同期)。実施: Claude Desktop の
  MCP を kb-app のみに(旧 vault / team-vault エントリを撤去、バックアップ
  `.bak-20260810-consolidate`)、Claude Code の user スコープに kb-app を追加、
  ソースを github.com/mcaz/kb-app(private)へ push。
  **残**: Claude Code 側の旧 vault / team-vault サーバー引退(フック・ルーチン・
  グローバル CLAUDE.md が旧 KB に依存しているため、kb-app 側の機能充足
  — check / feedback / ルーチン相当 — と合わせて段階的に)
- 2026-08-10 **司書運用を移植・旧ルーチン停止**: 前出しフック(kb search --any)・
  グローバル CLAUDE.md(kb-app 前提・大減量)・kb-researcher エージェントへ切替
  (integrations/claude-code/)。新フックの実働は本人確認済み。旧ルーチン6本
  (scout-law / scout-tech / scout-interests / follow-up / weekly-review / maintenance)は
  スケジューラ上で **enabled: false に** → 同日 **完全削除**(本人指示。定義ファイルは
  ~/.claude/scheduled-tasks/ に残置)。旧 KB への書き込み経路はこれで全停止。
  **方針(本人)**: 今後のお手入れ FR-C7・自動運転は旧・チーム/パーソナル二層運用の
  移植や再現を前提にせず、**製品としてゼロベースで設計する**(統治史に引っ張られない —
  ゼロベース切替の初心と同じ)
- 2026-08-10 **段1(かしこい検索)実装・引退完了**: ort+bge-m3 int8 内蔵、FTS×ベクトル
  RRF 融合(関連判定は生距離 0.95)、53/53 埋め込み(34秒)。意味クエリで事故ノートが
  最短距離ヒット — 前出し品質が旧システム同等に復帰。ベクトル索引は BLOB+Rust 総当たり
  (個人規模判断。sqlite-vec は PoC 実証済みの規模対策として温存)。
  同日、Claude Code の旧 vault / team-vault サーバーを撤去し ollama を停止 —
  **旧システムの可動部はゼロになり、一本化が完了**

## 未決

- 命名(kb-app は仮)
- ~~コアの実装言語~~ → **Rust+TS に決定(2026-08-09 本人決定)**。
  [adr/0001-core-language.md](adr/0001-core-language.md)(代替案・PoC 判定条件つき)
- ~~frontmatter の詳細設計 — OKF 準拠+app 拡張の適合方法~~ → **適合設計 v0 起草済み**
  ([okf-conformance.md](okf-conformance.md))。残る判断: `origin`(仮)の命名(下記
  「ノート2種類の呼び分け」と同時)/ ローカルユーザーの actor ID / type 語彙の拡張タイミング
- ~~既存ユーザー(開発者自身の現行 KB)の移行時期と互換レイヤの要否~~ →
  **実施済み(2026-08-10 本人指示)**: 互換レイヤ `kb import`(旧 frontmatter → OKF 変換・
  legacy キーに旧メタ無損失保持・`[[wikilink]]` → markdown リンクのソース横断解決)で
  personal 33 本+team 17 本を移植、バックアップ先へ同期済み。旧リポは無変更(読み取りのみ)。
  残: 旧 vault / team-vault MCP サーバーの引退時期(移行期の並立 routing 観察と合わせて判断)
- ~~GitHub 連携の認証方式~~ → **OAuth App のデバイスフロー + OS キーチェーンに決定・実装・
  実 GitHub 受入済み(2026-08-16)**。`mcaz` 所有の配布用OAuth Appを登録し、`repo` scope の広さを
  認証前に表示、取得後は `/user` で account を検証する。専用private repositoryで作成・private+
  push再検査・初回push・別ディレクトリ復元を完走した。Git/Git LFSにはprocess限定の認証headerを
  渡し、URL・`.git/config`・アプリ設定へtokenが残らないことを確認済み。
- ~~埋め込みランタイムの同梱方式~~ → **PoC で成立を実証(2026-08-09)**: ort+bge-m3 int8
  同梱(合計約586MB)で品質・速度・サイズとも PASS、既存較正の移植可も実測
  ([poc-report.md](poc-report.md))。残: 常駐時メモリ(RSS 1.8GB)の抑制方式
- ノート2種類の呼び分けの命名(「メモ / 育つノート」は仮。候補: メモ/ノート、メモ/まとめ 等。
  UI 上はバッジ程度の控えめな表示にし、機構概念として教えない)
- 複数クライアントの同時アクセス → **方針は PoC で実証(2026-08-09)**: WAL+全接続
  busy_timeout+`BEGIN IMMEDIATE`+fail-open(劣化フラグ)で成立
  ([poc-report.md](poc-report.md))。propose の競合など上位レイヤの設計は本実装で
- タグ自動提案の挙動(自動適用して後から直せる形か、受信箱の承諾を通す形か。
  通知疲れと統治感のバランス)
- vault コミットの著者情報(現状はグローバル git 設定/GECOS 由来を継承 — 随時 push で
  private バックアップ先へ私用メール・ホスト名が乗る。旧 KB で実害記録のある事故型。
  app 固定 actor に寄せるか、デバイス識別として活かして GitHub 側の秘匿設定で守るか)
- Claude Code 接続(v0.3 後半): 接続代行は Desktop のみ実装済み。Code 側は
  「vault ディレクトリに .mcp.json を同梱し、ランチャの『Claude Code で開く』で
  そこを起動ディレクトリにする」方式(旧トラック external-app-design の設計を流用)
