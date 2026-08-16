//! 実体(blob)の置き場 — ADR-0003 決定2・決定3。
//!
//! **保管庫(Vault Git)の外**に置く。中に入るのは台帳・参照・小さな印までで、
//! raw な実体は入れない(clone・履歴・同期を肥大させないため)。
//!
//! ## 境界ごとに物理的に分ける
//!
//! 置き場は `<データ領域>/kb-app/artifacts/<保管庫 ID>/<境界>/` で、
//! 同期区分ごとに**別のディレクトリ**になる。決定が求めるのは
//! 「境界を跨いだ重複排除をしない」だけでなく「**存在確認もしない**」ことで、
//! これは blob そのものが漏れなくても *「この hash を持っているか」への答え* が
//! 別境界の情報を漏らすため。
//!
//! したがってこの module の API は**必ず境界を引数に取る**。
//! 「この hash をどこかで持っているか」を訊く関数を置かない — 置けば必ず使われる。
//!
//! ## 取得状態(availability)がここに居る理由
//!
//! `Availability` を台帳(`artifact.rs`)ではなくここに置いているのは意図的。
//! 同じファイルがこの端末では取得済み・別の端末では未取得になりうるので、
//! これは**同期される台帳の field ではなく、端末ごとに算出する値**(ADR-0003 決定9)。
//! 算出する場所に型を置くことで、うっかり台帳へ載せる道を塞ぐ。

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::artifact::{
    ArtifactError, ContentHash, Hasher, Locator, Manifest, SyncPolicy, new_ulid,
};
use crate::vault::Vault;

/// 一度に読む塊の大きさ。全量をメモリへ載せない(ADR-0003 決定8)。
const CHUNK: usize = 64 * 1024;

/// この端末で実体を開けるか。**台帳には載せない**(上の doc 参照)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// 手元にある
    Local,
    /// この端末には無い(取り寄せられる可能性がある)
    Missing,
    /// 方針により、この端末では開かない。**取得の再試行も提案しない**
    UnavailableByPolicy,
}

/// 照合の結果。不一致は「壊れている」ではなく「**照合できない**」として扱う
/// — 台帳の不変 field を書き換えず、検索にも載せない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verified {
    Ok,
    Mismatch,
    Missing,
}

/// 取り込みの結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    pub hash: ContentHash,
    pub size: u64,
    /// 大きさの警告(拒否ではない)。`full` のときだけ出る
    pub warn_over: Option<u64>,
    /// 同じ境界に同じ内容が既にあった
    pub deduped: bool,
}

/// この端末で開けるかを**算出**する。台帳から読むのではない。
///
/// 境界ごとに実体の持ち主が違うので、ここが唯一の入口になる:
/// `full` は LFS の置き場、`local_only` は自前の置き場。
pub fn availability(vault: &Vault, stores: &Stores, m: &Manifest) -> Availability {
    // 仕事のリポジトリ由来は、取得を試みること自体をしない
    if m.policy.client_repo && m.policy.sync != SyncPolicy::LocalOnly {
        return Availability::UnavailableByPolicy;
    }
    // **どこにあるかは locator が決める。** 区分(sync)は「どこまで運びたいか」で、
    // 実体の持ち主とは別軸。区分だけで分岐していたため、保管庫の中に実物がある
    // 旧添付が「この端末にありません」になっていた(2026-08-13)
    let here = match &m.locator {
        Locator::Managed { .. } => match m.policy.sync {
            SyncPolicy::Full => crate::lfs::has(vault, &m.hash),
            other => stores.has_local(other, &m.hash),
        },
        // 移行前の旧添付は、保管庫の中の実ファイルがそのまま実体
        Locator::LegacyGit { note_id, file_name } => vault
            .legacy_attachment_path(note_id, file_name)
            .is_ok_and(|path| path.is_file()),
        // 元の場所を指すだけ。**リポジトリ ID から手元のパスを引く仕組みが無い**ので
        // 在否を確かめられない。確かめられないことを Local と言わない側に倒す
        // (リポジトリの所在を持つ台帳は ADR-0003 の残課題)
        Locator::Linked { .. } => false,
    };
    if here {
        Availability::Local
    } else {
        Availability::Missing
    }
}

/// 保管庫1つ分の置き場。境界ごとのディレクトリを束ねるだけで、跨ぐ操作を持たない。
#[derive(Debug, Clone)]
pub struct Stores {
    root: PathBuf,
    workspace_id: String,
}

impl Stores {
    /// 既定の置き場(保管庫の外)。
    pub fn open(workspace_id: &str) -> Result<Self> {
        let root = crate::app_data_dir()?.join("artifacts");
        Ok(Self::at(root, workspace_id))
    }

    /// 置き場を指定して開く(テストと、置き場を移したいとき)。
    pub fn at(root: PathBuf, workspace_id: &str) -> Self {
        Self {
            root,
            workspace_id: workspace_id.to_string(),
        }
    }

    /// 境界1つ分のディレクトリ。**保管庫 ID も挟む**ので、
    /// 別の保管庫の存在を訊くこともできない。
    fn boundary_dir(&self, sync: SyncPolicy) -> PathBuf {
        let name = match sync {
            SyncPolicy::Full => "full",
            SyncPolicy::LocalOnly => "local-only",
        };
        self.root.join(&self.workspace_id).join(name)
    }

    /// content-addressed な置き場所。先頭2文字で掘るのは1階層に溜めすぎないため。
    fn object_path(&self, sync: SyncPolicy, hash: &ContentHash) -> PathBuf {
        let h = hash.as_str();
        let (head, rest) = h.split_at(2);
        self.boundary_dir(sync)
            .join("objects")
            .join(head)
            .join(rest)
    }

    /// **その境界に**あるか。境界を跨いで訊く手段は用意しない。
    pub fn has(&self, sync: SyncPolicy, hash: &ContentHash) -> bool {
        self.object_path(sync, hash).is_file()
    }

    /// 実体を読む。無ければ `None`(呼び出し側が missing として扱う)。
    pub fn read(&self, sync: SyncPolicy, hash: &ContentHash) -> Result<Option<File>> {
        let path = self.object_path(sync, hash);
        match File::open(&path) {
            Ok(f) => Ok(Some(f)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("実体が読めない: {}", path.display())),
        }
    }

    /// この境界に実体があるか。`full` はここが持たないので常に false
    /// (判定は [`availability`] を通すこと)。
    fn has_local(&self, sync: SyncPolicy, hash: &ContentHash) -> bool {
        sync != SyncPolicy::Full && self.has(sync, hash)
    }

    /// 読みながら取り込む。**全量をメモリへ載せない**。
    ///
    /// 手順は 一時ファイルへ書く → 照合値を確定 → fsync → rename。
    /// 途中で落ちても、中途半端な object が置き場に現れない。
    pub fn import(&self, sync: SyncPolicy, src: &mut impl Read) -> Result<Imported> {
        self.import_limited(sync, src, sync.size_limits().map(|(_, max)| max))
    }

    /// 上限を明示して取り込む。上限は本来 `sync` から決まるが、
    /// 「読み切る前に断つ」挙動を小さなデータで確かめられるよう分けてある。
    fn import_limited(
        &self,
        sync: SyncPolicy,
        src: &mut impl Read,
        max: Option<u64>,
    ) -> Result<Imported> {
        if sync == SyncPolicy::Full {
            // 「本体も同期」の実体は LFS の置き場が持つ(ADR-0003 決定2 補足)。
            // ここにも置くと二重保存になる
            anyhow::bail!("full の実体は crate::lfs が持つ(ここでは扱わない)");
        }
        let dir = self.boundary_dir(sync);
        let tmp_dir = dir.join("tmp");
        fs::create_dir_all(&tmp_dir)?;
        let tmp = tmp_dir.join(new_ulid(0));

        let mut hasher = Hasher::new();
        let mut buf = vec![0u8; CHUNK];

        // 一時ファイルは失敗時に必ず消す(置き場にゴミを残さない)
        let result = (|| -> Result<()> {
            let mut out = File::create(&tmp)?;
            loop {
                let n = src.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                if let Some(max) = max {
                    // 上限は読み切る前に判定する。2GB を読み終えてから断らない
                    if hasher.len() > max {
                        return Err(ArtifactError::TooLarge {
                            size: hasher.len(),
                            limit: max,
                        }
                        .into());
                    }
                }
                out.write_all(&buf[..n])?;
            }
            out.sync_all()?;
            Ok(())
        })();
        if let Err(e) = result {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }

        let size = hasher.len();
        let hash = hasher.finish();
        let warn_over = match sync.check_size(size) {
            Ok(warn) => warn,
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                return Err(e.into());
            }
        };

        let dest = self.object_path(sync, &hash);
        if dest.is_file() {
            // 同じ境界に同じ内容が既にある。内容は不変なので上書きしない
            let _ = fs::remove_file(&tmp);
            return Ok(Imported {
                hash,
                size,
                warn_over,
                deduped: true,
            });
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&tmp, &dest).with_context(|| format!("置き場へ移せない: {}", dest.display()))?;
        Ok(Imported {
            hash,
            size,
            warn_over,
            deduped: false,
        })
    }

    /// ファイルから取り込む(選択・ドラッグ&ドロップ・CLI の入口)。
    pub fn import_path(&self, sync: SyncPolicy, src: &Path) -> Result<Imported> {
        let mut f = File::open(src).with_context(|| format!("読み込めない: {}", src.display()))?;
        self.import(sync, &mut f)
    }

    /// 置いてある実体を読み直して照合する。
    pub fn verify(&self, sync: SyncPolicy, hash: &ContentHash) -> Result<Verified> {
        let Some(mut f) = self.read(sync, hash)? else {
            return Ok(Verified::Missing);
        };
        let mut hasher = Hasher::new();
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(if &hasher.finish() == hash {
            Verified::Ok
        } else {
            Verified::Mismatch
        })
    }

    /// 境界ごとの件数と合計サイズ(ホームの内訳表示用)。
    /// **他の境界の数は混ぜない**。
    pub fn usage(&self, sync: SyncPolicy) -> (u64, u64) {
        let objects = self.boundary_dir(sync).join("objects");
        let mut count = 0;
        let mut bytes = 0;
        for entry in walkdir::WalkDir::new(objects)
            .into_iter()
            .filter_map(std::result::Result::ok)
        {
            if entry.file_type().is_file() {
                count += 1;
                bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
        (count, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{ArtifactId, Created, Policy, Role, Sensitivity};
    use tempfile::tempdir;

    /// 取得状態の判定に要る分だけの台帳。
    fn manifest_for(hash: ContentHash, locator: Locator) -> Manifest {
        Manifest::new(
            ArtifactId::new(1_755_000_000_000),
            hash,
            Created {
                media_type: "application/octet-stream".into(),
                size: 4,
                at: "2026-08-13T00:00:00Z".into(),
                origin: "test".into(),
                by: crate::OWNER_ACTOR.into(),
            },
            "名前".into(),
            locator,
            Policy::default_managed(),
            Role::File,
        )
    }

    fn stores(dir: &tempfile::TempDir, ws: &str) -> Stores {
        Stores::at(dir.path().join("artifacts"), ws)
    }

    #[test]
    fn import_is_content_addressed_and_verifies() {
        let dir = tempdir().unwrap();
        let s = stores(&dir, "ws-a");
        let out = s.import(SyncPolicy::LocalOnly, &mut &b"hello"[..]).unwrap();

        assert_eq!(out.hash, ContentHash::of_bytes(b"hello"));
        assert_eq!(out.size, 5);
        assert!(!out.deduped);
        assert!(s.has(SyncPolicy::LocalOnly, &out.hash));
        assert_eq!(
            s.verify(SyncPolicy::LocalOnly, &out.hash).unwrap(),
            Verified::Ok
        );
    }

    #[test]
    fn same_content_in_same_boundary_is_deduped() {
        let dir = tempdir().unwrap();
        let s = stores(&dir, "ws-a");
        let first = s.import(SyncPolicy::LocalOnly, &mut &b"same"[..]).unwrap();
        let second = s.import(SyncPolicy::LocalOnly, &mut &b"same"[..]).unwrap();
        assert_eq!(first.hash, second.hash);
        assert!(!first.deduped);
        assert!(second.deduped);
        assert_eq!(s.usage(SyncPolicy::LocalOnly).0, 1);
    }

    #[test]
    fn workspaces_do_not_share_objects() {
        let dir = tempdir().unwrap();
        let a = stores(&dir, "ws-a");
        let b = stores(&dir, "ws-b");
        let out = a
            .import(SyncPolicy::LocalOnly, &mut &b"shared bytes"[..])
            .unwrap();
        assert!(a.has(SyncPolicy::LocalOnly, &out.hash));
        assert!(!b.has(SyncPolicy::LocalOnly, &out.hash));
    }

    #[test]
    fn oversized_import_aborts_before_reading_everything() {
        /// 読んだ量を数える。上限超過で「読み切ってから断る」実装だと全量が読まれる
        struct Counting<'a> {
            data: &'a [u8],
            read: usize,
        }
        impl Read for Counting<'_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = (self.data.len() - self.read).min(buf.len());
                buf[..n].copy_from_slice(&self.data[self.read..self.read + n]);
                self.read += n;
                Ok(n)
            }
        }

        let dir = tempdir().unwrap();
        let s = stores(&dir, "ws-a");
        let data = vec![7u8; CHUNK * 4];
        let mut src = Counting {
            data: &data,
            read: 0,
        };

        let err = s
            .import_limited(SyncPolicy::LocalOnly, &mut src, Some(CHUNK as u64))
            .unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<ArtifactError>(),
                Some(ArtifactError::TooLarge { .. })
            ),
            "{err}"
        );
        // 上限の2塊ぶんまでで止まっている(全量 4 塊は読んでいない)
        assert!(src.read <= CHUNK * 2, "読み過ぎ: {}", src.read);
        assert_eq!(s.usage(SyncPolicy::LocalOnly), (0, 0));
        let tmp = s.boundary_dir(SyncPolicy::LocalOnly).join("tmp");
        assert!(
            fs::read_dir(tmp).unwrap().next().is_none(),
            "一時ファイルが残っている"
        );
    }

    #[test]
    fn no_size_limit_outside_full() {
        let dir = tempdir().unwrap();
        let s = stores(&dir, "ws-a");
        assert_eq!(SyncPolicy::LocalOnly.size_limits(), None);
        let data = vec![1u8; CHUNK + 1];
        assert!(s.import(SyncPolicy::LocalOnly, &mut &data[..]).is_ok());
    }

    #[test]
    fn failed_read_leaves_no_object_and_no_tmp() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("読み取り失敗"))
            }
        }
        let dir = tempdir().unwrap();
        let s = stores(&dir, "ws-a");
        assert!(s.import(SyncPolicy::LocalOnly, &mut Broken).is_err());
        assert_eq!(s.usage(SyncPolicy::LocalOnly), (0, 0));
        let tmp = s.boundary_dir(SyncPolicy::LocalOnly).join("tmp");
        let left: Vec<_> = fs::read_dir(tmp).unwrap().collect();
        assert!(left.is_empty(), "一時ファイルが残っている");
    }

    #[test]
    fn tampered_object_is_reported_as_mismatch() {
        let dir = tempdir().unwrap();
        let s = stores(&dir, "ws-a");
        let out = s
            .import(SyncPolicy::LocalOnly, &mut &b"original"[..])
            .unwrap();
        fs::write(s.object_path(SyncPolicy::LocalOnly, &out.hash), b"tampered").unwrap();
        assert_eq!(
            s.verify(SyncPolicy::LocalOnly, &out.hash).unwrap(),
            Verified::Mismatch
        );
    }

    #[test]
    fn missing_object_verifies_as_missing() {
        let dir = tempdir().unwrap();
        let s = stores(&dir, "ws-a");
        let absent = ContentHash::of_bytes(b"never imported");
        assert_eq!(
            s.verify(SyncPolicy::LocalOnly, &absent).unwrap(),
            Verified::Missing
        );
    }

    /// 取得状態は台帳から読まず、**locator と区分から算出する**。
    /// 区分だけで分岐していた頃は、保管庫の中に実物がある旧添付まで
    /// missing になっていた(2026-08-13 に移行の実装で判明)。
    #[test]
    fn availability_is_derived_from_the_locator_not_stored() {
        let dir = tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let s = stores(&dir, "ws-a");
        let out = s.import(SyncPolicy::LocalOnly, &mut &b"here"[..]).unwrap();

        let m = |hash: ContentHash, locator: Locator, client_repo: bool| {
            let mut manifest = manifest_for(hash.clone(), locator);
            manifest.policy = Policy {
                sensitivity: Sensitivity::Private,
                sync: SyncPolicy::LocalOnly,
                client_repo,
            };
            manifest
        };

        let managed = Locator::Managed {
            hash: out.hash.clone(),
        };
        assert_eq!(
            availability(&vault, &s, &m(out.hash.clone(), managed.clone(), false)),
            Availability::Local
        );

        let elsewhere = ContentHash::of_bytes(b"not here");
        assert_eq!(
            availability(
                &vault,
                &s,
                &m(
                    elsewhere.clone(),
                    Locator::Managed {
                        hash: elsewhere.clone()
                    },
                    false
                )
            ),
            Availability::Missing
        );

        // client repo 由来でも managed + local_only なら、この端末の複製を開ける
        assert_eq!(
            availability(&vault, &s, &m(out.hash.clone(), managed, true)),
            Availability::Local
        );

        // 旧添付は保管庫の中の実ファイルが実体。置き場を見ても見つからない
        let note_id = "notes/旧";
        let files = vault.attach_dir(note_id).unwrap();
        fs::create_dir_all(&files).unwrap();
        fs::write(files.join("図.png"), b"legacy bytes").unwrap();
        let legacy = Locator::LegacyGit {
            note_id: note_id.into(),
            file_name: "図.png".into(),
        };
        assert_eq!(
            availability(
                &vault,
                &s,
                &m(ContentHash::of_bytes(b"legacy bytes"), legacy, false)
            ),
            Availability::Local
        );

        // 元の場所を指すだけのものは在否を確かめられない(Local と言わない)
        let linked = Locator::Linked {
            repo_id: "github.com/acme/widgets".into(),
            rel_path: "README.md".into(),
        };
        assert_eq!(
            availability(&vault, &s, &m(out.hash, linked, false)),
            Availability::Missing
        );
    }

    #[test]
    fn full_is_owned_by_lfs_not_here() {
        let dir = tempdir().unwrap();
        let s = stores(&dir, "ws-a");
        let err = s.import(SyncPolicy::Full, &mut &b"x"[..]).unwrap_err();
        assert!(err.to_string().contains("lfs"), "{err}");
    }

    #[test]
    fn import_from_path_matches_streaming() {
        let dir = tempdir().unwrap();
        let s = stores(&dir, "ws-a");
        let src = dir.path().join("src.bin");
        // 塊の境界をまたぐ大きさで、分割読みでも同じ値になることを見る
        let data: Vec<u8> = (0..(CHUNK * 2 + 7)).map(|i| (i % 251) as u8).collect();
        fs::write(&src, &data).unwrap();

        let out = s.import_path(SyncPolicy::LocalOnly, &src).unwrap();
        assert_eq!(out.hash, ContentHash::of_bytes(&data));
        assert_eq!(out.size, data.len() as u64);
        assert_eq!(
            s.verify(SyncPolicy::LocalOnly, &out.hash).unwrap(),
            Verified::Ok
        );
    }
}
