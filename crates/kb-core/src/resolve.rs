//! リンクの解決 — ADR-0003 決定5。
//!
//! 3つの形を受ける:
//!
//! - `kb-artifact-ref:<名前>` … 本文中の通常リンク。**最新版に追従する**
//! - `kb-artifact:<id>` … 出典(`sources[].resource`)。**版を固定する**
//! - `/…files/…` … 旧サイドカーのリンク。**本文を書き換えず**対応表で辿る
//!
//! 通常リンクを ref に、出典を id にするのは、証拠としての出典が後から
//! 別の内容へ変わらないようにするため(本人判断 2026-08-12)。
//!
//! ## 手元に無いものの中身を渡さない
//!
//! 「local でない場合、AI はファイル名・過去の OCR・caption から内容を推測しては
//! ならない」は文章で守る規律ではない。ここでは [`Resolved::open`] が
//! **`Local` のときしか中身を返さない**ようにして、経路そのものを塞ぐ。

use std::fs::File;
use std::str::FromStr;

use anyhow::Result;

use crate::artifact::{ArtifactId, Locator, Manifest, RefName, SyncPolicy};
use crate::ledger::Ledger;
use crate::store::{Availability, Stores, availability};
use crate::vault::Vault;

/// 本文や frontmatter に書かれた参照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// 最新版に追従する
    Ref(RefName),
    /// 版を固定する
    Fixed(ArtifactId),
    /// 旧サイドカーのリンク(移行前に書かれた本文)
    Legacy(String),
}

/// 解決した結果。**中身は `open` を通してしか取れない。**
#[derive(Debug, Clone)]
pub struct Resolved {
    pub manifest: Manifest,
    pub availability: Availability,
}

impl Resolved {
    /// 中身を開く。**手元に無ければ `None`。**
    ///
    /// 呼び出し側が availability を見忘れても、中身が漏れない形にしてある
    /// (検索・`get`・Context Pack はすべてここを通る)。
    pub fn open(&self, vault: &Vault, stores: &Stores) -> Result<Option<File>> {
        if self.availability != Availability::Local {
            return Ok(None);
        }
        // 実体の持ち主は locator が決める(availability と同じ理由で区分では分岐しない)
        match &self.manifest.locator {
            Locator::Managed { .. } => match self.manifest.policy.sync {
                SyncPolicy::Full => crate::lfs::read(vault, &self.manifest.hash),
                other => stores.read(other, &self.manifest.hash),
            },
            Locator::LegacyGit { note_id, file_name } => {
                let path = vault.attach_dir(note_id).join(file_name);
                Ok(File::open(path).ok())
            }
            // 指すだけで実体を持たない。手元のパスを引く仕組みが無い(残課題)
            Locator::Linked { .. } => Ok(None),
        }
    }

    /// 取り寄せを提案してよいか。**方針で閉じているものには提案しない。**
    pub fn can_fetch(&self) -> bool {
        self.availability == Availability::Missing && self.manifest.policy.sync == SyncPolicy::Full
    }
}

/// 書かれた文字列を参照として読む。読めなければ `None`(ただの URL や相対パス)。
pub fn parse_link(raw: &str) -> Option<Link> {
    let s = raw.trim();
    if let Some(name) = s.strip_prefix("kb-artifact-ref:") {
        return RefName::from_str(name).ok().map(Link::Ref);
    }
    if let Some(id) = s.strip_prefix("kb-artifact:") {
        return ArtifactId::from_str(id).ok().map(Link::Fixed);
    }
    // 旧サイドカー: /<note-id>.files/<name>
    if s.starts_with('/') && s.contains(".files/") {
        return Some(Link::Legacy(s.to_string()));
    }
    None
}

/// 解決する。見つからなければ `None`(壊れたリンクは呼び出し側が気づきに出す)。
pub fn resolve(
    vault: &Vault,
    stores: &Stores,
    ledger: &Ledger,
    link: &Link,
) -> Result<Option<Resolved>> {
    let manifest = match link {
        Link::Ref(name) => match ledger.get_ref(name)? {
            Some(r) => ledger.get(&r.artifact_id)?,
            None => None,
        },
        // **ref を経由しない。** 出典は版が動いてはいけない
        Link::Fixed(id) => ledger.get(id)?,
        Link::Legacy(path) => match ledger.alias(path) {
            Some(name) => match ledger.get_ref(&name)? {
                Some(r) => ledger.get(&r.artifact_id)?,
                None => None,
            },
            None => None,
        },
    };
    let Some(manifest) = manifest else {
        return Ok(None);
    };
    let availability = availability(vault, stores, &manifest);
    Ok(Some(Resolved {
        manifest,
        availability,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{ArtifactRef, Created, Locator, Policy, Role, Sensitivity};
    use std::fs;
    use tempfile::{TempDir, tempdir};

    struct Env {
        _dir: TempDir,
        vault: Vault,
        stores: Stores,
        ledger: Ledger,
    }

    fn env() -> Env {
        let dir = tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let stores = Stores::at(dir.path().join("artifacts"), "ws-a");
        let ledger = Ledger::at(vault.root.clone(), dir.path().join("sidecar"));
        Env {
            _dir: dir,
            vault,
            stores,
            ledger,
        }
    }

    /// 「情報のみ」で1件置く(full は LFS が要るので、解決の筋を見るにはこれで足りる)
    fn put(e: &Env, bytes: &[u8], name: &str) -> Manifest {
        let imported = e
            .stores
            .import(SyncPolicy::ManifestOnly, &mut &bytes[..])
            .unwrap();
        let m = Manifest::new(
            ArtifactId::new(1_755_000_000_000),
            imported.hash.clone(),
            Created {
                media_type: "application/octet-stream".into(),
                size: imported.size,
                at: "2026-08-12T09:04:00Z".into(),
                origin: "test".into(),
                by: crate::OWNER_ACTOR.into(),
            },
            name.into(),
            Locator::Managed {
                hash: imported.hash,
            },
            Policy {
                sensitivity: Sensitivity::Private,
                sync: SyncPolicy::ManifestOnly,
                client_repo: false,
            },
            Role::File,
        );
        e.ledger.put(&e.vault, &m).unwrap();
        m
    }

    #[test]
    fn parses_the_three_shapes_and_ignores_others() {
        assert_eq!(
            parse_link("kb-artifact-ref:sketch"),
            Some(Link::Ref(RefName::from_str("sketch").unwrap()))
        );
        let id = ArtifactId::new(1_755_000_000_000);
        assert_eq!(
            parse_link(&format!("kb-artifact:{id}")),
            Some(Link::Fixed(id))
        );
        assert_eq!(
            parse_link("/notes/foo.files/img.png"),
            Some(Link::Legacy("/notes/foo.files/img.png".into()))
        );
        // 普通のリンクは拾わない
        assert_eq!(parse_link("https://example.com/x.png"), None);
        assert_eq!(parse_link("/notes/other.md"), None);
        assert_eq!(parse_link("kb-artifact-ref:Bad Name"), None);
    }

    #[test]
    fn a_body_link_follows_the_latest_version() {
        let e = env();
        let first = put(&e, b"v1", "図.png");
        let name = RefName::from_str("diagram").unwrap();
        let mut r = ArtifactRef::new("ws-a", name.clone(), first.id.clone());
        e.ledger
            .put_ref(&e.vault, SyncPolicy::ManifestOnly, &r)
            .unwrap();

        let link = Link::Ref(name.clone());
        let got = resolve(&e.vault, &e.stores, &e.ledger, &link)
            .unwrap()
            .unwrap();
        assert_eq!(got.manifest.id, first.id);

        // 新しい版を足して参照を移すと、本文のリンクは自動で追従する
        let second = put(&e, b"v2", "図.png");
        r.point_to(1, second.id.clone()).unwrap();
        e.ledger
            .put_ref(&e.vault, SyncPolicy::ManifestOnly, &r)
            .unwrap();
        let got = resolve(&e.vault, &e.stores, &e.ledger, &link)
            .unwrap()
            .unwrap();
        assert_eq!(got.manifest.id, second.id, "本文リンクが追従していない");
    }

    #[test]
    fn a_citation_stays_pinned_to_its_version() {
        let e = env();
        let first = put(&e, b"v1", "表.csv");
        let name = RefName::from_str("table").unwrap();
        let mut r = ArtifactRef::new("ws-a", name, first.id.clone());
        e.ledger
            .put_ref(&e.vault, SyncPolicy::ManifestOnly, &r)
            .unwrap();

        let second = put(&e, b"v2", "表.csv");
        r.point_to(1, second.id.clone()).unwrap();
        e.ledger
            .put_ref(&e.vault, SyncPolicy::ManifestOnly, &r)
            .unwrap();

        // 出典は動かない。証拠が後から別の内容に変わってはいけない
        let cited = resolve(
            &e.vault,
            &e.stores,
            &e.ledger,
            &Link::Fixed(first.id.clone()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(cited.manifest.id, first.id);
        assert_ne!(cited.manifest.id, second.id);
    }

    #[test]
    fn an_old_link_resolves_through_the_alias_table() {
        let e = env();
        let m = put(&e, b"legacy", "旧図.png");
        let name = RefName::from_str("old-diagram").unwrap();
        let r = ArtifactRef::new("ws-a", name.clone(), m.id.clone());
        e.ledger
            .put_ref(&e.vault, SyncPolicy::ManifestOnly, &r)
            .unwrap();

        let legacy = "/notes/foo.files/旧図.png";
        // 対応表を作る前は解決しない(壊れたリンクとして気づきに出す)
        let link = parse_link(legacy).unwrap();
        assert!(
            resolve(&e.vault, &e.stores, &e.ledger, &link)
                .unwrap()
                .is_none()
        );

        e.ledger.put_alias(&e.vault, legacy, &name).unwrap();
        let got = resolve(&e.vault, &e.stores, &e.ledger, &link)
            .unwrap()
            .unwrap();
        assert_eq!(got.manifest.id, m.id);
    }

    #[test]
    fn content_is_unreachable_when_it_is_not_here() {
        let e = env();
        let mut m = put(&e, b"bytes", "x.bin");
        let name = RefName::from_str("x").unwrap();
        let r = ArtifactRef::new("ws-a", name.clone(), m.id.clone());
        e.ledger
            .put_ref(&e.vault, SyncPolicy::ManifestOnly, &r)
            .unwrap();

        // 手元にある間は開ける
        let got = resolve(&e.vault, &e.stores, &e.ledger, &Link::Ref(name.clone()))
            .unwrap()
            .unwrap();
        assert_eq!(got.availability, Availability::Local);
        assert!(got.open(&e.vault, &e.stores).unwrap().is_some());

        // 実体だけ消す(別の端末で取り込まれた状態と同じ)
        remove_object(&e, &m);

        let got = resolve(&e.vault, &e.stores, &e.ledger, &Link::Ref(name.clone()))
            .unwrap()
            .unwrap();
        assert_eq!(got.availability, Availability::Missing);
        // **中身は取れない。** 呼び出し側が availability を見忘れても漏れない
        assert!(got.open(&e.vault, &e.stores).unwrap().is_none());

        // 仕事のリポジトリ由来は、取り寄せの提案もしない
        m.policy.client_repo = true;
        m.policy.sync = SyncPolicy::ManifestOnly;
        e.ledger.put(&e.vault, &m).unwrap();
        let got = resolve(&e.vault, &e.stores, &e.ledger, &Link::Ref(name))
            .unwrap()
            .unwrap();
        assert_eq!(got.availability, Availability::UnavailableByPolicy);
        assert!(!got.can_fetch());
        assert!(got.open(&e.vault, &e.stores).unwrap().is_none());
    }

    fn remove_object(e: &Env, m: &Manifest) {
        let h = m.hash.as_str();
        let (head, rest) = h.split_at(2);
        let p = e
            ._dir
            .path()
            .join("artifacts")
            .join("ws-a")
            .join("manifest-only")
            .join("objects")
            .join(head)
            .join(rest);
        fs::remove_file(p).unwrap();
    }

    #[test]
    fn a_broken_link_is_none_not_an_error() {
        let e = env();
        let missing = RefName::from_str("nope").unwrap();
        assert!(
            resolve(&e.vault, &e.stores, &e.ledger, &Link::Ref(missing))
                .unwrap()
                .is_none()
        );
    }
}
