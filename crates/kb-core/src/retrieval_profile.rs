//! 配信 profile — 呼び出し用途ごとに検索・候補展開・本文縮約の予算と出力形を 1 箇所で決める。
//!
//! 同じ `search` tool でも、管理 hook が発話ごとに自動で引く経路と、host 側で model が
//! 明示的に呼ぶ経路とでは、許容できる本文量と求める精度が違う。query の意味で順位を変える
//! `QueryIntent`(search.rs)とは軸が別で、profile は process 起動時に固定され query を見ない。
//! MCP tool の入力 schema には出さない(hook 子 process と host の区別は起動引数で行う)。
//!
//! 初期値と gate は docs/claude-led-retrieval-discussion.md §8.4 / §8.8 の実験契約に従う。
//! 実測は docs/retrieval-profiles.md。

use anyhow::Result;

use crate::retrieval::{AUTO_SEED_LIMIT, RetrievalOptions};

/// host 側 MCP `search` の既定件数。tool 引数 `limit` を省略したときの値で、profile 分離前の
/// 既定(8)を変えない。
pub const HOST_SEARCH_LIMIT: usize = 8;
/// field ranking の再順位付け対象を最終件数の何倍まで広げるか。8 倍は 10k fixture でも
/// 最大 40 行に留まり、本文反復だけが強い候補の外から title 一致を回収できる実測上の最小余裕。
pub const FIELD_RANKING_CANDIDATE_MULTIPLIER: usize = 8;

#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalProfile {
    /// 管理 hook の子 process(発話ごとの自動 retrieval)。契約 8 の数値そのもの。
    SessionAuto,
    /// host 側 MCP(read / all 面)で model が明示的に `search` を呼ぶ経路。
    SessionExplicit,
    /// routine 由来の定型 prompt 向け変種。この round では benchmark 上でだけ測り、
    /// hook からの自動選択と card 注入は実装しない(実験契約 §8.12)。
    RoutineAuto,
    /// `kb eval retrieval` / benchmark の linked_v1。`SessionAuto` と同じ計画を返す
    /// (評価値が本番 hook の挙動を指すことを型と test で固定する)。
    Evaluation,
}

impl RetrievalProfile {
    pub const ALL: [Self; 4] = [
        Self::SessionAuto,
        Self::SessionExplicit,
        Self::RoutineAuto,
        Self::Evaluation,
    ];

    /// 起動引数の既定が無いときの host 側 profile。hook 子 process は host と同じ read 面を
    /// 使うので surface では区別できず、hook_mode が `--retrieval-profile session-auto` を
    /// 明示する。
    pub const fn host_default() -> Self {
        Self::SessionExplicit
    }

    /// report・initialize 表示・JSON で使う名前(serde の名前と同じ)。
    pub const fn label(self) -> &'static str {
        match self {
            Self::SessionAuto => "session_auto",
            Self::SessionExplicit => "session_explicit",
            Self::RoutineAuto => "routine_auto",
            Self::Evaluation => "evaluation",
        }
    }

    /// `--retrieval-profile` の値。snake_case / kebab-case のどちらも受け、未知値は拒否する。
    /// 黙って既定へ落とすと profile 分離の計測が壊れるので fail-closed にする。
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().replace('-', "_").as_str() {
            "session_auto" => Ok(Self::SessionAuto),
            "session_explicit" => Ok(Self::SessionExplicit),
            "routine_auto" => Ok(Self::RoutineAuto),
            "evaluation" => Ok(Self::Evaluation),
            _ => anyhow::bail!(
                "unknown retrieval profile: {value} (expected session-auto, session-explicit, routine-auto, or evaluation)"
            ),
        }
    }

    /// この profile の計画。値は実験契約 §8.4 の表そのもの(調整は gate の範囲内で 1 回まで)。
    pub fn plan(self) -> RetrievalPlan {
        match self {
            Self::SessionAuto | Self::Evaluation => RetrievalPlan {
                search: SearchPolicy::any_terms(AUTO_SEED_LIMIT),
                retrieval: RetrievalOptions::default(),
                output: OutputShape::Body,
            },
            Self::SessionExplicit => RetrievalPlan {
                search: SearchPolicy::exact(HOST_SEARCH_LIMIT),
                retrieval: RetrievalOptions {
                    seed_limit: 5,
                    max_depth: 1,
                    candidate_limit: 20,
                    document_limit: 5,
                    estimated_token_budget: 6_000,
                    // 契約 9 の supports / derived_from は「根拠 → 正本」の被リンク方向が
                    // 自然で、切ると根拠 record を候補からも落とす。depth 1 に絞るだけにする。
                    include_incoming: true,
                    passage: PassagePolicy {
                        trigger_tokens: 4_000,
                        max_bytes: PassagePolicy::default().max_bytes,
                        document_limit: 2,
                        document_token_budget: 2_400,
                    },
                },
                output: OutputShape::Body,
            },
            Self::RoutineAuto => RetrievalPlan {
                search: SearchPolicy::any_terms(AUTO_SEED_LIMIT),
                retrieval: RetrievalOptions {
                    seed_limit: 3,
                    max_depth: 1,
                    candidate_limit: 20,
                    // 変種 B(本文 ≤3)。変種 A は同じ計画の document_limit を 0 にして測る。
                    document_limit: 3,
                    estimated_token_budget: 3_000,
                    include_incoming: true,
                    passage: PassagePolicy {
                        trigger_tokens: 2_000,
                        max_bytes: PassagePolicy::default().max_bytes,
                        document_limit: 1,
                        document_token_budget: 1_200,
                    },
                },
                output: OutputShape::CardLite,
            },
        }
    }
}

/// 実験用の rerank 軸。統合 core が実行できるのは `off` だけで、`off` 以外は
/// `retrieval_eval::EvaluationPlan::ensure_rerank_off_for_core` が明確なエラーにする
/// (ContextCard rerank の実装は評価用ブランチ — R4 I-5)。enum は比較表の語彙として残す。
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RerankMode {
    Off,
    On,
}

impl RerankMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
        }
    }
}

/// profile が決める計画。検索 → 候補展開・本文選択 → 出力形の順に使う。
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub struct RetrievalPlan {
    pub search: SearchPolicy,
    pub retrieval: RetrievalOptions,
    pub output: OutputShape,
}

/// 検索(search.rs)の予算と経路の有効化。`search` / `search_mode` はこの値の wrapper。
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub struct SearchPolicy {
    /// 語を OR 結合する(hook の文まるごと前出し用)。false は AND。
    pub any_terms: bool,
    /// 最終件数。
    pub limit: usize,
    /// field ranking の再順位付け対象を `limit` の何倍まで広げるか。
    pub candidate_multiplier: usize,
    /// 部分語・未知語形を拾う rescue 経路(trigram / LIKE)。
    pub rescue: bool,
    /// 意味検索(段 1)。モデル未導入なら policy に関わらず全文のみ(段 0 の正常形)。
    pub semantic: bool,
    /// 類似候補の cluster 化(重複排除・多様化)。
    pub diversify: bool,
}

impl SearchPolicy {
    /// 語を AND 結合する明示検索。経路はすべて有効。
    pub const fn exact(limit: usize) -> Self {
        Self {
            any_terms: false,
            limit,
            candidate_multiplier: FIELD_RANKING_CANDIDATE_MULTIPLIER,
            rescue: true,
            semantic: true,
            diversify: true,
        }
    }

    /// 語を OR 結合する前出し検索。経路はすべて有効。
    pub const fn any_terms(limit: usize) -> Self {
        Self {
            any_terms: true,
            ..Self::exact(limit)
        }
    }

    /// field ranking の再順位付け対象件数。倍率が 0 でも最終件数を下回らせない。
    pub fn candidate_limit(&self) -> usize {
        self.limit
            .saturating_mul(self.candidate_multiplier)
            .max(self.limit)
    }
}

/// 長文ノートの passage 縮約(retrieval.rs)の予算。既定値は passage ranking 導入時の
/// 実測値(docs/retrieval-passage-ranking.md)。
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub struct PassagePolicy {
    /// この推定 token を超える本文だけを縮約対象にする。短いノートは全文のまま。
    pub trigger_tokens: usize,
    /// 巨大 section・見出しの無い本文を分割する上限 byte。
    pub max_bytes: usize,
    /// 1 文書から返す passage の最大数。
    pub document_limit: usize,
    /// 1 文書の passage 合計の推定 token 予算。
    pub document_token_budget: usize,
}

impl Default for PassagePolicy {
    fn default() -> Self {
        Self {
            trigger_tokens: 4_000,
            max_bytes: 2_400,
            document_limit: 3,
            document_token_budget: 3_600,
        }
    }
}

/// 応答の主産物。`Body` は選択本文、`CardLite` は authority 付き候補一覧
/// (`RetrievalCandidate` の authority 列)を主とし、本文は `document_limit` 件まで。
/// この round では応答の組み立てを変えず、benchmark の変種ラベルと表示にだけ使う
/// (routine の routing・card 注入は実験契約 §8.12 の範囲外)。
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputShape {
    Body,
    CardLite,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 実験契約 §8.4: `session_auto` = `evaluation` を型で同一化し、値は profile 分離前の
    /// 既定(契約 8 の hook 数値 + passage ranking の実測値)と一致させる。
    #[test]
    fn session_auto_and_evaluation_share_the_pre_profile_defaults() {
        let plan = RetrievalProfile::SessionAuto.plan();
        assert_eq!(plan, RetrievalProfile::Evaluation.plan());
        assert_eq!(plan.search, SearchPolicy::any_terms(5));
        assert_eq!(plan.retrieval, RetrievalOptions::default());
        assert_eq!(plan.output, OutputShape::Body);

        let retrieval = plan.retrieval;
        assert_eq!(retrieval.seed_limit, 5);
        assert_eq!(retrieval.max_depth, 2);
        assert_eq!(retrieval.candidate_limit, 50);
        assert_eq!(retrieval.document_limit, 10);
        assert_eq!(retrieval.estimated_token_budget, 10_000);
        assert!(retrieval.include_incoming);
        assert_eq!(
            retrieval.passage,
            PassagePolicy {
                trigger_tokens: 4_000,
                max_bytes: 2_400,
                document_limit: 3,
                document_token_budget: 3_600,
            }
        );
        assert_eq!(plan.search.candidate_limit(), 40);
        assert!(plan.search.rescue && plan.search.semantic && plan.search.diversify);
    }

    #[test]
    fn session_explicit_narrows_budgets_but_keeps_incoming_links() {
        let plan = RetrievalProfile::SessionExplicit.plan();
        assert_eq!(plan.search, SearchPolicy::exact(8));
        assert_eq!(plan.output, OutputShape::Body);
        let retrieval = plan.retrieval;
        assert_eq!(retrieval.seed_limit, 5);
        assert_eq!(retrieval.max_depth, 1);
        assert_eq!(retrieval.candidate_limit, 20);
        assert_eq!(retrieval.document_limit, 5);
        assert_eq!(retrieval.estimated_token_budget, 6_000);
        assert!(retrieval.include_incoming);
        assert_eq!(retrieval.passage.trigger_tokens, 4_000);
        assert_eq!(retrieval.passage.document_limit, 2);
        assert_eq!(retrieval.passage.document_token_budget, 2_400);
        assert_eq!(retrieval.passage.max_bytes, 2_400);
    }

    #[test]
    fn routine_auto_is_a_benchmark_variant_with_card_lite_output() {
        let plan = RetrievalProfile::RoutineAuto.plan();
        assert_eq!(plan.search, SearchPolicy::any_terms(5));
        assert_eq!(plan.output, OutputShape::CardLite);
        let retrieval = plan.retrieval;
        assert_eq!(retrieval.seed_limit, 3);
        assert_eq!(retrieval.max_depth, 1);
        assert_eq!(retrieval.candidate_limit, 20);
        assert_eq!(retrieval.document_limit, 3);
        assert_eq!(retrieval.estimated_token_budget, 3_000);
        assert!(retrieval.include_incoming);
        assert_eq!(retrieval.passage.trigger_tokens, 2_000);
        assert_eq!(retrieval.passage.document_limit, 1);
        assert_eq!(retrieval.passage.document_token_budget, 1_200);

        let variant_a = RetrievalOptions {
            document_limit: 0,
            ..retrieval
        };
        assert_eq!(variant_a.candidate_limit, retrieval.candidate_limit);
    }

    #[test]
    fn labels_round_trip_through_parse_and_serde() {
        for profile in RetrievalProfile::ALL {
            assert_eq!(RetrievalProfile::parse(profile.label()).unwrap(), profile);
            assert_eq!(
                RetrievalProfile::parse(&profile.label().replace('_', "-")).unwrap(),
                profile
            );
            assert_eq!(
                serde_json::to_value(profile).unwrap(),
                serde_json::json!(profile.label())
            );
            assert_eq!(
                serde_json::from_value::<RetrievalProfile>(serde_json::json!(profile.label()))
                    .unwrap(),
                profile
            );
        }
        // fixture / suite の JSON は snake_case だけを受ける(kebab-case は CLI の parse 側)。
        assert!(serde_json::from_str::<RetrievalProfile>("\"session-auto\"").is_err());
        for mode in [RerankMode::Off, RerankMode::On] {
            let json = serde_json::to_string(&mode).unwrap();
            assert_eq!(json, format!("\"{}\"", mode.label()));
            assert_eq!(serde_json::from_str::<RerankMode>(&json).unwrap(), mode);
        }
        assert_eq!(
            RetrievalProfile::host_default(),
            RetrievalProfile::SessionExplicit
        );
    }

    #[test]
    fn unknown_profile_values_are_rejected_instead_of_defaulting() {
        for value in ["", "auto", "session", "SESSION_AUTO ", "gui_browse"] {
            let error = RetrievalProfile::parse(value).unwrap_err().to_string();
            assert!(
                error.contains("unknown retrieval profile"),
                "{value}: {error}"
            );
        }
    }

    #[test]
    fn search_policy_candidate_limit_never_drops_below_the_final_limit() {
        let mut policy = SearchPolicy::exact(8);
        assert_eq!(policy.candidate_limit(), 64);
        policy.candidate_multiplier = 0;
        assert_eq!(policy.candidate_limit(), 8);
        policy.limit = usize::MAX;
        policy.candidate_multiplier = 8;
        assert_eq!(policy.candidate_limit(), usize::MAX);
    }
}
