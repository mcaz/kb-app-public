//! AI クライアントの製品面を、モデル名や部分一致から独立して識別する。
//!
//! Claude Desktop と Claude Code、ChatGPT と Codex は同じモデル family でも
//! 利用できる能力と強制点が異なる。security / schema / retrieval delivery の判定は
//! この型へ集約し、`client` actor の曖昧な部分一致へ戻さない。

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientSurface {
    ClaudeCode,
    CodexCli,
    ClaudeDesktop,
    ChatGpt,
    RuleDeliveryEvaluation,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientFamily {
    Claude,
    Gpt,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RawVaultBoundary {
    ManagedOsSandbox,
    McpToolBoundary,
    EvaluationFixture,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreAnswerRetrieval {
    ManagedHook,
    DiscoverableTools,
    EvaluationHarness,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ClientCapabilities {
    pub client_surface: ClientSurface,
    pub client_family: ClientFamily,
    pub raw_vault_boundary: RawVaultBoundary,
    pub pre_answer_retrieval: PreAnswerRetrieval,
    pub current_note_argument_optional: bool,
    pub new_tag_write_available: bool,
    pub conversation_events_version: u8,
}

impl ClientSurface {
    /// `generated.by` と MCP 起動引数で共有する actor の先頭segmentだけを見る。
    /// model名の `claude` / `gpt` などはsurface判定へ使わない。
    pub fn from_hint(client: &str) -> Self {
        let actor = client
            .split('/')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        match actor.as_str() {
            "claude-code" => Self::ClaudeCode,
            "codex" | "codex-cli" => Self::CodexCli,
            "claude-desktop" => Self::ClaudeDesktop,
            "chatgpt" | "chatgpt-web" => Self::ChatGpt,
            "rule-delivery-eval" => Self::RuleDeliveryEvaluation,
            _ => Self::Unknown,
        }
    }

    pub fn family(self) -> ClientFamily {
        match self {
            Self::ClaudeCode | Self::ClaudeDesktop => ClientFamily::Claude,
            Self::CodexCli | Self::ChatGpt => ClientFamily::Gpt,
            Self::RuleDeliveryEvaluation | Self::Unknown => ClientFamily::Other,
        }
    }

    pub fn capabilities(self) -> ClientCapabilities {
        let (raw_vault_boundary, pre_answer_retrieval, current_note_argument_optional) = match self
        {
            Self::ClaudeCode | Self::CodexCli => (
                RawVaultBoundary::ManagedOsSandbox,
                PreAnswerRetrieval::ManagedHook,
                false,
            ),
            Self::ClaudeDesktop => (
                RawVaultBoundary::McpToolBoundary,
                PreAnswerRetrieval::DiscoverableTools,
                true,
            ),
            Self::ChatGpt => (
                RawVaultBoundary::McpToolBoundary,
                PreAnswerRetrieval::DiscoverableTools,
                false,
            ),
            Self::RuleDeliveryEvaluation => (
                RawVaultBoundary::EvaluationFixture,
                PreAnswerRetrieval::EvaluationHarness,
                false,
            ),
            Self::Unknown => (
                RawVaultBoundary::Unsupported,
                PreAnswerRetrieval::Unavailable,
                false,
            ),
        };
        ClientCapabilities {
            client_surface: self,
            client_family: self.family(),
            raw_vault_boundary,
            pre_answer_retrieval,
            current_note_argument_optional,
            // 新語追加は trusted UI / CLI の別承認経路だけに残し、AI用MCPには公開しない。
            new_tag_write_available: false,
            conversation_events_version: 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_uses_the_actor_segment_instead_of_model_name_substrings() {
        assert_eq!(
            ClientSurface::from_hint("claude-code/claude"),
            ClientSurface::ClaudeCode
        );
        assert_eq!(
            ClientSurface::from_hint("claude-desktop/claude"),
            ClientSurface::ClaudeDesktop
        );
        assert_eq!(
            ClientSurface::from_hint("codex-cli/gpt-5-codex"),
            ClientSurface::CodexCli
        );
        assert_eq!(
            ClientSurface::from_hint("chatgpt/openai"),
            ClientSurface::ChatGpt
        );
        assert_eq!(
            ClientSurface::from_hint("future-client/claude-gpt-codex"),
            ClientSurface::Unknown
        );
    }

    #[test]
    fn same_model_family_does_not_collapse_distinct_surfaces() {
        let code = ClientSurface::ClaudeCode.capabilities();
        let desktop = ClientSurface::ClaudeDesktop.capabilities();
        assert_eq!(code.client_family, desktop.client_family);
        assert_ne!(code.raw_vault_boundary, desktop.raw_vault_boundary);
        assert_ne!(code.pre_answer_retrieval, desktop.pre_answer_retrieval);
        assert!(!code.current_note_argument_optional);
        assert!(desktop.current_note_argument_optional);
    }
}
