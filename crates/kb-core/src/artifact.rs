//! Artifact(UI 語彙では「ファイル」)のモデル層 — ADR-0003 の決定を型にしたもの。
//!
//! **この module は I/O を持たない。** 置き場所・転送・取り込みは後続の層が担う。
//! ここにあるのは識別子・区分・参照先・台帳と、それらの検証規則だけ。
//! 規則を型と関数に閉じ込めるのは、instructions や画面側の実装に賭けないため
//! (docs/contract.md 設計原則「確定すべき挙動は機構で必然にする」)。
//!
//! ここに **`availability` を置かない**のは意図的(ADR-0003 決定9)。
//! 同じファイルがこの端末では取得済み・別の端末では未取得になりうるので、
//! 同期される台帳に持たせると端末間で上書き合戦になる。取得状態は端末ローカルの
//! 記録から**都度算出する**ものであって、台帳の field ではない。

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Crockford base32(ULID の英数字)。紛らわしい I / L / O / U を含まない。
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// `full` のときだけ効くサイズの目安(ADR-0003 決定8)。
/// 旧 FR-C8 の 10MB / 50MB は GitHub 同期の保全が根拠だったので、
/// 実体が Vault Git を出た時点で失効している。
pub const FULL_WARN_BYTES: u64 = 100 * 1024 * 1024;
pub const FULL_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// 参照名に使える最大長。表示名とは別物(下記 `RefName` の doc を参照)。
const REF_NAME_MAX: usize = 64;

// ───────────────────────── エラー ─────────────────────────

/// Artifact 層の失敗。**`code` で訳し分ける**(ADR-0002 決定10)。
/// kb-core の他 module はまだ anyhow だが、新しく書く層は型付きから始める
/// (docs/coding-guidelines.md §9 の昇格待ち行列)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactError {
    /// 識別子・ハッシュ・参照名の形が不正
    Malformed { field: &'static str },
    /// linked に絶対パスなど端末固有の位置を渡した
    UnstableLocator,
    /// 仕事のリポジトリ由来なので、この変更は認めない
    ClientRepoLocked,
    /// 持ち出しを広げる変更なので、明示確認を経ていない限り通さない
    RelaxationNeedsConfirm,
    /// 版が古い(別の場所で更新された)
    Conflict { expected: u64, current: u64 },
    /// `full` の上限を超えた。quota 不足とは別物として分ける
    TooLarge { size: u64, limit: u64 },
}

impl ArtifactError {
    /// 画面が文言を選ぶためのキー。文字列そのものを UI へ出さない。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Malformed { .. } => "artifact.malformed",
            Self::UnstableLocator => "artifact.unstable_locator",
            Self::ClientRepoLocked => "artifact.client_repo_locked",
            Self::RelaxationNeedsConfirm => "artifact.relaxation_needs_confirm",
            Self::Conflict { .. } => "artifact.conflict",
            Self::TooLarge { .. } => "artifact.too_large",
        }
    }
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed { field } => write!(f, "形式が不正: {field}"),
            Self::UnstableLocator => write!(f, "この場所は端末固有で、他の端末から辿れない"),
            Self::ClientRepoLocked => write!(f, "仕事のリポジトリ由来なので変更できない"),
            Self::RelaxationNeedsConfirm => write!(f, "持ち出しを広げる変更には明示確認が要る"),
            Self::Conflict { expected, current } => {
                write!(f, "別の場所で更新された(手元 {expected} / 最新 {current})")
            }
            Self::TooLarge { size, limit } => write!(f, "大きすぎる({size} > {limit})"),
        }
    }
}

impl std::error::Error for ArtifactError {}

pub type Result<T> = std::result::Result<T, ArtifactError>;

// ───────────────────────── 識別子 ─────────────────────────

/// Artifact record の不透明な identity(ULID)。
///
/// `content_hash` とは**別物**。同じ bytes でも来歴や信頼境界が違えば別の record を持てる
/// (正本の却下案「`artifact_id = content_hash`」)。時刻が先頭に来るので、
/// 文字列のまま並べれば作成順になる。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ArtifactId(String);

impl ArtifactId {
    /// 新しい ID(48bit の時刻 + 80bit の乱数)。
    pub fn new(unix_ms: u64) -> Self {
        Self(new_ulid(unix_ms))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// ULID を1つ作る(48bit の時刻 + 80bit の乱数)。
/// Artifact と保管庫の ID で同じ生成規則を使うため、ここに出しておく。
pub fn new_ulid(unix_ms: u64) -> String {
    let mut rand_bytes = [0u8; 10];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut rand_bytes);
    let mut rand_part: u128 = 0;
    for b in rand_bytes {
        rand_part = (rand_part << 8) | b as u128;
    }
    let time_part = (unix_ms as u128 & ((1u128 << 48) - 1)) << 80;
    encode_crockford(time_part | rand_part)
}

/// ULID の形をしているか。26文字・Crockford・先頭は 128bit に収まる範囲。
pub fn is_ulid(s: &str) -> bool {
    s.len() == 26
        && matches!(s.as_bytes().first(), Some(b'0'..=b'7'))
        && s.bytes().all(|b| CROCKFORD.contains(&b))
}

impl FromStr for ArtifactId {
    type Err = ArtifactError;

    fn from_str(s: &str) -> Result<Self> {
        // 26 文字 × 5bit = 130bit なので、先頭は 2bit ぶんしか使えない
        if is_ulid(s) {
            Ok(Self(s.to_string()))
        } else {
            Err(ArtifactError::Malformed {
                field: "artifact_id",
            })
        }
    }
}

impl fmt::Display for ArtifactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn encode_crockford(mut value: u128) -> String {
    let mut out = [b'0'; 26];
    for slot in out.iter_mut().rev() {
        *slot = CROCKFORD[(value & 0x1f) as usize];
        value >>= 5;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// raw bytes の SHA-256(小文字 hex)。
///
/// **Git LFS の OID と同じ値**になるよう SHA-256 に固定している(ADR-0003 決定2)。
/// 別の値を採ると、転送側と CAS 側で検証を二重に持つことになる。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ContentHash(String);

impl ContentHash {
    /// 手元にある bytes から。大きなファイルは `Hasher` を使って読みながら流す。
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let mut hasher = Hasher::new();
        hasher.update(bytes);
        hasher.finish()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ContentHash {
    type Err = ArtifactError;

    fn from_str(s: &str) -> Result<Self> {
        let ok = s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if ok {
            Ok(Self(s.to_string()))
        } else {
            Err(ArtifactError::Malformed {
                field: "content_hash",
            })
        }
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 読みながら流すためのハッシュ器。取り込みは全量をメモリへ載せない
/// (ADR-0003 決定8。現行フロントの base64 経路を置き換える先)。
#[derive(Default)]
pub struct Hasher {
    inner: Sha256,
    len: u64,
}

impl Hasher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, chunk: &[u8]) {
        self.inner.update(chunk);
        self.len += chunk.len() as u64;
    }

    /// 読んだ総バイト数。台帳の `size` はここから採る(呼び出し側で数え直さない)。
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn finish(self) -> ContentHash {
        ContentHash(hex_lower(&self.inner.finalize()))
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((b & 0x0f) as u32, 16).unwrap_or('0'));
    }
    out
}

/// 保管庫の中で一意な参照名。**表示名とは別物**。
///
/// 表示名は自由に直せるが、参照名は本文リンク `kb-artifact-ref:<名前>` の解決先なので、
/// 保管庫の中で一意でなければならない(正本の受入条件「artifact_ref は workspace scope で一意」)。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RefName(String);

impl RefName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for RefName {
    type Err = ArtifactError;

    fn from_str(s: &str) -> Result<Self> {
        let ok = !s.is_empty()
            && s.len() <= REF_NAME_MAX
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !s.starts_with('-')
            && !s.ends_with('-');
        if ok {
            Ok(Self(s.to_string()))
        } else {
            Err(ArtifactError::Malformed { field: "ref_name" })
        }
    }
}

impl fmt::Display for RefName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ───────────────────────── 区分(二軸) ─────────────────────────

/// 閲覧区分。**転送軸(`SyncPolicy`)とは別の軸**で、UI でも1語に統合しない
/// (統合すると、正本が分けた軸を画面で混ぜ直すことになる)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    Private,
    Shared,
}

/// 転送軸。どこまで端末の外へ出すか。並び順がそのまま「広さ」になる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum SyncPolicy {
    /// この端末だけ。台帳も外へ出さない
    LocalOnly,
    /// 名前・役割・来歴は同期する。実体は出さない
    ManifestOnly,
    /// 実体も同期する
    Full,
}

impl SyncPolicy {
    /// `full` のときだけサイズの目安を持つ(ADR-0003 決定8)。
    /// 返り値は (警告, 拒否)。
    pub fn size_limits(self) -> Option<(u64, u64)> {
        match self {
            Self::Full => Some((FULL_WARN_BYTES, FULL_MAX_BYTES)),
            Self::LocalOnly | Self::ManifestOnly => None,
        }
    }

    /// 上限そのもの。quota 不足はサイズ内でも起きるので、ここでは扱わない。
    pub fn check_size(self, size: u64) -> Result<Option<u64>> {
        match self.size_limits() {
            Some((_, max)) if size > max => Err(ArtifactError::TooLarge { size, limit: max }),
            Some((warn, _)) if size > warn => Ok(Some(warn)),
            _ => Ok(None),
        }
    }
}

/// 役割。独立した entity には分けず field で表す(正本の決定)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    /// ふつうのファイル
    File,
    /// 会話の原本。既定で検索から外す
    Transcript,
    Dataset,
    /// 機械が作った索引。原本より順位を下げる
    DerivedIndex,
}

impl Role {
    /// 既定で検索に載せるか。`transcript` だけ既定で外す。
    pub fn retrievable_by_default(self) -> bool {
        !matches!(self, Self::Transcript)
    }
}

/// 二軸をまとめた持ち出し設定。仕事のリポジトリ由来かどうかもここが持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Policy {
    pub sensitivity: Sensitivity,
    pub sync: SyncPolicy,
    /// 仕事のリポジトリの中にあるファイル。**instructions や prompt から緩められない**
    /// (docs/contract.md 契約5)。
    pub client_repo: bool,
}

impl Policy {
    /// 通常の新規ファイルの既定(ADR-0003 決定6)。
    pub fn default_managed() -> Self {
        Self {
            sensitivity: Sensitivity::Private,
            sync: SyncPolicy::Full,
            client_repo: false,
        }
    }

    /// 仕事のリポジトリ由来の既定。`local_only` に固定される。
    pub fn client_repo_locked() -> Self {
        Self {
            sensitivity: Sensitivity::Private,
            sync: SyncPolicy::LocalOnly,
            client_repo: true,
        }
    }

    /// 移行してきた既存の添付(ADR-0003 決定4)。
    /// `shared` と推定しない・`client_repo` を false と断定しない、が要点。
    /// ここでは前者だけを型で守り、後者は取り込み側が来歴に「不明」を残す。
    pub fn migrated_legacy() -> Self {
        Self {
            sensitivity: Sensitivity::Private,
            sync: SyncPolicy::Full,
            client_repo: false,
        }
    }

    /// `to` への変更が持ち出しを**広げる**か。狭める方向はいつでも通す。
    pub fn is_relaxation(&self, to: &Policy) -> bool {
        to.sensitivity > self.sensitivity || to.sync > self.sync
    }

    /// 変更してよいかの判定。`confirmed` は**対話的な UI だけが立てられる**
    /// (MCP には緩和の能力自体を渡さない — docs/contract.md 契約5)。
    pub fn check_change(&self, to: &Policy, confirmed: bool) -> Result<()> {
        if self.client_repo {
            // 仕事のリポジトリ由来は、確認画面に入る前にここで落とす
            return Err(ArtifactError::ClientRepoLocked);
        }
        if to.client_repo && !self.client_repo {
            return Err(ArtifactError::ClientRepoLocked);
        }
        if self.is_relaxation(to) && !confirmed {
            return Err(ArtifactError::RelaxationNeedsConfirm);
        }
        Ok(())
    }
}

// ───────────────────────── 参照先 ─────────────────────────

/// 実体がどこにあるか。
///
/// **裸の絶対パスを持たせない**(正本の受入条件)。端末ごとにクローン先が違うので、
/// 絶対パスは他の端末から辿れない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Locator {
    /// この保管庫が実体を持つ。置き場は同期境界ごとに分ける
    Managed { hash: ContentHash },
    /// 元の場所を指す。リポジトリ ID + その中の相対パスだけ
    /// (安定 URI は後段 — ADR-0003 決定6)
    Linked { repo_id: String, rel_path: String },
    /// 移行元の旧サイドカー `<note_id>.files/<file_name>`。
    /// **読み取り専用**で、新規の保存先にはしない(ADR-0003 決定4)
    LegacyGit { note_id: String, file_name: String },
}

impl Locator {
    /// リポジトリの中を指す参照を作る。絶対パスや親への遡上は拒否する。
    pub fn linked(repo_id: &str, rel_path: &str) -> Result<Self> {
        if repo_id.is_empty() {
            return Err(ArtifactError::Malformed { field: "repo_id" });
        }
        let unstable = rel_path.is_empty()
            || rel_path.starts_with('/')
            || rel_path.starts_with('~')
            || rel_path.split('/').any(|seg| seg == "..")
            // Windows のドライブレター(C:\… )も端末固有
            || rel_path.chars().nth(1) == Some(':');
        if unstable {
            return Err(ArtifactError::UnstableLocator);
        }
        Ok(Self::Linked {
            repo_id: repo_id.to_string(),
            rel_path: rel_path.to_string(),
        })
    }

    /// 新しく書き込んでよい置き場か。旧サイドカーは読むだけ。
    pub fn is_writable(&self) -> bool {
        !matches!(self, Self::LegacyGit { .. })
    }
}

// ───────────────────────── 台帳 ─────────────────────────

/// 作成時に決まり、二度と変わらない部分。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Created {
    pub media_type: String,
    pub size: u64,
    /// RFC3339
    pub at: String,
    /// どこから来たか(会話・取り込み・移行など)。移行分は「不明」を残す
    pub origin: String,
    pub by: String,
}

/// 追記だけできる出来事。**消せない**ので、訂正も追記として残す。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Event {
    pub at: String,
    pub kind: String,
    pub detail: String,
}

/// ファイル1つ分の台帳。
///
/// 3区分（変えられない / 積み上がる / 直せる）を struct の形でそのまま表す。
/// 画面もこの3区分をそのまま3ブロックにする。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Manifest {
    // ── 変えられない ──
    pub id: ArtifactId,
    pub hash: ContentHash,
    pub created: Created,

    // ── 積み上がる ──
    pub events: Vec<Event>,

    // ── 直せる(版の指定が要る) ──
    pub version: u64,
    pub display_name: String,
    pub locator: Locator,
    pub policy: Policy,
    pub role: Role,
    /// 検索に載せるか。既定は role から決まるが、あとから変えられる
    pub retrievable: bool,
    /// 内容を差し替えたとき、前の版の ID をここへ残す
    pub supersedes: Option<ArtifactId>,
}

impl Manifest {
    /// 取り込み直後の台帳。
    pub fn new(
        id: ArtifactId,
        hash: ContentHash,
        created: Created,
        display_name: String,
        locator: Locator,
        policy: Policy,
        role: Role,
    ) -> Self {
        Self {
            id,
            hash,
            created,
            events: Vec::new(),
            version: 1,
            display_name,
            locator,
            policy,
            role,
            retrievable: role.retrievable_by_default(),
            supersedes: None,
        }
    }

    /// 出来事を1件足す。既存の履歴は書き換えない。
    pub fn record(&mut self, at: &str, kind: &str, detail: &str) {
        self.events.push(Event {
            at: at.to_string(),
            kind: kind.to_string(),
            detail: detail.to_string(),
        });
    }

    /// 直せる部分の更新。**版が合わなければ通さない**(自動再試行も強制上書きもしない)。
    pub fn apply(&mut self, expected_version: u64, change: Change, confirmed: bool) -> Result<()> {
        if expected_version != self.version {
            return Err(ArtifactError::Conflict {
                expected: expected_version,
                current: self.version,
            });
        }
        if let Some(policy) = change.policy {
            self.policy.check_change(&policy, confirmed)?;
            self.policy = policy;
        }
        if let Some(name) = change.display_name {
            self.display_name = name;
        }
        if let Some(retrievable) = change.retrievable {
            self.retrievable = retrievable;
        }
        self.version += 1;
        Ok(())
    }

    /// 内容の差し替え = **新しい台帳**。元は書き換えない(不変)。
    pub fn succeed(&self, id: ArtifactId, hash: ContentHash, created: Created) -> Self {
        let mut next = Manifest::new(
            id,
            hash,
            created,
            self.display_name.clone(),
            self.locator.clone(),
            self.policy,
            self.role,
        );
        next.retrievable = self.retrievable;
        next.supersedes = Some(self.id.clone());
        next
    }
}

/// 更新して**よい** field だけを持つ。ここに無いものは変えられない、を型で示す。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Change {
    pub display_name: Option<String>,
    pub policy: Option<Policy>,
    pub retrievable: Option<bool>,
}

/// 保管庫の中の「いまの版」。本文リンクはこれを辿るので、最新へ追従する。
/// 出典(`sources[].resource`)は逆に `artifact_id` で固定する(ADR-0003 決定5)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ArtifactRef {
    pub workspace_id: String,
    pub name: RefName,
    pub artifact_id: ArtifactId,
    pub revision: u64,
}

impl ArtifactRef {
    pub fn new(workspace_id: &str, name: RefName, artifact_id: ArtifactId) -> Self {
        Self {
            workspace_id: workspace_id.to_string(),
            name,
            artifact_id,
            revision: 1,
        }
    }

    /// 指す先の差し替え。版が合わなければ競合として返す
    /// (呼び出し側は再取得してからやり直す。別名で足す道も UI が持つ)。
    pub fn point_to(&mut self, expected_revision: u64, artifact_id: ArtifactId) -> Result<()> {
        if expected_revision != self.revision {
            return Err(ArtifactError::Conflict {
                expected: expected_revision,
                current: self.revision,
            });
        }
        self.artifact_id = artifact_id;
        self.revision += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn created() -> Created {
        Created {
            media_type: "image/png".into(),
            size: 412 * 1024,
            at: "2026-08-12T09:04:00Z".into(),
            origin: "conversation:codex-cli".into(),
            by: crate::OWNER_ACTOR.into(),
        }
    }

    fn manifest() -> Manifest {
        Manifest::new(
            ArtifactId::new(1_755_000_000_000),
            ContentHash::of_bytes(b"sketch"),
            created(),
            "検討スケッチ.png".into(),
            Locator::Managed {
                hash: ContentHash::of_bytes(b"sketch"),
            },
            Policy::default_managed(),
            Role::File,
        )
    }

    #[test]
    fn id_is_26_chars_and_round_trips() {
        let id = ArtifactId::new(1_755_000_000_000);
        assert_eq!(id.as_str().len(), 26);
        assert_eq!(ArtifactId::from_str(id.as_str()).unwrap(), id);
    }

    #[test]
    fn ids_sort_by_creation_time() {
        let older = ArtifactId::new(1_700_000_000_000);
        let newer = ArtifactId::new(1_800_000_000_000);
        assert!(older < newer, "{older} < {newer}");
    }

    #[test]
    fn id_rejects_confusable_letters_and_bad_length() {
        // Crockford は I / L / O / U を持たない
        assert!(ArtifactId::from_str("0123456789ABCDEFGHIJKLMNOP").is_err());
        assert!(ArtifactId::from_str("0123").is_err());
        // 26 文字 × 5bit は 128bit に収まらないので、先頭は 7 まで
        assert!(ArtifactId::from_str("Z123456789ABCDEFGH JKMNPQR".trim()).is_err());
    }

    #[test]
    fn hash_matches_known_sha256() {
        // 空入力の SHA-256(LFS の OID と同じ値になることの確認)
        assert_eq!(
            ContentHash::of_bytes(b"").as_str(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn streaming_hash_equals_one_shot() {
        let mut h = Hasher::new();
        h.update(b"abc");
        h.update(b"def");
        assert_eq!(h.len(), 6);
        assert_eq!(h.finish(), ContentHash::of_bytes(b"abcdef"));
    }

    #[test]
    fn hash_rejects_uppercase_and_short() {
        assert!(
            ContentHash::from_str(
                "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855"
            )
            .is_err()
        );
        assert!(ContentHash::from_str("abc").is_err());
    }

    #[test]
    fn ref_name_shape() {
        assert!(RefName::from_str("two-layer-sketch").is_ok());
        assert!(RefName::from_str("-leading").is_err());
        assert!(RefName::from_str("Upper").is_err());
        assert!(RefName::from_str("").is_err());
        assert!(RefName::from_str(&"a".repeat(REF_NAME_MAX + 1)).is_err());
    }

    #[test]
    fn linked_rejects_device_specific_paths() {
        assert!(Locator::linked("kb-app", "docs/spec.md").is_ok());
        assert_eq!(
            Locator::linked("kb-app", "/Users/me/docs/spec.md"),
            Err(ArtifactError::UnstableLocator)
        );
        assert_eq!(
            Locator::linked("kb-app", "~/docs/spec.md"),
            Err(ArtifactError::UnstableLocator)
        );
        assert_eq!(
            Locator::linked("kb-app", "../outside.md"),
            Err(ArtifactError::UnstableLocator)
        );
        assert_eq!(
            Locator::linked("kb-app", r"C:\docs\spec.md"),
            Err(ArtifactError::UnstableLocator)
        );
    }

    #[test]
    fn legacy_sidecar_is_read_only() {
        let legacy = Locator::LegacyGit {
            note_id: "notes/foo".into(),
            file_name: "diagram.png".into(),
        };
        assert!(!legacy.is_writable());
        assert!(Locator::linked("kb-app", "a/b.md").unwrap().is_writable());
    }

    #[test]
    fn narrowing_is_always_allowed_widening_needs_confirmation() {
        let from = Policy::default_managed(); // private + full
        let narrower = Policy {
            sync: SyncPolicy::ManifestOnly,
            ..from
        };
        assert!(from.check_change(&narrower, false).is_ok());

        let wider = Policy {
            sensitivity: Sensitivity::Shared,
            ..from
        };
        assert_eq!(
            from.check_change(&wider, false),
            Err(ArtifactError::RelaxationNeedsConfirm)
        );
        assert!(from.check_change(&wider, true).is_ok());
    }

    #[test]
    fn client_repo_cannot_be_changed_even_with_confirmation() {
        let locked = Policy::client_repo_locked();
        let wider = Policy {
            sync: SyncPolicy::Full,
            ..locked
        };
        // 確認済みフラグを立てても通らない。確認画面へ入る前に落ちる
        assert_eq!(
            locked.check_change(&wider, true),
            Err(ArtifactError::ClientRepoLocked)
        );
    }

    #[test]
    fn normal_file_cannot_be_marked_as_client_repo_afterwards() {
        let normal = Policy::default_managed();
        let pretend = Policy {
            client_repo: true,
            ..normal
        };
        assert_eq!(
            normal.check_change(&pretend, true),
            Err(ArtifactError::ClientRepoLocked)
        );
    }

    #[test]
    fn size_limits_apply_to_full_only() {
        assert!(SyncPolicy::LocalOnly.check_size(u64::MAX).is_ok());
        assert!(SyncPolicy::ManifestOnly.check_size(u64::MAX).is_ok());
        assert_eq!(SyncPolicy::Full.check_size(1024).unwrap(), None);
        assert_eq!(
            SyncPolicy::Full.check_size(FULL_WARN_BYTES + 1).unwrap(),
            Some(FULL_WARN_BYTES)
        );
        assert!(matches!(
            SyncPolicy::Full.check_size(FULL_MAX_BYTES + 1),
            Err(ArtifactError::TooLarge { .. })
        ));
    }

    #[test]
    fn transcript_is_excluded_from_search_by_default() {
        assert!(!Role::Transcript.retrievable_by_default());
        assert!(Role::File.retrievable_by_default());
        assert!(Role::DerivedIndex.retrievable_by_default());
    }

    #[test]
    fn stale_version_is_a_conflict_and_changes_nothing() {
        let mut m = manifest();
        let change = Change {
            display_name: Some("別名.png".into()),
            ..Default::default()
        };
        assert_eq!(
            m.apply(99, change.clone(), false),
            Err(ArtifactError::Conflict {
                expected: 99,
                current: 1
            })
        );
        assert_eq!(m.display_name, "検討スケッチ.png");
        assert_eq!(m.version, 1);

        assert!(m.apply(1, change, false).is_ok());
        assert_eq!(m.display_name, "別名.png");
        assert_eq!(m.version, 2);
    }

    #[test]
    fn events_only_accumulate() {
        let mut m = manifest();
        m.record("2026-08-11T20:16:04Z", "imported", "会話から取り込み");
        m.record("2026-08-12T09:04:00Z", "corrected", "図の左右が逆だった");
        assert_eq!(m.events.len(), 2);
        // 更新しても履歴は減らない
        m.apply(1, Change::default(), false).unwrap();
        assert_eq!(m.events.len(), 2);
    }

    #[test]
    fn replacing_content_creates_a_new_manifest() {
        let old = manifest();
        let next = old.succeed(
            ArtifactId::new(1_755_000_100_000),
            ContentHash::of_bytes(b"sketch v2"),
            created(),
        );
        assert_eq!(next.supersedes.as_ref(), Some(&old.id));
        assert_ne!(next.id, old.id);
        assert_ne!(next.hash, old.hash);
        // 元は触られていない
        assert_eq!(old.supersedes, None);
        assert_eq!(old.version, 1);
    }

    #[test]
    fn ref_update_needs_the_current_revision() {
        let mut r = ArtifactRef::new(
            "wsp-1",
            RefName::from_str("sketch").unwrap(),
            ArtifactId::new(1_755_000_000_000),
        );
        let next = ArtifactId::new(1_755_000_100_000);
        assert_eq!(
            r.point_to(7, next.clone()),
            Err(ArtifactError::Conflict {
                expected: 7,
                current: 1
            })
        );
        assert!(r.point_to(1, next.clone()).is_ok());
        assert_eq!(r.artifact_id, next);
        assert_eq!(r.revision, 2);
    }

    #[test]
    fn manifest_round_trips_through_json() {
        let m = manifest();
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<Manifest>(&json).unwrap(), m);
        // 取得状態は台帳に載せない(ADR-0003 決定9)
        assert!(!json.contains("availability"));
    }

    #[test]
    fn error_codes_are_stable() {
        assert_eq!(
            ArtifactError::ClientRepoLocked.code(),
            "artifact.client_repo_locked"
        );
        assert_eq!(
            ArtifactError::Conflict {
                expected: 1,
                current: 2
            }
            .code(),
            "artifact.conflict"
        );
    }
}
