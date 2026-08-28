//! 配信profile(呼び出し用途別のretrieval方針)の名前と、実験用のrerank軸。
//!
//! 実験base(`docs/retrieval-experiment-base.md`)では全profileが現行の
//! `RetrievalOptions::default()`へ解決し、挙動差は無い。差を付けるのは各experiment branchの
//! 役目で、base側は比較表の軸(profile名・rerank)と`session_auto` = `evaluation`の同一性だけを
//! 型とtestで固定する。query意味の分類(`search::QueryIntent`)とは別物であり、
//! ここへ意図分類を重ねない(2026-08-28 実験契約 §5-1)。

use crate::retrieval::RetrievalOptions;

#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalProfile {
    /// 管理hookの自動注入。現行挙動そのもの
    SessionAuto,
    /// モデルが明示的に呼ぶMCP `search`
    SessionExplicit,
    /// 定期routine起動。benchmark上の変種としてだけ測る(routingは未実装)
    RoutineAuto,
    /// 評価fixture。`session_auto`と型で同一化し、比較可能性の錨にする
    Evaluation,
}

impl RetrievalProfile {
    pub const ALL: [Self; 4] = [
        Self::SessionAuto,
        Self::SessionExplicit,
        Self::RoutineAuto,
        Self::Evaluation,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::SessionAuto => "session_auto",
            Self::SessionExplicit => "session_explicit",
            Self::RoutineAuto => "routine_auto",
            Self::Evaluation => "evaluation",
        }
    }

    /// baseでは全profileが現行既定値。experiment branchはここで予算・深さ・被リンクを分ける。
    pub fn retrieval_options(self) -> RetrievalOptions {
        RetrievalOptions::default()
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// 実験契約 §8.8 G1: `session_auto`と`evaluation`は同じ定数でなければ、fixtureの結果を
    /// hookの実挙動として読めない。base時点ではどのprofileも現行既定と同一。
    #[test]
    fn session_auto_and_evaluation_resolve_to_the_same_current_options() {
        let auto = RetrievalProfile::SessionAuto.retrieval_options();
        let evaluation = RetrievalProfile::Evaluation.retrieval_options();
        let current = RetrievalOptions::default();
        for options in [auto, evaluation] {
            assert_eq!(options.seed_limit, current.seed_limit);
            assert_eq!(options.max_depth, current.max_depth);
            assert_eq!(options.candidate_limit, current.candidate_limit);
            assert_eq!(options.document_limit, current.document_limit);
            assert_eq!(
                options.estimated_token_budget,
                current.estimated_token_budget
            );
            assert_eq!(options.include_incoming, current.include_incoming);
        }
    }

    /// fixture / CLI / reportで同じsnake_case labelを使う。serdeの名前とlabel()がずれると
    /// reportのstrategy labelとfixtureのprofiles指定が食い違う。
    #[test]
    fn serde_names_match_labels() {
        for profile in RetrievalProfile::ALL {
            let json = serde_json::to_string(&profile).unwrap();
            assert_eq!(json, format!("\"{}\"", profile.label()));
            assert_eq!(
                serde_json::from_str::<RetrievalProfile>(&json).unwrap(),
                profile
            );
        }
        assert!(serde_json::from_str::<RetrievalProfile>("\"session-auto\"").is_err());
        for mode in [RerankMode::Off, RerankMode::On] {
            let json = serde_json::to_string(&mode).unwrap();
            assert_eq!(json, format!("\"{}\"", mode.label()));
            assert_eq!(serde_json::from_str::<RerankMode>(&json).unwrap(), mode);
        }
    }
}
