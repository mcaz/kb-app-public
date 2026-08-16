//! fail-open で返す部分失敗の型。
//!
//! 空配列だけでは「本当に0件」と「取得に失敗して0件」が区別できない。データと同じ
//! 応答へこの列挙型を載せ、GUI は `code` で翻訳し、MCP は `code` を明示する。

/// データ本体を返し続けられる部分失敗。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Degradation {
    RemoteSync { detail: String },
    IndexSync { detail: String },
    EmbeddingIndexPending { remaining: usize },
    EmbeddingIndex { detail: String },
    MainSearch { detail: String },
    SemanticSearch { detail: String },
    RescueSearch { detail: String },
    RelatedNotes { detail: String },
    SimilarNotes { detail: String },
    CurrentNoteContext { detail: String },
    CareDetection { detail: String },
    CareList { detail: String },
    TagCounts { detail: String },
}

impl Degradation {
    /// MCP 等、型を持たない表示面でも安定した識別子を残す。
    pub fn code(&self) -> &'static str {
        match self {
            Self::RemoteSync { .. } => "remote_sync",
            Self::IndexSync { .. } => "index_sync",
            Self::EmbeddingIndexPending { .. } => "embedding_index_pending",
            Self::EmbeddingIndex { .. } => "embedding_index",
            Self::MainSearch { .. } => "main_search",
            Self::SemanticSearch { .. } => "semantic_search",
            Self::RescueSearch { .. } => "rescue_search",
            Self::RelatedNotes { .. } => "related_notes",
            Self::SimilarNotes { .. } => "similar_notes",
            Self::CurrentNoteContext { .. } => "current_note_context",
            Self::CareDetection { .. } => "care_detection",
            Self::CareList { .. } => "care_list",
            Self::TagCounts { .. } => "tag_counts",
        }
    }
}

impl std::fmt::Display for Degradation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RemoteSync { detail } => write!(f, "同期に失敗: {detail}"),
            Self::IndexSync { detail } => write!(f, "索引の更新に失敗: {detail}"),
            Self::EmbeddingIndexPending { remaining } => {
                write!(f, "かしこい検索の索引が追い付き中(残り {remaining} 件)")
            }
            Self::EmbeddingIndex { detail } => {
                write!(f, "かしこい検索の索引更新に失敗: {detail}")
            }
            Self::MainSearch { detail } => write!(f, "主索引が利用できない: {detail}"),
            Self::SemanticSearch { detail } => {
                write!(f, "かしこい検索が一時停止: {detail}")
            }
            Self::RescueSearch { detail } => {
                write!(f, "レスキュー索引が利用できない: {detail}")
            }
            Self::RelatedNotes { detail } => write!(f, "つながりを取得できない: {detail}"),
            Self::SimilarNotes { detail } => write!(f, "近いノートを取得できない: {detail}"),
            Self::CurrentNoteContext { detail } => {
                write!(f, "現在のノートを記録できない: {detail}")
            }
            Self::CareDetection { detail } => write!(f, "お手入れ検知に失敗: {detail}"),
            Self::CareList { detail } => write!(f, "お手入れ一覧を取得できない: {detail}"),
            Self::TagCounts { detail } => write!(f, "タグ一覧を取得できない: {detail}"),
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
            Degradation::IndexSync { detail: "x".into() },
            Degradation::EmbeddingIndexPending { remaining: 3 },
            Degradation::EmbeddingIndex { detail: "x".into() },
            Degradation::MainSearch { detail: "x".into() },
            Degradation::SemanticSearch { detail: "x".into() },
            Degradation::RescueSearch { detail: "x".into() },
            Degradation::RelatedNotes { detail: "x".into() },
            Degradation::SimilarNotes { detail: "x".into() },
            Degradation::CurrentNoteContext { detail: "x".into() },
            Degradation::CareDetection { detail: "x".into() },
            Degradation::CareList { detail: "x".into() },
            Degradation::TagCounts { detail: "x".into() },
        ];
        for item in items {
            let value = serde_json::to_value(&item).unwrap();
            assert_eq!(value["code"], item.code());
        }
    }
}
