//! 接続の通知は既知codeと固定文だけを渡し、設定pathやhost由来の自由文を配信しない。

use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientNotice {
    GuardOutdated,
    HostCapabilityUnverified,
}

impl ClientNotice {
    pub fn code(self) -> &'static str {
        match self {
            Self::GuardOutdated => "guard_outdated",
            Self::HostCapabilityUnverified => "host_capability_unverified",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::GuardOutdated => {
                "管理された保護設定が現在の要件と一致しないため、KBへの接続を停止した。kb-appの設定で保護設定を更新する必要がある。"
            }
            Self::HostCapabilityUnverified => {
                "実行中のAIクライアントの受信能力は未確認。hookは保守的な出力予算を使用し、全文受信は保証しない。"
            }
        }
    }

    pub fn value(self) -> Value {
        json!({"code": self.code(), "message": self.message()})
    }

    pub fn is_present_in(self, notices: &Value) -> bool {
        notices.as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| item.get("code").and_then(Value::as_str) == Some(self.code()))
        })
    }

    /// 単独の通知もhostにJSONと誤認させない（2026-09-05、PR #128）。
    pub fn hook_line(self) -> String {
        format!("# [kb-app 接続状態] {}: {}\n", self.code(), self.message())
    }
}
