//! ファイルの取り込み — ADR-0003 決定6・contract.md 契約5。
//!
//! **path を扱う取り込み経路(選択・ドラッグ&ドロップ・貼り付け・CLI)は
//! ここへ合流する。** MCP には別の content 経路だけを公開し、同じ store / ledger 処理へ
//! 合流させる。現行の添付は画面側から実体書き込みを直接呼べてしまい、
//! ダイアログを通らない経路が存在する。判定を UI に置くと、その UI を通らない
//! 経路の数だけ穴が空くので、**拒否はここで行い、画面の無効化は補助**とする。
//!
//! ここが引き受ける判断は2つ:
//!
//! 1. **仕事のリポジトリの中にあるか**(あれば「同期しない」に固定し、緩められなくする)
//! 2. どの置き場へ入れるか(区分ごとに物理的に分かれている)

use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::artifact::{
    ArtifactId, ArtifactRef, Created, Locator, Manifest, Policy, RefName, Role, SyncPolicy,
};
use crate::ledger::Ledger;
use crate::store::Stores;
use crate::vault::Vault;

/// 取り込みの指定。区分を指定しなくても、場所から安全側の既定が決まる。
#[derive(Debug, Clone)]
pub struct Request {
    /// ひもづけ先のノート。**ファイルはノートの持ち物**として現れる
    pub note_id: Option<String>,
    pub display_name: String,
    pub media_type: String,
    pub role: Role,
    /// 未指定なら既定。**仕事のリポジトリ内なら指定に関わらず固定される**
    pub policy: Option<Policy>,
    pub ref_name: Option<RefName>,
    /// 差し替え元。**内容は不変**なので、中身の更新は新しい台帳になる(ADR-0003 決定7)
    pub supersedes: Option<ArtifactId>,
    /// どこから来たか(会話・取り込み・移行など)
    pub origin: String,
    pub by: String,
    /// RFC3339
    pub at: String,
}

/// 取り込んだ結果。
#[derive(Debug, Clone)]
pub struct Taken {
    pub manifest: Manifest,
    pub artifact_ref: Option<ArtifactRef>,
    /// 大きさの警告(拒否ではない)
    pub warn_over: Option<u64>,
    /// 場所から区分を固定した(画面はこの理由を出す)
    pub forced_local_only: bool,
}

/// `src` を含む Git リポジトリを探す。保管庫自身は client repo とみなさない。
fn is_client_repo(vault: &Vault, src: &Path) -> bool {
    let Ok(vault_root) = vault.root.canonicalize() else {
        return false;
    };
    let Ok(start) = src.canonicalize() else {
        return false;
    };
    let Some(mut cur) = start.parent() else {
        return false;
    };
    loop {
        if cur.join(".git").exists() {
            return cur != vault_root;
        }
        let Some(parent) = cur.parent() else {
            return false;
        };
        cur = parent;
    }
}

/// 取り込む。**この関数を通らない書き込み経路を作らないこと。**
pub fn take(
    vault: &Vault,
    stores: &Stores,
    ledger: &Ledger,
    workspace_id: &str,
    src: &Path,
    req: Request,
) -> Result<Taken> {
    let client_repo = is_client_repo(vault, src);

    // 0. 差し替え元。無ければ普通の新規取り込み
    let previous = match &req.supersedes {
        Some(id) => Some(
            ledger
                .get(id)?
                .with_context(|| format!("差し替え元が台帳に無い: {id}"))?,
        ),
        None => None,
    };

    // 1. 区分を決める。仕事のリポジトリ内なら、指定に関わらず固定する
    let requested = match &previous {
        // 版を重ねるときは前の版の区分を引き継ぎ、req.policy を見ない。
        // 見ると「新しい版として追加」が持ち出し範囲を広げる裏口になる
        // (決定10 は緩和を対話的な確認の後ろにしか置かない)
        Some(prev) => prev.policy,
        None => req.policy.unwrap_or_else(Policy::default_managed),
    };
    let (policy, forced_local_only) = if client_repo {
        (
            Policy::client_repo_locked(),
            requested.sync != SyncPolicy::LocalOnly,
        )
    } else if previous.is_some() {
        // 差し替え元の印も含めて区分を丸ごと引き継ぐ。
        // client repo 由来の旧版を一度 repo 外へコピーしてから差し替えることで
        // hard gate を外せる抜け道を作らない。
        (requested, false)
    } else {
        (
            Policy {
                client_repo: false,
                ..requested
            },
            false,
        )
    };

    // 2. 前の版を指していた参照は、新しい版へ付け替える(本文リンクは最新へ追従する)
    let inherited = previous.as_ref().and_then(|prev| ledger.ref_for(&prev.id));

    // 参照名の衝突は、上書きせず呼び出し側へ返す(別名を提案するのは UI)。
    // ただし付け替え先が自分自身なら衝突ではない
    if let Some(name) = &req.ref_name
        && ledger.ref_taken(name)
        && inherited.as_ref().is_none_or(|r| r.name != *name)
    {
        bail!("参照名 {name} は使われている");
    }

    let (locator, hash, size, warn_over) = if policy.sync == SyncPolicy::Full {
        // 「本体も同期」の実体は LFS の置き場が持つ(自前 CAS には置かない)。
        // 大きさは複製する前に見る — 2GB を写してから断らない
        let size = std::fs::metadata(src)
            .with_context(|| format!("読めない: {}", src.display()))?
            .len();
        let warn_over = policy.sync.check_size(size)?;
        let (hash, size) = crate::lfs::import(vault, src)?;
        (
            Locator::Managed { hash: hash.clone() },
            hash,
            size,
            warn_over,
        )
    } else {
        let imported = stores.import_path(policy.sync, src)?;
        (
            Locator::Managed {
                hash: imported.hash.clone(),
            },
            imported.hash,
            imported.size,
            imported.warn_over,
        )
    };

    let id = ArtifactId::new(unix_ms(&req.at));
    let created = Created {
        media_type: req.media_type.clone(),
        size,
        at: req.at.clone(),
        origin: req.origin.clone(),
        by: req.by.clone(),
    };
    let mut manifest = match &previous {
        Some(prev) => {
            // 直せる部分は前の版から引き継ぐ。**表示名も引き継ぐ** —
            // 版を重ねても行の見え方が変わらないほうが同じ物として追える。
            // 取り込んだファイル名は下の来歴に残るので失われない
            let mut next = prev.succeed(id, hash, created);
            next.locator = locator;
            next.policy = policy;
            next
        }
        None => Manifest::new(
            id,
            hash,
            created,
            req.display_name.clone(),
            locator,
            policy,
            req.role,
        ),
    };
    if let Some(note_id) = &req.note_id
        && !manifest.notes.iter().any(|n| n == note_id)
    {
        manifest.notes.push(note_id.clone());
    }
    manifest.record(&req.at, "imported", &req.origin);
    if let Some(prev) = &previous {
        manifest.record(
            &req.at,
            "superseded",
            &format!(
                "前の版 {} を差し替え(取り込んだ名前 {})",
                prev.id, req.display_name
            ),
        );
    }
    if forced_local_only {
        manifest.record(
            &req.at,
            "policy-forced",
            "仕事のリポジトリの中にあるため、同期しない設定に固定した",
        );
    }
    ledger.put(vault, &manifest)?;

    let artifact_ref = match inherited {
        // 前の版を指していた参照を最新版へ向け直す。**名前は変えない** —
        // 参照名を変えると本文リンクが切れるので、それは詳細画面の明示操作にする
        Some(mut r) => {
            r.point_to(r.revision, manifest.id.clone())?;
            ledger.put_ref(vault, policy.sync, &r)?;
            Some(r)
        }
        None => match req.ref_name {
            Some(name) => {
                let r = ArtifactRef::new(workspace_id, name, manifest.id.clone());
                ledger.put_ref(vault, policy.sync, &r)?;
                Some(r)
            }
            None => None,
        },
    };

    Ok(Taken {
        manifest,
        artifact_ref,
        warn_over,
        forced_local_only,
    })
}

/// 拡張子から媒体種別を推定する。分からなければ `application/octet-stream`。
///
/// 呼び口(画面・CLI・MCP)ごとに書くと表記が割れるのでここに置く。
/// 中身は見ない — 取り込みは streaming なので、判定のために全量を読まない。
pub fn guess_media_type(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "heic" => "image/heic",
        "pdf" => "application/pdf",
        "csv" => "text/csv",
        "md" | "markdown" => "text/markdown",
        "txt" | "log" => "text/plain",
        "json" => "application/json",
        "yaml" | "yml" => "application/yaml",
        "zip" => "application/zip",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// RFC3339 から ULID 用のミリ秒。読めなければ 0(ID の一意性は乱数側が担保する)。
fn unix_ms(at: &str) -> u64 {
    time::OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339)
        .map(|t| (t.unix_timestamp_nanos() / 1_000_000).max(0) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{ArtifactError, Sensitivity};
    use std::fs;
    use std::path::PathBuf;
    use std::str::FromStr;
    use tempfile::{TempDir, tempdir};

    struct Env {
        _dir: TempDir,
        vault: Vault,
        stores: Stores,
        ledger: Ledger,
        root: PathBuf,
    }

    fn env() -> Env {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let vault = Vault::create(root.join("v")).unwrap();
        let stores = Stores::at(root.join("artifacts"), "ws-a");
        let ledger = Ledger::at(vault.root.clone(), root.join("sidecar"));
        Env {
            _dir: dir,
            vault,
            stores,
            ledger,
            root,
        }
    }

    fn req() -> Request {
        Request {
            note_id: Some("notes/decision".into()),
            display_name: "表.csv".into(),
            media_type: "text/csv".into(),
            role: Role::File,
            policy: None,
            ref_name: None,
            supersedes: None,
            origin: "conversation".into(),
            by: crate::OWNER_ACTOR.into(),
            at: "2026-08-12T09:04:00Z".into(),
        }
    }

    /// remote つきの「仕事のリポジトリ」を作る
    fn client_repo(root: &Path, name: &str, with_remote: bool) -> PathBuf {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        let repo = git2::Repository::init(&dir).unwrap();
        if with_remote {
            repo.remote("origin", "git@github.com:acme/widgets.git")
                .unwrap();
        }
        let file = dir.join("data").join("table.csv");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, b"id,name\n1,a\n").unwrap();
        file
    }

    fn take_at(e: &Env, src: &Path, r: Request) -> Result<Taken> {
        take(&e.vault, &e.stores, &e.ledger, "ws-a", src, r)
    }

    #[test]
    fn ordinary_file_is_copied_and_kept_private() {
        let e = env();
        let src = e.root.join("photo.png");
        fs::write(&src, b"png bytes").unwrap();

        let out = take_at(&e, &src, req()).unwrap();
        assert_eq!(out.manifest.policy.sensitivity, Sensitivity::Private);
        assert_eq!(out.manifest.policy.sync, SyncPolicy::Full);
        assert!(!out.manifest.policy.client_repo);
        assert!(!out.forced_local_only);
        assert!(matches!(out.manifest.locator, Locator::Managed { .. }));
        // 「本体も同期」の実体は LFS の置き場が持つ。自前 CAS には**入れない**
        assert!(!e.stores.has(SyncPolicy::Full, &out.manifest.hash));
        assert!(e.ledger.get(&out.manifest.id).unwrap().is_some());
        if crate::connect::ensure_vault_config(&e.vault).is_ok() {
            assert!(crate::lfs::has(&e.vault, &out.manifest.hash));
        }
    }

    #[test]
    fn file_in_a_client_repo_is_pinned_even_when_full_is_requested() {
        let e = env();
        let src = client_repo(&e.root, "work", true);

        let mut r = req();
        // 画面をすり抜けて「本体も同期」を渡してきた場合
        r.policy = Some(Policy {
            sensitivity: Sensitivity::Shared,
            sync: SyncPolicy::Full,
            client_repo: false,
        });

        let out = take_at(&e, &src, r).unwrap();
        assert!(out.manifest.policy.client_repo);
        assert_eq!(out.manifest.policy.sync, SyncPolicy::LocalOnly);
        assert_eq!(out.manifest.policy.sensitivity, Sensitivity::Private);
        assert!(out.forced_local_only);
        assert!(matches!(out.manifest.locator, Locator::Managed { .. }));
        assert!(e.stores.has(SyncPolicy::LocalOnly, &out.manifest.hash));
        // 固定した理由が来歴に残る
        assert!(
            out.manifest
                .events
                .iter()
                .any(|ev| ev.kind == "policy-forced")
        );
        // あとから緩めることもできない
        let wider = Policy {
            sync: SyncPolicy::Full,
            ..out.manifest.policy
        };
        assert_eq!(
            out.manifest.policy.check_change(&wider, true),
            Err(ArtifactError::ClientRepoLocked)
        );
    }

    #[test]
    fn client_repo_file_is_copied_into_the_local_only_store() {
        let e = env();
        let src = client_repo(&e.root, "work", true);
        let out = take_at(&e, &src, req()).unwrap();

        assert!(matches!(out.manifest.locator, Locator::Managed { .. }));
        assert!(e.stores.has(SyncPolicy::LocalOnly, &out.manifest.hash));
    }

    #[test]
    fn client_repo_without_remote_is_also_copied() {
        let e = env();
        let src = client_repo(&e.root, "work", false);

        let out = take_at(&e, &src, req()).unwrap();
        assert_eq!(out.manifest.policy.sync, SyncPolicy::LocalOnly);
        assert!(matches!(out.manifest.locator, Locator::Managed { .. }));
        assert!(e.stores.has(SyncPolicy::LocalOnly, &out.manifest.hash));
        assert!(!e.stores.has(SyncPolicy::Full, &out.manifest.hash));
    }

    #[test]
    fn the_vault_itself_is_not_a_client_repo() {
        let e = env();
        let src = e.vault.root.join("note.files").join("img.png");
        fs::create_dir_all(src.parent().unwrap()).unwrap();
        fs::write(&src, b"img").unwrap();

        assert!(!is_client_repo(&e.vault, &src));
        let out = take_at(&e, &src, req()).unwrap();
        assert!(!out.manifest.policy.client_repo);
        assert_eq!(out.manifest.policy.sync, SyncPolicy::Full);
    }

    #[test]
    fn ref_name_collision_is_reported_not_overwritten() {
        let e = env();
        let a = e.root.join("a.png");
        let b = e.root.join("b.png");
        fs::write(&a, b"a").unwrap();
        fs::write(&b, b"b").unwrap();
        let name = RefName::from_str("sketch").unwrap();

        let mut r = req();
        r.ref_name = Some(name.clone());
        let first = take_at(&e, &a, r).unwrap();

        let mut r2 = req();
        r2.ref_name = Some(name.clone());
        assert!(take_at(&e, &b, r2).is_err());

        // 先に取った方を指したまま
        let current = e.ledger.get_ref(&name).unwrap().unwrap();
        assert_eq!(current.artifact_id, first.manifest.id);
    }

    #[test]
    fn transcript_role_is_kept_out_of_search() {
        let e = env();
        let src = e.root.join("talk.md");
        fs::write(&src, "# 会話".as_bytes()).unwrap();
        let mut r = req();
        r.role = Role::Transcript;

        let out = take_at(&e, &src, r).unwrap();
        assert!(!out.manifest.retrievable);
    }

    #[test]
    fn the_file_belongs_to_the_note_it_was_added_to() {
        let e = env();
        let src = e.root.join("p.png");
        fs::write(&src, b"p").unwrap();
        let out = take_at(&e, &src, req()).unwrap();
        assert_eq!(out.manifest.notes, vec!["notes/decision"]);
        assert_eq!(e.ledger.list_for_note("notes/decision").len(), 1);
        assert!(e.ledger.list_for_note("notes/other").is_empty());
    }

    #[test]
    fn provenance_records_how_it_arrived() {
        let e = env();
        let src = e.root.join("x.bin");
        fs::write(&src, b"x").unwrap();
        let out = take_at(&e, &src, req()).unwrap();
        assert_eq!(out.manifest.events.len(), 1);
        assert_eq!(out.manifest.events[0].kind, "imported");
        assert_eq!(out.manifest.created.origin, "conversation");
    }

    #[test]
    fn a_new_version_is_a_new_record_that_points_back() {
        let e = env();
        let first = e.root.join("図.png");
        let second = e.root.join("図-改.png");
        fs::write(&first, b"v1").unwrap();
        fs::write(&second, b"v2").unwrap();

        let old = take_at(&e, &first, req()).unwrap();
        let mut r = req();
        r.display_name = "図-改.png".into();
        r.supersedes = Some(old.manifest.id.clone());
        let new = take_at(&e, &second, r).unwrap();

        assert_eq!(new.manifest.supersedes, Some(old.manifest.id.clone()));
        // 元の版は書き換わらない(内容は不変 — 決定7)
        let kept = e.ledger.get(&old.manifest.id).unwrap().unwrap();
        assert_eq!(kept, old.manifest);
        // 行の見え方は変えず、取り込んだ名前は来歴に残す
        assert_eq!(new.manifest.display_name, "表.csv");
        assert!(
            new.manifest
                .events
                .iter()
                .any(|ev| ev.kind == "superseded" && ev.detail.contains("図-改.png"))
        );
        // ひもづくノートは引き継ぐ(重複させない)
        assert_eq!(new.manifest.notes, vec!["notes/decision"]);
    }

    /// 「新しい版として追加」が持ち出し範囲を広げる裏口にならないこと。
    /// 画面をすり抜けて緩い区分を渡してきても、前の版の区分を引き継ぐ。
    #[test]
    fn a_new_version_cannot_widen_the_boundary() {
        let e = env();
        let src = e.root.join("秘.txt");
        fs::write(&src, b"v1").unwrap();

        let mut narrow = req();
        narrow.policy = Some(Policy {
            sensitivity: Sensitivity::Private,
            sync: SyncPolicy::LocalOnly,
            client_repo: false,
        });
        let old = take_at(&e, &src, narrow).unwrap();
        assert_eq!(old.manifest.policy.sync, SyncPolicy::LocalOnly);

        let next = e.root.join("秘2.txt");
        fs::write(&next, b"v2").unwrap();
        let mut wide = req();
        wide.supersedes = Some(old.manifest.id.clone());
        wide.policy = Some(Policy {
            sensitivity: Sensitivity::Shared,
            sync: SyncPolicy::Full,
            client_repo: false,
        });
        let new = take_at(&e, &next, wide).unwrap();

        assert_eq!(new.manifest.policy.sync, SyncPolicy::LocalOnly);
        assert_eq!(new.manifest.policy.sensitivity, Sensitivity::Private);
    }

    #[test]
    fn a_new_version_keeps_the_client_repo_lock_outside_the_repo() {
        let e = env();
        let first = client_repo(&e.root, "work", true);
        let old = take_at(&e, &first, req()).unwrap();
        assert!(old.manifest.policy.client_repo);

        let next = e.root.join("copied-outside.txt");
        fs::write(&next, b"v2").unwrap();
        let mut r = req();
        r.supersedes = Some(old.manifest.id.clone());
        let new = take_at(&e, &next, r).unwrap();

        assert!(new.manifest.policy.client_repo);
        assert_eq!(new.manifest.policy.sync, SyncPolicy::LocalOnly);
        assert!(e.stores.has(SyncPolicy::LocalOnly, &new.manifest.hash));
    }

    /// 参照を付け替えないと、本文リンクが古い版を指したままになる(決定5)。
    #[test]
    fn the_reference_follows_the_new_version() {
        let e = env();
        let first = e.root.join("a.png");
        let second = e.root.join("b.png");
        fs::write(&first, b"a").unwrap();
        fs::write(&second, b"b").unwrap();
        let name = RefName::from_str("sketch").unwrap();

        let mut r = req();
        r.ref_name = Some(name.clone());
        let old = take_at(&e, &first, r).unwrap();

        let mut r2 = req();
        r2.supersedes = Some(old.manifest.id.clone());
        let new = take_at(&e, &second, r2).unwrap();

        let current = e.ledger.get_ref(&name).unwrap().unwrap();
        assert_eq!(current.artifact_id, new.manifest.id);
        assert_eq!(current.revision, 2);
        // 名前は変わらない(本文に書かれているのはこの名前)
        assert_eq!(current.name, name);
    }

    #[test]
    fn superseding_something_that_is_not_in_the_ledger_fails() {
        let e = env();
        let src = e.root.join("x.png");
        fs::write(&src, b"x").unwrap();
        let mut r = req();
        r.supersedes = Some(ArtifactId::new(1_755_000_000_000));
        assert!(take_at(&e, &src, r).is_err());
    }
}
