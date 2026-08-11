//! OKF v0.2 準拠の frontmatter(docs/okf-conformance.md の詳細設計)。
//! app 固有拡張は `origin` 1キーのみ。未知キーは `extra` に保持し、
//! round-trip で落とさない(OKF §4.1: consumers SHOULD preserve unknown keys)。

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const STATUS_DRAFT: &str = "draft";
pub const STATUS_STABLE: &str = "stable";
pub const STATUS_DEPRECATED: &str = "deprecated";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Generated {
    pub by: String,
    pub at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frontmatter {
    /// OKF 必須キー。kb-app v0.1 は全ノート "Note"。
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// OKF §5.4。不在は stable と等価。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated: Option<Generated>,
    /// OKF §5.2。bare mapping も1要素リストとして扱う必要があるため raw で保持。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified: Option<serde_yaml::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sources: Option<serde_yaml::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_after: Option<String>,
    /// app 拡張: 作成日時(OKF に該当フィールドが無いため。generated.at は「最終更新」)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    /// app 拡張。"human" = メモ(聖域)/ "agent" = 育つノート。
    /// 作成時に刻まれ、越境の明示操作でのみ変わる(原則9)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

impl Frontmatter {
    pub fn new_note(title: &str) -> Self {
        Frontmatter {
            kind: "Note".to_string(),
            title: Some(title.to_string()),
            description: None,
            tags: Vec::new(),
            status: None,
            generated: None,
            verified: None,
            sources: None,
            stale_after: None,
            created: None,
            origin: None,
            extra: BTreeMap::new(),
        }
    }

    /// 作成日時。未設定なら移行ノートの legacy.created を使う(旧 KB からの引き継ぎ)。
    pub fn created_at(&self) -> Option<String> {
        if let Some(c) = &self.created {
            return Some(c.clone());
        }
        self.extra
            .get("legacy")
            .and_then(|v| v.get("created"))
            .and_then(|v| v.as_str())
            .map(|d| {
                if d.len() == 10 {
                    format!("{d}T00:00:00Z")
                } else {
                    d.to_string()
                }
            })
    }

    /// 最終更新(generated.at)。
    pub fn updated_at(&self) -> Option<String> {
        self.generated.as_ref().map(|g| g.at.clone())
    }

    /// 不在 = stable(OKF §5.4)。
    pub fn effective_status(&self) -> &str {
        self.status.as_deref().unwrap_or(STATUS_STABLE)
    }

    /// verified に検証イベントを追記。bare mapping は1要素リストへ正規化(OKF §5.2)。
    pub fn append_verified(&mut self, by: &str, at: &str) {
        let event = serde_yaml::to_value(Generated {
            by: by.into(),
            at: at.into(),
        })
        .expect("verified event serializes");
        let list = match self.verified.take() {
            None => vec![event],
            Some(serde_yaml::Value::Sequence(mut seq)) => {
                seq.push(event);
                seq
            }
            Some(bare) => vec![bare, event],
        };
        self.verified = Some(serde_yaml::Value::Sequence(list));
    }
}

/// ノートファイル全体(frontmatter + body)。
#[derive(Debug, Clone)]
pub struct Note {
    pub front: Frontmatter,
    pub body: String,
}

impl Note {
    pub fn to_file_string(&self) -> Result<String> {
        let yaml = serde_yaml::to_string(&self.front).context("frontmatter serialize")?;
        let body = self.body.trim_start_matches('\n').trim_end();
        Ok(format!("---\n{yaml}---\n\n{body}\n"))
    }

    /// OKF conformance(§11): parse 不能な frontmatter・type 欠落はエラー。
    pub fn parse(content: &str) -> Result<Note> {
        let rest = content
            .strip_prefix("---\n")
            .or_else(|| content.strip_prefix("---\r\n"))
            .with_context(|| "frontmatter がない(--- で始まっていない)")?;
        let end = rest
            .find("\n---\n")
            .or_else(|| rest.find("\n---\r\n"))
            .with_context(|| "frontmatter の終端 --- がない")?;
        let (yaml, body) = rest.split_at(end);
        let body = body
            .trim_start_matches("\n---\n")
            .trim_start_matches("\n---\r\n")
            .trim_start_matches('\n');
        let front: Frontmatter = serde_yaml::from_str(yaml).context("frontmatter parse")?;
        if front.kind.trim().is_empty() {
            bail!("type が空(OKF 必須キー)");
        }
        Ok(Note {
            front,
            body: body.to_string(),
        })
    }
}

/// 現在時刻の ISO 8601(UTC、秒精度)。
pub fn now_iso() -> String {
    let fmt = time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc()
        .replace_millisecond(0)
        .expect("ms=0 valid")
        .format(&fmt)
        .expect("rfc3339 format")
}

/// 今日の日付(UTC、YYYY-MM-DD)。log.md の日付見出し用。
pub fn today() -> String {
    let d = time::OffsetDateTime::now_utc().date();
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_unknown_keys() {
        let src = "---\ntype: Note\ntitle: t\ncustom_key: 42\nstatus: draft\n---\n\nbody text\n";
        let note = Note::parse(src).unwrap();
        assert_eq!(note.front.kind, "Note");
        assert_eq!(note.front.effective_status(), STATUS_DRAFT);
        assert!(note.front.extra.contains_key("custom_key"));
        let out = note.to_file_string().unwrap();
        assert!(out.contains("custom_key"));
        let again = Note::parse(&out).unwrap();
        assert_eq!(again.body.trim(), "body text");
    }

    #[test]
    fn verified_bare_mapping_becomes_list() {
        let src =
            "---\ntype: Note\nverified: { by: 'human:o', at: '2026-01-01T00:00:00Z' }\n---\n\nx\n";
        let mut note = Note::parse(src).unwrap();
        note.front
            .append_verified("human:o", "2026-02-01T00:00:00Z");
        match note.front.verified {
            Some(serde_yaml::Value::Sequence(ref s)) => assert_eq!(s.len(), 2),
            ref other => panic!("expected sequence, got {other:?}"),
        }
    }

    #[test]
    fn missing_type_rejected() {
        assert!(Note::parse("---\ntitle: x\n---\n\nb\n").is_err());
    }
}
