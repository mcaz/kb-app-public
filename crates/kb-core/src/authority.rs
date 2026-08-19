//! ノートのcanonical authorityとtyped relation。
//!
//! 物理path・タグ・本文推定を正本にせず、継続蒸留が同じscopeの現行canonicalを
//! 一意に選び、根拠と後継を不変IDで辿るための機械管理envelope。

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(transparent)]
pub struct NoteUid(String);

impl NoteUid {
    pub fn new() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        Self(crate::artifact::new_ulid(now))
    }

    #[cfg(test)]
    pub(crate) fn at(unix_ms: u64) -> Self {
        Self(crate::artifact::new_ulid(unix_ms))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for NoteUid {
    fn default() -> Self {
        Self::new()
    }
}

impl FromStr for NoteUid {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        if crate::artifact::is_ulid(value) {
            Ok(Self(value.to_string()))
        } else {
            bail!("note_uidは26文字のULIDにする")
        }
    }
}

impl<'de> Deserialize<'de> for NoteUid {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_str(&value).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for NoteUid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum NoteNamespace {
    Entities,
    Initiatives,
    Decisions,
    Procedures,
    Records,
    Knowledge,
}

impl NoteNamespace {
    pub const ALL: [Self; 6] = [
        Self::Entities,
        Self::Initiatives,
        Self::Decisions,
        Self::Procedures,
        Self::Records,
        Self::Knowledge,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Entities => "entities",
            Self::Initiatives => "initiatives",
            Self::Decisions => "decisions",
            Self::Procedures => "procedures",
            Self::Records => "records",
            Self::Knowledge => "knowledge",
        }
    }
}

impl FromStr for NoteNamespace {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|namespace| namespace.as_str() == value)
            .ok_or_else(|| anyhow::anyhow!("未知のnamespace: {value}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum AuthorityRole {
    Canonical,
    Record,
    Proposal,
}

impl AuthorityRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Canonical => "canonical",
            Self::Record => "record",
            Self::Proposal => "proposal",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum AuthorityStatus {
    Active,
    Historical,
    Superseded,
}

impl AuthorityStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Historical => "historical",
            Self::Superseded => "superseded",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Authority {
    pub namespace: NoteNamespace,
    pub role: AuthorityRole,
    pub status: AuthorityStatus,
    pub scope: String,
}

impl Authority {
    pub fn validate(&self) -> Result<()> {
        validate_scope(&self.scope)?;
        match (self.namespace, self.role) {
            (NoteNamespace::Records, AuthorityRole::Record) => {}
            (NoteNamespace::Records, _) => {
                bail!("records namespaceのroleはrecordに固定する")
            }
            (_, AuthorityRole::Record) => {
                bail!("record roleはrecords namespaceにだけ使える")
            }
            _ => {}
        }
        if self.role == AuthorityRole::Proposal && self.status != AuthorityStatus::Active {
            bail!("proposal roleのstatusはactiveに固定する")
        }
        if self.status == AuthorityStatus::Superseded && self.role != AuthorityRole::Canonical {
            bail!("superseded statusはcanonical roleにだけ使える")
        }
        Ok(())
    }

    pub fn is_active_canonical(&self) -> bool {
        self.role == AuthorityRole::Canonical && self.status == AuthorityStatus::Active
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    DerivedFrom,
    Supports,
    Updates,
    Contradicts,
    Supersedes,
    Mentions,
}

impl RelationKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DerivedFrom => "derived_from",
            Self::Supports => "supports",
            Self::Updates => "updates",
            Self::Contradicts => "contradicts",
            Self::Supersedes => "supersedes",
            Self::Mentions => "mentions",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct NoteRelation {
    #[serde(rename = "type")]
    pub kind: RelationKind,
    pub target: NoteUid,
}

/// authority envelopeを一部だけ書く状態を拒否する。両方とも無い既存ノートだけは
/// 物理移行waveまでlegacy互換として読む。
pub fn validate_envelope(
    note_uid: Option<&NoteUid>,
    authority: Option<&Authority>,
    relations: &[NoteRelation],
) -> Result<()> {
    match (note_uid, authority) {
        (None, None) if relations.is_empty() => Ok(()),
        (Some(uid), Some(authority)) => {
            authority.validate()?;
            let mut unique = BTreeSet::new();
            for relation in relations {
                if relation.target == *uid {
                    bail!("typed relationは自分自身を参照できない")
                }
                if !unique.insert((relation.kind, relation.target.clone())) {
                    bail!(
                        "typed relationが重複している: {} -> {}",
                        relation.kind.as_str(),
                        relation.target
                    )
                }
            }
            Ok(())
        }
        _ => bail!("note_uidとauthorityは同時に設定する"),
    }
}

fn validate_scope(scope: &str) -> Result<()> {
    if scope.is_empty() || scope.chars().count() > 160 || scope.trim() != scope {
        bail!("authority scopeは1〜160文字で前後空白なしにする")
    }
    for segment in scope.split('/') {
        let mut chars = segment.chars();
        let first = chars.next();
        let last = segment.chars().next_back();
        if first.is_none_or(|value| !value.is_alphanumeric())
            || last.is_none_or(|value| !value.is_alphanumeric())
            || !segment
                .chars()
                .all(|value| value.is_alphanumeric() || value == '-')
        {
            bail!("authority scopeは英数字・Unicode文字・内部hyphenのslash区切りにする")
        }
        if segment
            .chars()
            .any(|value| value.is_ascii_uppercase() || value.is_control())
        {
            bail!("authority scopeはASCII大文字や制御文字を含めない")
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uid(seed: u64) -> NoteUid {
        NoteUid::at(seed)
    }

    #[test]
    fn namespace_and_role_cannot_disagree() {
        let authority = Authority {
            namespace: NoteNamespace::Records,
            role: AuthorityRole::Canonical,
            status: AuthorityStatus::Active,
            scope: "kb-app/同期検証".into(),
        };
        assert!(authority.validate().is_err());
    }

    #[test]
    fn envelope_rejects_partial_identity_and_duplicate_edges() {
        let me = uid(1);
        let target = uid(2);
        let authority = Authority {
            namespace: NoteNamespace::Knowledge,
            role: AuthorityRole::Canonical,
            status: AuthorityStatus::Active,
            scope: "kb-app/remote-backup".into(),
        };
        assert!(validate_envelope(None, Some(&authority), &[]).is_err());
        assert!(
            validate_envelope(
                Some(&me),
                Some(&authority),
                &[
                    NoteRelation {
                        kind: RelationKind::Supports,
                        target: target.clone(),
                    },
                    NoteRelation {
                        kind: RelationKind::Supports,
                        target,
                    },
                ],
            )
            .is_err()
        );
    }

    #[test]
    fn legacy_note_without_an_envelope_remains_readable() {
        assert!(validate_envelope(None, None, &[]).is_ok());
    }

    #[test]
    fn deserialization_cannot_bypass_uid_validation() {
        assert!(serde_json::from_str::<NoteUid>("\"not-a-ulid\"").is_err());
    }
}
