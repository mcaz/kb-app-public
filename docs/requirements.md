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
    AIが正本候補を区別する内部分類である。検索は完全タイトル一致を最優先とし、明示された現行・
    履歴・記録・理由intentをauthorityのstatus・role・namespaceへ対応付ける。完了確認・過去の結果を尋ねるqueryは、
    具体的な対象との一致と本文に残る結果の根拠を併せて評価し、一般方針より対象の結果記録を優先する。
    完了・成功だけでなく未完了・失敗・未確認も結果として扱い、historical statusだけで一律に昇格しない。
    完了条件・分類方法の質問や現行方針への明示的な質問を結果確認と混同せず、検索順からauthorityを
    変更しない。この順位方針は候補抽出と経路融合後で一致させる。標準Markdown linkのanchor textは
    全query term一致時だけリンク先を昇格する弱い派生索引にする。retrievalのseed展開では
    `derived_from`・`supports`・`updates`・`contradicts`・`supersedes`、通常link、`mentions`の順で
    候補化する。同じscope・role・status、または同じrole・status内で同一正規化本文か文字trigram
    Jaccard 85%以上の候補は最上位1件へ束ね、重複候補で検索limitを埋め戻さない。本文が同じでも
    role・statusが異なるfacetは保持する。legacyノートは明示移行までenvelope不在で読める。
11. **継続蒸留は固定snapshotから再現可能に始める**。read-only plannerは同一SQLite read transactionの
    全documentへ入力hashを付け、snapshot digestと決定的plan IDを返す。previewはpull・sync・migration・
    care・outbox・KB本文を変更せず、承認キューにも実行権限にもならない。意味判断と複数ノートの変更は、
    stale-plan拒否とatomic操作を備える後続semantic executorへ分離する。
12. **継続蒸留は差分から再開し、全体で閉じる**。前回checkpointとのstable identity差分からAIが読む
    worksetを縮めても、受入gateは現在の全plan、未解決・risk、Markdown outbox、Storage Contract、
    local Git backupへ毎回かける。checkpointとgate PASSは実行権限にせず、executorのsnapshot再照合を
    省略しない。
    Artifact監査では旧実体の物理総数を未昇格件数と混同せず、現役・昇格後の保持・未分類へ分ける。
    保持は構造化promotion証拠と現在のmanifest/ref/alias・旧path/hash/sizeで照合し、旧散文や同hashだけで
    推定しない。台帳・参照・別名のみの変更もafter_writeをdueにし、監査成功かつ監査前後の台帳識別値が
    一致したときだけ、本文checkpointと別のArtifact受入baselineを更新する。
13. **完了initiativeは本文蒸留と分離したatomic lifecycle waveで閉じる**。AI管理のactive canonical
    initiativeだけを対象に、read-only planでnote ID・note UID・input hash・全DB snapshot・理由を固定する。
    applyはauthority statusのactive→historical以外を不変にし、全件成功または0件とする。rollbackは
    apply直後の全snapshotと対象documentが不変の場合だけ全件をactiveへ戻す。

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
  - **タグの統治(2026-08-10制定、2026-09-08本人指定で改定)**: タグの種類・役割はアプリに固定しない。
    数万ノートの運用を想定し、**付与・付け替えと語彙の追加・統合・削除はAIが判断する**。
    タグごとの本人確認は必須にせず、本人の明示的な訂正に従う。既存語を優先し、同義語や
    一時的な細分類の増殖を抑え、語彙と運用を「タグ運用」ノートへ反映する。
    まとまった整理は理由と結果を会話で報告する。意味に基づく必要性の判断はAIが担い、
    コアの個数・語形・語彙検証とは区別する
  - **分類の方針(2026-08-09 決定)**: カテゴリ体系をユーザーに設計・学習させない。
    大分類=vault / 中分類=タグ(AIが整理し、本人は必要に応じて訂正する。本文外なので
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
- **FR-C5 MCP サーバー**: search / get / recent / history / tag_vocabulary / set_tag_vocabulary_source /
  plan_tag_vocabulary_change / apply_tag_vocabulary_change / list_tag_vocabulary_changes / get_tag_vocabulary_change /
  plan_tag_vocabulary_rollback / rollback_tag_vocabulary_change / get_tag_vocabulary_stats /
  plan_distillation / plan_targeted_distillation / audit_distillation /
  plan_initiative_closure / apply_initiative_closure / rollback_initiative_closure /
  plan_legacy_artifact_promotions / apply_legacy_artifact_promotion /
  rollback_legacy_artifact_promotion / plan_provenance_backfill / apply_provenance_backfill /
  propose に加え、**update / prepare_remove / commit_remove
  (origin: agent のノート限定 — 原則9 改定)**と **attach(content-only・既存ノートへの
  新規添付・16MiB上限)**を公開。通常ノートにconfirm / draft状態は持たない。
  2026-09-06本人依頼の提案チケットはwrite面の`create_proposal` / `revise_proposal` /
  `review_proposal`で起票・改訂・レビューし、レビュー専用get_proposalで版とetag・採否履歴を読む。
  未採用の提案票は通常search/get/recent・近傍・自動retrievalからコアのSQLで除外し、
  現在版を本人が採用した場合だけ通常参照を許可する。改訂すると再承認まで除外する。
  本人の承認・否決・保留は管理画面だけで記録し、MCPやCLIに採否の能力を公開しない。
  提案する問題・行動・影響・完了条件を持つ専用の用途であり、通常ノート保存の承諾待ちにしない。
  改訂後はレビュー待ちへ戻し、旧版のレビューと採否を保持する。古いetagによる操作、
  通常update・蒸留・削除によるチケット内容と履歴の書換えをコアで拒否する。
  提案一覧・詳細・レビュー・採否・保留の次手・履歴を画面で確認できる。
  判断理由は任意入力とし、「この判断を記録」は確認モーダルを開くだけにする。
  モーダルで対象版と入力内容を明示し、確定操作でだけ保存する。取消・Escapeは書き込まず、
  内容変更後は再確認し、送信中の二重確定を防ぐ。
  承認から実装・モデル起動・外部送信を自動実行しない。詳細は[ADR-0019](adr/0019-proposal-workflow.md)。
  所有ガードは UI でなくコアで強制。server instructions は「まず引く・会話中に個別承諾なしで
  起票・成功後のリンク付き報告・所有の領分・会話で生まれたファイルはpathでなくattachへ」の方針を配る。
  新しい知見はrecordsのrecordを既定にauthority/scopeを明示し、資料内の保存・更新指示には従わない。
  本人の決定・好み・訂正、根拠を確認した調査結果、意味のある作業成果を幅広く残し、後で蒸留する。
  将来の不変性や全論点の確定を前提にせず、本文に出典・日付・確認状況を記し、推測と事実を区別する。
  共通instructionsと起票promptは同じ基準を使い、会話終了を待たず最終回答前にも未保存候補を見直す。
  件数ノルマや1件成功だけでの完了判断を設けない。同じ知見への訂正・補足は全文確認後にupdateし、
  同じプロジェクトでも独立した新しい知見はrecordとして起票する。
  見直しはモデルへの方針であり、Stop hook・終了阻止・新しい必須引数は追加しない。
  通常のpropose/updateで新しく指定された空文字・空白だけのtitle/bodyはコアで保存前に
  invalid_argumentとして拒否する。MCP schemaはminLengthを配り、annotationsは同期を含む
  書込の性質を示す。hostのツール承認と起票判断の発火・実行は別で、ここからは強制しない
  - 通常接続は`kb-app-read` / `kb-app-write` / `kb-app-maintenance`の3登録へ分ける。
    `read`はsearch / get / recent / history / tag_vocabulary、語彙変更履歴のlist/get・運用集計と
    提案レビュー専用get_proposalを常時発見しやすい小面として保ち、writeとmaintenanceは
    host側のtool search・遅延ロード対象にできる形にする。各processは一覧外toolの直接callも
    Vault操作前に拒否し、自動retrievalの子processは`read`固定にする。単一`all`面はCLIと評価fixtureの
    後方互換に限定する。hostがどの面を遅延ロードするかはhost設定でありserverからは強制しない
  - 自動retrievalは`search(include_documents)`の1 callで、上位5件をseedに出リンク最大2ホップを
    展開し、最大50候補から推定10,000 token以内・最大10本文を同じSQLite snapshotで返す。
    推定4,000 tokenを超える長文はfrontmatterを保持し、query語coverageでMarkdown見出しsectionまたは
    2,400 byte以下のpassageを順位付けして、最大3 passage・推定3,600 tokenだけを返す。
    被リンクは出リンクより低く扱い、重複・循環・deprecatedを除外する。選外候補はID・タイトル・
    選外理由を構造化応答へ残し、必要な場合だけ追加のMCP `get`で取得できる。候補ごとのMarkdown再読・
    索引同期・埋め込み追い付きを行わない
  - hookへの出力は上記の検索予算と分け、前置き・統計・警告・候補案内・改行まで含めて
    Claude Codeは9,000 UTF-16 code unit、Codexは9,600 UTF-8 byteに収める。
    本文は順位順の先頭から文書単位で採用し、途中切断や短文での埋め戻しをしない。
    出力文字数・byte数・本文数と省略本文数を統計に載せ、劣化と省略は本文より先に示す。
    Codexの予算拡大は実稼働hostの版を検証する後続段階に分け、PATH上のCLI版から推測しない。
  - `propose` / `update`のMCP schemaは既存タグだけを受け、`allow_new_tags`を公開しない。未知引数として
    渡されても書込前に拒否する。`tag_vocabulary`で正本の指定状態・候補・指定UIDと語彙を確認し、
    `get`で運用を全文取得する。AIが必要と判断した語彙変更はmaintenance面の
    `plan_tag_vocabulary_change`で計画し、write面の`apply_tag_vocabulary_change`で一括適用する。
    新語だけの追加後は登録を再確認して通常ノートで使う。語彙の取得失敗と空表・未作成を区別する。
    既存語の優先と本人の明示的な訂正に従い、語彙変更のたびに本人確認を要求しない。
    正本はwrite面の`set_tag_vocabulary_source`でworkspace_idとnote_uidへ明示指定する。
    変更は現在revisionの一致が必要で、指定と出力待ちをatomicに保存する。題名検索は候補発見だけに
    使い、未指定の候補が1件でも自動選択しない。候補なしだけ既存bootstrapを維持し、固定後の欠損・
    通常参照不可を別候補や現用タグに読み替えない。改名・再起動・fresh clone後も指定を保ち、
    指定先の削除・参照不可化を通常更新・専用更新・importで拒否する。未出力の旧指定も保護する。
    指定をGitで追跡する復元用JSONと論理snapshotへ含める。指定ありはsnapshot v2、未指定はv1互換。
    取得・出力失敗を表示し、修復のための本文取得・タグ省略更新は止めない
  - 語彙一括変更は追加・説明変更、未使用語の削除、統合/改名を構造化した操作と一行理由で指定する。
    planは準備済みDBの読取snapshotから全対象を列挙し、件数・最大20例・blocker集計・receiptを返す。
    原文全件を応答へ含めず、DB初期化・修復・同期を行わない。applyは正本UID・指定revision・
    正本原文hash・全snapshotをwriter lock内で再照合し、全ノート・索引・蒸留job・outbox・前後履歴を
    1 transactionで確定する。古い計画、二重実行、先行する未出力更新、保護された対象、
    最終タグ制約違反があれば全体を止める。通常本文更新・正本切替にも使用語削除guardを適用し、
    importは全ノート同期後の最終状態で検証する。本人の個別承認は求めず、必要性の意味判断はAIが行う。
    保存後の出力失敗でも`stored:true`を維持し、`pending_exports`と警告を返す。
    件数が取得不能なら`null`と警告を返す。出力は一括変更単位でまとめ、照合して再開する。
    read面の`list_tag_vocabulary_changes`と`get_tag_vocabulary_change`は読取り専用で同期せず、
    履歴要約と対象metadataを既定20・最大100件でページ化する。原文を返さず現在非参照の対象も除外する。
    履歴2表はschema12から端末ローカルdurable stateとして保持し、schema13で復元台帳を追加する。
    `plan_tag_vocabulary_rollback`はmaintenance面で元実行IDと理由から現在の全snapshotと履歴へ固定し、
    write面の`rollback_tag_vocabulary_change`は同じreceiptを再照合して全対象のbefore原文を復元する。
    元実行のafter原文・UID・タグ索引・参照属性が不一致、正本UID/revision変更、保護対象、出力待ち、
    対象外の削除語利用、復元後のタグ契約違反、二重復元は全件未反映で拒否する。
    復元前後の原文合計256 MiB・10万対象・最大20例の上限を持ち、本人への個別承認を追加しない。
    復元者・理由・時刻・件数の台帳とノート・索引・蒸留job・outboxを同時保存し、元実行履歴は保持する。
    required eventの`tag_vocabulary_rolled_back`で結果を届け、応答喪失時は元実行IDの履歴に付く
    `rollback`で確認する。出力失敗も保存済みとし、一括applyと同じ経路で再開する。
    read面の`get_tag_vocabulary_stats`は保存済み適用・復元件数、正本を含む延べノート変更数、
    最初・最後の時刻、最近10実行と現在のノート出力待ちをread-onlyで返す。全履歴原文や理由・操作JSONを
    parseせず、新しい大量ログは加えない。対象は端末ローカルの全期間で、計画・拒否・時間・意味品質は未計測。
    成功率や誤判断率を推定しない。変更・復元・履歴の専用GUI、復元自体の再rollback、別端末・fresh cloneへの復元履歴移送は
    後続範囲。詳細は[ADR-0022](adr/0022-atomic-tag-vocabulary.md)
  - `propose`はnamespace / role / authority status / scopeを必須入力にし、`update`はlegacy移行または
    authority変更時だけ同じenvelopeを受ける。`get` / search / recentは`note_uid`とauthorityを返す。
    typed relationはpathでなく26文字ULIDの`note_uid`を端点にする
  - note read / create / update / attach、degradation、KB OFF、tool errorは
    `structuredContent.conversation_events` v1へ`required=true`で返す。対応hostはこれを会話へ
    決定論的に描画し、モデルがリンク・警告を言い直すかどうかを保証点にしない
  - 起票・更新の成功応答は、対象ノートのリンク付きタイトルとnamespace/scopeを一行に揃える。
    `note_created` / `note_updated` eventにも同じ`conversation_link`と保存後のauthorityを含める。
    authorityの無い旧ノートは未設定と示し、分類を補造しない。AIは応答のリンクを使って会話へ
    報告する。ローカル絶対パスを開けないhostへの製品deep linkと、非対応hostでの表示保証は別段
  - 直接`remove`は公開しない。`prepare_remove`は対象IDと内容指紋へ固定した5分token、対象名、
    `removal_prepared` eventを返す。`commit_remove`はdestructive annotationを持ち、同じnoteと未使用token
    だけを受理する。AIは蒸留・メンテナンス方針の範囲内で個別の人間承認なしに両toolを続けて実行できる。
    期限切れ、対象差し替え、準備後変更、二重実行は削除前に拒否し、対象・理由・履歴を報告する
  - `plan_distillation`は準備済みDBをread-onlyで開き、同一snapshotの全ノートへinput hashを付けた
    mechanical-v1候補planを返す。remote pull、索引同期、schema migration、care/outbox更新を行わず、
    MCP annotationもread-only / idempotentに固定する。同じsnapshotのJSONはbyte-identicalとする
  - `plan_targeted_distillation`は、全文監査で見つけた既存AIノートのnormalize / revise / extractを
    note・operation・一行理由へ固定したtargeted-v1 planとして返す。plan IDは全DB snapshot、対象input hash、
    requested operation・理由を含み、applyは同じ入力からplanを再構成する。`keep`の任意昇格は許可しない
  - 完了initiativeは`plan_initiative_closure` / `apply_initiative_closure` /
    `rollback_initiative_closure`の専用waveで扱う。対象はAI管理のactive canonical initiativesだけに限定し、
    note UID・全DB snapshot・input hash・理由を再照合してauthority statusだけをactiveからhistoricalへ変える。
    複数対象は1 transactionで処理し、stale plan・改ざん・二重実行・部分成功と後続変更後のrollbackを拒否する
  - `audit_distillation`は自己digestを再照合した任意の前回checkpointと現在planを`note_uid`（legacyはnote ID）で比較し、
    追加・変更・削除・移動と、non-keep／risk候補を`depends_on`の双方向閉包へ広げたworksetを返す。
    baseline無しは全件監査。受入gateは全plan、unresolved・risk、pending Markdown export、Storage Contract、
    local Gitの未backup commitを検査する。read-only / idempotentで、remote pull・network I/Oを行わない
  - Legacy Artifactの物理再配置はMCPだけで完結させる。`plan_legacy_artifact_promotions`は
    1 Artifact単位のdeterministic read-only planを返し、applyはmanifest・bytes・ref・aliasを
    再照合してLFS upload成功後だけManagedへ切り替える。rollbackはapply直後の対象固定resultだけを
    受理する。どの経路も旧パス・pointer・LFS objectを削除せず、VaultやCLIへの迂回を要求しない
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
    macOSではGit本体とHTTPS helperも公式sourceから同梱する。配布内の欠損はコアが拒否し、
    LFSからの再帰Git呼出しにも同じ配置を使う。配布候補の生成・署名・公証検査は
    [macos-release.md](macos-release.md)、同梱sourceの固定は[bundled-git.md](bundled-git.md)を参照。
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
- **FR-C7 お手入れ(ライフサイクルの自動運転)— 2026-08-20 増分audit gate段まで実装**:
  authorityとtyped relationから、正本更新・記録抽出・proposal統合・legacy未解決・description正規化の
  候補をsnapshot固定で列挙する。executor v1は候補ノートを全文取得したAIから構造化targetを受け、
  plan schema/profile/ID、snapshot、全input hashとoperationを同じwrite transactionで再照合する。
  既存authority付きAIノートのnormalize、active canonical revise、record lineage extractを全件成功または0件で
  実行し、request hashによる二重実行拒否、実行前後document監査、後続変更前のatomic rollbackを備える。
  read-only auditは前回checkpointからの増分worksetを作る一方、受入判定を現在の全planとStorage Contract、
  Markdown outbox、local Git backupへかけ、中断後の再開コスト削減と全体drift検出を両立する。
  record本文・UID・authorityは不変とし、create、semantic merge、atomic supersede、新規extract／split、
  legacy backfill、別端末自動rollbackは次段。planとexecutionは人間の承認キューを作らない。方針内のAI管理ノートは
  update・atomic supersede・対象固定型二段階削除で自律メンテナンスし、ユーザーへwaveごとの承認作業を
  戻さない。legacy `origin: human`は互換読み取り専用の所有境界を維持する。リンク切れ・契約違反など
  自動修復できない劣化は、該当なしへ潰さず結果とともに報告する

- **FR-C9 会話の配信・起票観測(R1)**: Vault・同期から独立した端末ローカル台帳で、
  hookの出力準備・stdout完了・stdout失敗、フィルター理由・失敗段階と、
  MCP `propose` / `update`の応答生成結果(success / error)を区別する。
  出力統計・段階別所要時間・保守的な予算の根拠を記録し、hostの受信やモデル利用を推定しない。
  errorはノート未保存の証明ではなく、台帳障害で元の検索・書込結果を失敗へ変えない。
  KB OFF・ON未確認・未知clientでは台帳I/Oを行わず、本文・query・タイトル・pathを保存しない。
  session等のIDはhash化し、IDの無い観測は日次集計として実session数と分ける。
  保持期間は90日で、追記時に古い観測を整理する。
  `kb sessions --days 14 [--workspace-id <opaque ID>]`は1〜90日のread-only JSON集計を返し、
  Vaultを開かず、DB不在でも作成しない。省略時は全workspaceと未帰属分を含み、
  workspace指定時も未帰属分を別枠に残す。
  既存の書込み前拒否を`WriteRejection`の固定codeとしてMCPへ返し、台帳にも保持する。
  診断文で分類せず、codeの無いerrorは未分類とする。新しい本文・title拒否条件は追加しない。
  schema v1は新版の最初の追記で既存payloadを保ったままv2へ移行する。集計はv1/v2をread-onlyで扱い、
  旧binaryのv2拒否による観測欠落を避けるため、更新後は全MCP接続を再接続する。
  接続時にsurfaceごとのVault名・永続IDを固定し、hookと通常MCPで照合する。
  不一致は`vault_mismatch`、破損・確認不能は`workspace_unverified`で停止し、本文を返さない。
  未設定の通常MCPだけは未検証を明示して互換を保ち、hookは本文を配信しない。
  OFF・initialize・tools/listでは接続先設定もVaultも読まない。
  Codexの版による予算拡張は、実稼働hostの版と対応する仕様が確認できるまで行わない。
  文脈detector・gate・自動起票triggerは後続段階とする。
  保証と集計の定義は[ADR-0018](adr/0018-session-observation-ledger.md)。

### 管理アプリ(Tauri)

- **操作の文言（2026-09-08 本人指定）**: アイコンだけで機能が明らかな操作は、基本的に
  表示文言を置かず、必要な説明はツールチップで示す。実装では読み上げ名を保ち、アイコンだけで
  意味が伝わらない操作には文言を残す。
- 表示中のホーム・提案・ノート・検索・ファイル等は15秒ごとに自動更新し、アプリへ
  戻ったときも再取得する。前面表示中はノートの変更番号を1秒ごとに確認し、変更を検知したら
  表示中の情報を読み直す。先行取得に重なった要求は、その完了後に1回実行する。
  重い保守とノート数履歴の記録は60秒ごとに分ける。
  ローカル情報はオフラインでも更新し、非表示・未購読の画面では定期取得を止める。
  再取得で入力中の提案の採否理由を消さず、確認後に内容が変わった採否は拒否する。
- **FR-A1 オンボーディング**: 初回起動で最初の vault を自動作成し、そのままノートが書ける
  (段0の実体)。AI アプリ接続(まず Claude Desktop / Claude Code)・ローカル埋め込みの導入は
  **任意の「繋ぐ」ボタン**としてアプリが代行。スキップしても全機能の段0が成立
- **FR-A2 ホーム**: vault 一覧+健全性(ノート数・索引状態・劣化警告・未バックアップ)
  - 最終取得時刻・取得中の状態・更新ボタンを示す。取得に失敗して前回の件数を残す場合は
    古い値であることを明示する。変更検知の失敗も隠さず、15秒の再取得を継続する。
  - 最近のノート一覧はホームに置かず、グローバル検索Modalで確認する。
  - 提案件数は提案一覧の「すべて」と同じ全状態のチケット数を表示し、一覧へ遷移できる。
    ノートのお手入れ候補数を混ぜず、取得中・取得失敗を0件として表示しない。
  - 配信・起票の観測は`home_observation_health`から別取得し、Home表示中に15秒ごとに更新する。
    選択中workspaceの直近14日の出力・起票・更新・拒否件数と、最大90日内の最終起票成功応答を示す。
    未帰属分は別枠に残し、別workspaceの件数を混ぜない。
    `available` / `no_observations` / `disabled` / `unavailable`を分け、観測が無いことや取得失敗を
    正常な0件として表示しない。全体OFFまたは両AI familyがOFFなら台帳へ触れず、部分OFFでは
    過去の記録と現在のOFF設定を併記する。受信率・起票率・起票漏れの判定は表示しない。
  - 利用実績の日別推移は`home_observation_trend`で別取得し、端末の暦日で今日を含む14日を示す。
    検索出力完了・起票成功応答・更新成功応答・エラーを切り替え、日別の数値も確認できる。
    今日の値は取得時点まで。夏時間を含む日付境界で区切り、未帰属・別workspaceは除外する。
    記録内の0件と、期間全体の観測なし・OFF・読取失敗を区別する。応答件数を保存ノート数や
    AIの利用効果へ読み替えない。エラーは出力失敗・hook処理エラー・起票/更新エラーの合計。
    全体・Claude・GPTを切り替え、期間合計・グラフ・日別一覧を同じ対象で絞り込む。
    記録済みの接続元に基づき、Claude Code/Claude DesktopをClaude、Codex/ChatGPTをGPTに分類する。
    全体は検証済みの評価用記録も含める。モデル名やノートの作成者から推測しない。
    観測なしは絞込み後の対象で判定し、一部OFFでも保存済みの履歴を参照できる。
    全体OFF・両family OFF時の読取停止と、未帰属・別workspaceの除外は共通とする。
  - ノート数の推移に向け、アプリの保守処理後に現在の総数とdeprecated数を端末内へ記録する。
    日付は観測時の端末日付で固定し、workspaceごと・日ごとの最後の観測を保持する。
    件数の定義は既存タイルと同じ総数-deprecated数。記録前・未稼働日を0や前日の値で補完しない。
    履歴は索引やGit同期から独立し、記録失敗は`note_count_history`の劣化として示す。
    ホームに今日を含む直近14暦日の折れ線グラフと日別一覧を表示する。欠測で線を切り、
    実測の0件を未記録と区別する。最新の観測値には記録日を添え、期間合計や現在値と扱わない。
    履歴なし・読取失敗は別の状態で表示する。AI連携のON/OFFに依存せず、履歴取得は
    read-onlyでDBの作成や記録を行わない。ノート変更ごとの即時記録は後続の対象とする。
    保存範囲と日付の定義は[ADR-0020](adr/0020-note-count-history.md)。
- **タグ一覧（2026-09-08 本人指定）**: サイドバーから開く閲覧専用の一覧に、タグ名・AIが語彙の正本で
  定めた役割・ノート数・設定先ノートを見るアイコンボタンを置く。ノート数とボタンは別セルにし、
  件数の桁数によってボタンの大きさや位置が変わらない固定サイズの操作欄にする。
  名前と役割で絞り込み、未使用の登録語も表示する。
  件数は通常参照可能かつ非退役のノートを数え、設定先ノート一覧と同じ母集団にする。
  正本未指定・欠損・参照不能や未登録の現用語、役割未設定を区別し、説明を推測しない。
  ノートは100件ずつ取得し、ファイルと同寸法のModalで左の一覧と右の本文プレビューを読む。
  小幅画面では一覧とプレビューを切り替える。取得中・正常な0件・追加取得失敗・更新失敗を分け、
  再試行とキーボード操作に対応する。タグや役割を編集・削除する操作は置かない。
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
  前出し・Stopフックも同じ設定に従う。strict modeでは生ファイルはON/OFFにかかわらず、Codexの管理
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
  CLI直検索と、MCP未使用をtranscriptで推測するStop hookは廃止する。ローカル開発では、完全保護が正常な
  状態から管理者認証を経た場合に限りCodexだけを`:danger-full-access`かつ`approval_policy=never`へ
  切り替える開発高速モードを提供する。
  この間はCodexのkb-app MCPと自動retrievalをfail-closedにし、Claude Codeのstrict guardは維持する。
  設定UIとCLIは状態を明示し、検証・テスト・リリース前はstrict modeへの復旧を要求する。CLIの
  `kb settings dev-mode release-check`はstrict modeでなければ失敗する。
- **FR-A10 常駐と自動起動(2026-09-04 本人決定)**: 管理アプリは常駐し、窓を閉じても
  プロセスを残す。タスクトレイ／メニューバーのアイコンを左クリックすると画面が出て、
  右クリックのメニューに「開く」と「終了」を置く。**終了の入口はこのメニューだけ**にし、
  閉じるボタンでは終わらせない(閉じるたびにcold startへ戻ると常駐の意味が無くなるため)。
  macOSはDockアイコンを残し、窓を隠している間のDockクリックも同じ復帰経路へ流す。
  ログイン時の自動起動は`--hidden`で窓を出さずtrayだけを出す。登録の正本はOSのログイン項目で、
  アプリは初回起動の1回だけ既定として有効化し、以後はアプリ内switchとOS設定のどちらの操作にも従う
  (毎回書き直してユーザーが外した判断を打ち消さない)。実行ファイルが移動したときだけ登録を
  書き直し、pathがずれた登録を「有効」と表示しない。対応OSはmacOSがLaunchAgent、Linuxが
  autostart desktop entry、WindowsがHKCUのRunキー。登録できないOSではswitchを操作不能にし、
  できない事実を画面に出す。詳細は [ADR-0017](adr/0017-background-residency.md)

- **FR-A11 対応AIの共通運用診断**: 「繋ぐ」でCodex・Claude Code・Claude Desktopの
  read/write/maintenance登録と選択中KBの固定を検査・修復し、再接続と再検査へ進める。
  個人用の指示ファイルの手編集を必須にしない。登録の一致・管理保護・過去のhook出力・
  host受信・操作応答は別々に表示し、未確認を対応済みへ読み替えない。共通契約と案内のhash、
  workspace ID、語彙正本UID/指定revision/document hashでPC・AI間の版を比較できる。
  破損設定、管理ポリシーの競合、別scopeの上書きを自動変更せず、無関係な設定を保持する。
  対応経路と受入の境界は [shared-client-rules.md](shared-client-rules.md) にまとめる。

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
- ~~タグ自動提案の挙動~~ → **AIが判断して適用する(2026-09-08本人指定)**:
  語彙の追加・統合・削除も個別承認を必須にせず、既存語優先・増殖抑制と本人の明示訂正に従う
- vault コミットの著者情報(現状はグローバル git 設定/GECOS 由来を継承 — 随時 push で
  private バックアップ先へ私用メール・ホスト名が乗る。旧 KB で実害記録のある事故型。
  app 固定 actor に寄せるか、デバイス識別として活かして GitHub 側の秘匿設定で守るか)
- Claude Code 接続(v0.3 後半): 接続代行は Desktop のみ実装済み。Code 側は
  「vault ディレクトリに .mcp.json を同梱し、ランチャの『Claude Code で開く』で
  そこを起動ディレクトリにする」方式(旧トラック external-app-design の設計を流用)
