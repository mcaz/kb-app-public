//! fail-open で返す部分失敗の型。
//!
//! 空配列だけでは「本当に0件」と「取得に失敗して0件」が区別できない。データと同じ
//! 応答へこの列挙型を載せ、GUI は `code` で翻訳し、MCP は `code` を明示する。

/// データ本体を返し続けられる部分失敗。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Degradation {
    RemoteSync {
        detail: String,
    },
    MarkdownExport {
        detail: String,
    },
    IndexSync {
        detail: String,
    },
    /// open時の自己修復で派生索引を再構築した(今回だけのrecovery notice)。
    IndexRecovered {
        artifact: String,
        detail: String,
    },
    /// 派生索引の修復に失敗した(検索はfallback継続、次回openで再試行)。
    IndexRepair {
        artifact: String,
        detail: String,
    },
    /// governance台帳を修復できず、note書込をfail-closedで停止中。
    GovernanceWriteBlocked {
        detail: String,
    },
    /// versioned派生artifactが利用不可(dirty・format不一致・retired等)で、
    /// baseline検索へfallbackした(derived_lifecycle::check_availability)。
    ArtifactNotReady {
        artifact: String,
        detail: String,
    },
    IndexMetadata {
        note: String,
        detail: String,
    },
    IndexRead {
        note: String,
        detail: String,
    },
    IndexParse {
        note: String,
        detail: String,
    },
    EmbeddingIndexPending {
        remaining: usize,
    },
    EmbeddingIndex {
        detail: String,
    },
    MainSearch {
        detail: String,
    },
    AnchorSearch {
        detail: String,
    },
    DiversityRanking {
        detail: String,
    },
    SemanticSearch {
        detail: String,
    },
    RescueSearch {
        detail: String,
    },
    RelatedNotes {
        detail: String,
    },
    ContextRetrieval {
        detail: String,
    },
    SimilarNotes {
        detail: String,
    },
    CurrentNoteContext {
        detail: String,
    },
    CareDetection {
        detail: String,
    },
    CareList {
        detail: String,
    },
    TagCounts {
        detail: String,
    },
    GraphNodes {
        detail: String,
    },
    GraphEdges {
        detail: String,
    },
}

impl Degradation {
    /// MCP 等、型を持たない表示面でも安定した識別子を残す。
    pub fn code(&self) -> &'static str {
        match self {
            Self::RemoteSync { .. } => "remote_sync",
            Self::MarkdownExport { .. } => "markdown_export",
            Self::IndexSync { .. } => "index_sync",
            Self::IndexRecovered { .. } => "index_recovered",
            Self::IndexRepair { .. } => "index_repair",
            Self::GovernanceWriteBlocked { .. } => "governance_write_blocked",
            Self::ArtifactNotReady { .. } => "artifact_not_ready",
            Self::IndexMetadata { .. } => "index_metadata",
            Self::IndexRead { .. } => "index_read",
            Self::IndexParse { .. } => "index_parse",
            Self::EmbeddingIndexPending { .. } => "embedding_index_pending",
            Self::EmbeddingIndex { .. } => "embedding_index",
            Self::MainSearch { .. } => "main_search",
            Self::AnchorSearch { .. } => "anchor_search",
            Self::DiversityRanking { .. } => "diversity_ranking",
            Self::SemanticSearch { .. } => "semantic_search",
            Self::RescueSearch { .. } => "rescue_search",
            Self::RelatedNotes { .. } => "related_notes",
            Self::ContextRetrieval { .. } => "context_retrieval",
            Self::SimilarNotes { .. } => "similar_notes",
            Self::CurrentNoteContext { .. } => "current_note_context",
            Self::CareDetection { .. } => "care_detection",
            Self::CareList { .. } => "care_list",
            Self::TagCounts { .. } => "tag_counts",
            Self::GraphNodes { .. } => "graph_nodes",
            Self::GraphEdges { .. } => "graph_edges",
        }
    }
}

impl std::fmt::Display for Degradation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RemoteSync { detail } => write!(f, "同期に失敗: {detail}"),
            Self::MarkdownExport { detail } => {
                write!(f, "Markdownバックアップの更新に失敗: {detail}")
            }
            Self::IndexSync { detail } => write!(f, "索引の更新に失敗: {detail}"),
            Self::IndexRecovered { artifact, detail } => {
                write!(f, "派生索引 {artifact} を自動修復した(原因: {detail})")
            }
            Self::IndexRepair { artifact, detail } => {
                write!(f, "派生索引 {artifact} を修復できない: {detail}")
            }
            Self::GovernanceWriteBlocked { detail } => {
                write!(f, "ノートの書込を停止中: {detail}")
            }
            Self::ArtifactNotReady { artifact, detail } => {
                write!(
                    f,
                    "派生artifact {artifact} が利用できず基本検索へ切替: {detail}"
                )
            }
            Self::IndexMetadata { note, detail } => {
                write!(f, "{note} の更新日時を取得できない: {detail}")
            }
            Self::IndexRead { note, detail } => write!(f, "{note} を読めない: {detail}"),
            Self::IndexParse { note, detail } => write!(f, "{note} の形式が不正: {detail}"),
            Self::EmbeddingIndexPending { remaining } => {
                write!(f, "かしこい検索の索引が追い付き中(残り {remaining} 件)")
            }
            Self::EmbeddingIndex { detail } => {
                write!(f, "かしこい検索の索引更新に失敗: {detail}")
            }
            Self::MainSearch { detail } => write!(f, "主索引が利用できない: {detail}"),
            Self::AnchorSearch { detail } => {
                write!(f, "リンク文言索引が利用できない: {detail}")
            }
            Self::DiversityRanking { detail } => {
                write!(f, "検索結果の多様化が利用できない: {detail}")
            }
            Self::SemanticSearch { detail } => {
                write!(f, "かしこい検索が一時停止: {detail}")
            }
            Self::RescueSearch { detail } => {
                write!(f, "レスキュー索引が利用できない: {detail}")
            }
            Self::RelatedNotes { detail } => write!(f, "つながりを取得できない: {detail}"),
            Self::ContextRetrieval { detail } => {
                write!(f, "関連本文の連鎖取得に失敗: {detail}")
            }
            Self::SimilarNotes { detail } => write!(f, "近いノートを取得できない: {detail}"),
            Self::CurrentNoteContext { detail } => {
                write!(f, "現在のノートを記録できない: {detail}")
            }
            Self::CareDetection { detail } => write!(f, "お手入れ検知に失敗: {detail}"),
            Self::CareList { detail } => write!(f, "お手入れ一覧を取得できない: {detail}"),
            Self::TagCounts { detail } => write!(f, "タグ一覧を取得できない: {detail}"),
            Self::GraphNodes { detail } => write!(f, "グラフのノードを一部読めない: {detail}"),
            Self::GraphEdges { detail } => write!(f, "グラフの辺を一部読めない: {detail}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization_keeps_a_stable_code() {
        let items = [
            Degradation::RemoteSync { detail: "x".into() },
            Degradation::MarkdownExport { detail: "x".into() },
            Degradation::IndexSync { detail: "x".into() },
            Degradation::IndexRecovered {
                artifact: "fts_main".into(),
                detail: "x".into(),
            },
            Degradation::IndexRepair {
                artifact: "fts_tri".into(),
                detail: "x".into(),
            },
            Degradation::GovernanceWriteBlocked { detail: "x".into() },
            Degradation::ArtifactNotReady {
                artifact: "fts_entry_v1".into(),
                detail: "x".into(),
            },
            Degradation::IndexMetadata {
                note: "notes/a".into(),
                detail: "x".into(),
            },
            Degradation::IndexRead {
                note: "notes/a".into(),
                detail: "x".into(),
            },
            Degradation::IndexParse {
                note: "notes/a".into(),
                detail: "x".into(),
            },
            Degradation::EmbeddingIndexPending { remaining: 3 },
            Degradation::EmbeddingIndex { detail: "x".into() },
            Degradation::MainSearch { detail: "x".into() },
            Degradation::AnchorSearch { detail: "x".into() },
            Degradation::DiversityRanking { detail: "x".into() },
            Degradation::SemanticSearch { detail: "x".into() },
            Degradation::RescueSearch { detail: "x".into() },
            Degradation::RelatedNotes { detail: "x".into() },
            Degradation::ContextRetrieval { detail: "x".into() },
            Degradation::SimilarNotes { detail: "x".into() },
            Degradation::CurrentNoteContext { detail: "x".into() },
            Degradation::CareDetection { detail: "x".into() },
            Degradation::CareList { detail: "x".into() },
            Degradation::TagCounts { detail: "x".into() },
            Degradation::GraphNodes { detail: "x".into() },
            Degradation::GraphEdges { detail: "x".into() },
        ];
        for item in items {
            let value = serde_json::to_value(&item).unwrap();
            assert_eq!(value["code"], item.code());
        }
    }
}
