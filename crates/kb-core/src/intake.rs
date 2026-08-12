//! ファイルの取り込み — ADR-0003 決定6・contract.md 契約5。
//!
//! **すべての取り込み経路(選択・ドラッグ&ドロップ・貼り付け・CLI・MCP)は
//! ここへ合流する。** 現行の添付は画面側から実体書き込みを直接呼べてしまい、
//! ダイアログを通らない経路が存在する。判定を UI に置くと、その UI を通らない
//! 経路の数だけ穴が空くので、**拒否はここで行い、画面の無効化は補助**とする。
//!
//! ここが引き受ける判断は3つ:
//!
//! 1. **仕事のリポジトリの中にあるか**(あれば「同期しない」に固定し、緩められなくする)
//! 2. **元の場所を指せるか**(リポジトリの同一性が確定できないなら指さない)
//! 3. どの置き場へ入れるか(区分ごとに物理的に分かれている)

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::artifact::{
    ArtifactError, ArtifactId, ArtifactRef, ContentHash, Created, Hasher, Locator, Manifest,
    Policy, RefName, Role, SyncPolicy,
};
use crate::ledger::Ledger;
use crate::store::Stores;
use crate::vault::Vault;

/// 保存方法。UI の語彙では「kb-app にコピーして保管」/「元の場所のまま参照」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    /// この保管庫が実体を持つ
    Managed,
    /// 元の場所を指す(リポジトリ ID + その中の相対パス)
    Linked,
}

/// 取り込みの指定。区分を指定しなくても、場所から安全側の既定が決まる。
#[derive(Debug, Clone)]
pub struct Request {
    pub display_name: String,
    pub media_type: String,
    pub role: Role,
    /// 未指定なら場所から決める(仕事のリポジトリ内なら参照、それ以外はコピー)
    pub keep: Option<Keep>,
    /// 未指定なら既定。**仕事のリポジトリ内なら指定に関わらず固定される**
    pub policy: Option<Policy>,
    pub ref_name: Option<RefName>,
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

/// 元ファイルが属するリポジトリ。保管庫自身は**含めない**
/// (自分の保管庫は「仕事のリポジトリ」ではない)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientRepo {
    pub root: PathBuf,
    /// remote から導いた安定 ID。確定できないときは None
    pub id: Option<String>,
}

/// `src` を含む Git リポジトリを探す。保管庫の中なら None。
pub fn detect_client_repo(vault: &Vault, src: &Path) -> Option<ClientRepo> {
    let vault_root = vault.root.canonicalize().ok()?;
    let start = src.canonicalize().ok()?;
    let mut cur = start.parent()?;
    loop {
        if cur.join(".git").exists() {
            // 自分の保管庫は client repo ではない
            if cur == vault_root {
                return None;
            }
            return Some(ClientRepo {
                root: cur.to_path_buf(),
                id: repo_identity(cur),
            });
        }
        cur = cur.parent()?;
    }
}

/// remote から安定した ID を作る。`https://github.com/acme/widgets.git` も
/// `git@github.com:acme/widgets.git` も `github.com/acme/widgets` に揃える。
///
/// **remote が無ければ None。** ローカルパスは端末固有なので ID にしない。
fn repo_identity(root: &Path) -> Option<String> {
    let repo = git2::Repository::open(root).ok()?;
    let remote = repo.find_remote("origin").ok()?;
    let url = remote.url()?;
    let trimmed = url.trim_end_matches('/').trim_end_matches(".git");
    let without_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    // scp 形式(user@host:owner/repo)を host/owner/repo へ寄せる
    let without_user = without_scheme
        .split_once('@')
        .map(|(_, rest)| rest)
        .unwrap_or(without_scheme);
    let normalized = without_user.replacen(':', "/", 1);
    (!normalized.is_empty()).then_some(normalized)
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
    let client = detect_client_repo(vault, src);

    // 1. 区分を決める。仕事のリポジトリ内なら、指定に関わらず固定する
    let requested = req.policy.unwrap_or_else(Policy::default_managed);
    let (policy, forced_local_only) = match &client {
        Some(_) => (
            Policy::client_repo_locked(),
            requested.sync != SyncPolicy::LocalOnly,
        ),
        None => (
            Policy {
                client_repo: false,
                ..requested
            },
            false,
        ),
    };

    // 2. 保存方法を決める。仕事のリポジトリ内は既定で「元の場所を指す」
    let keep = req.keep.unwrap_or(match client {
        Some(_) => Keep::Linked,
        None => Keep::Managed,
    });

    // 3. 参照名の衝突は、上書きせず呼び出し側へ返す(別名を提案するのは UI)
    if let Some(name) = &req.ref_name
        && ledger.ref_taken(name)
    {
        bail!("参照名 {name} は使われている");
    }

    let (locator, hash, size, warn_over) = match keep {
        Keep::Managed => {
            let imported = stores.import_path(policy.sync, src)?;
            (
                Locator::Managed {
                    hash: imported.hash.clone(),
                },
                imported.hash,
                imported.size,
                imported.warn_over,
            )
        }
        Keep::Linked => {
            let Some(client) = &client else {
                // 保管庫の外の、リポジトリでもない場所は指せない。
                // 端末固有のパスになるため(正本の受入条件)
                return Err(ArtifactError::UnstableLocator.into());
            };
            let Some(repo_id) = &client.id else {
                // リポジトリの同一性が確定できない。指すのをやめてコピーを案内する
                bail!("このリポジトリは他の端末から辿れない。コピーして保管してください");
            };
            let rel = src
                .canonicalize()?
                .strip_prefix(client.root.canonicalize()?)
                .context("リポジトリ内の位置が取れない")?
                .to_string_lossy()
                .replace('\\', "/");
            let locator = Locator::linked(repo_id, &rel)?;
            let (hash, size) = hash_file(src)?;
            (locator, hash, size, None)
        }
    };

    let mut manifest = Manifest::new(
        ArtifactId::new(unix_ms(&req.at)),
        hash,
        Created {
            media_type: req.media_type.clone(),
            size,
            at: req.at.clone(),
            origin: req.origin.clone(),
            by: req.by.clone(),
        },
        req.display_name.clone(),
        locator,
        policy,
        req.role,
    );
    manifest.record(
        &req.at,
        match keep {
            Keep::Managed => "imported",
            Keep::Linked => "linked",
        },
        &req.origin,
    );
    if forced_local_only {
        manifest.record(
            &req.at,
            "policy-forced",
            "仕事のリポジトリの中にあるため、同期しない設定に固定した",
        );
    }
    ledger.put(vault, &manifest)?;

    let artifact_ref = match req.ref_name {
        Some(name) => {
            let r = ArtifactRef::new(workspace_id, name, manifest.id.clone());
            ledger.put_ref(vault, policy.sync, &r)?;
            Some(r)
        }
        None => None,
    };

    Ok(Taken {
        manifest,
        artifact_ref,
        warn_over,
        forced_local_only,
    })
}

/// 実体をコピーせずに照合値だけ採る(linked 用)。
fn hash_file(path: &Path) -> Result<(ContentHash, u64)> {
    let mut f = File::open(path).with_context(|| format!("読み込めない: {}", path.display()))?;
    let mut hasher = Hasher::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let size = hasher.len();
    Ok((hasher.finish(), size))
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
    use crate::artifact::Sensitivity;
    use std::fs;
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
            display_name: "表.csv".into(),
            media_type: "text/csv".into(),
            role: Role::File,
            keep: None,
            policy: None,
            ref_name: None,
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
        // 実体が置き場に入り、台帳が引ける
        assert!(e.stores.has(SyncPolicy::Full, &out.manifest.hash));
        assert!(e.ledger.get(&out.manifest.id).unwrap().is_some());
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
    fn client_repo_file_is_linked_by_repo_id_not_absolute_path() {
        let e = env();
        let src = client_repo(&e.root, "work", true);
        let out = take_at(&e, &src, req()).unwrap();

        match &out.manifest.locator {
            Locator::Linked { repo_id, rel_path } => {
                assert_eq!(repo_id, "github.com/acme/widgets");
                assert_eq!(rel_path, "data/table.csv");
            }
            other => panic!("linked のはず: {other:?}"),
        }
        // 実体はコピーしていない
        assert!(!e.stores.has(SyncPolicy::LocalOnly, &out.manifest.hash));
    }

    #[test]
    fn client_repo_without_remote_refuses_to_link() {
        let e = env();
        let src = client_repo(&e.root, "work", false);

        let err = take_at(&e, &src, req()).unwrap_err();
        assert!(
            err.to_string().contains("コピーして保管"),
            "コピーを案内すること: {err}"
        );
        // 明示的にコピーを選べば通る(正本は managed を禁じていない)
        let mut r = req();
        r.keep = Some(Keep::Managed);
        let out = take_at(&e, &src, r).unwrap();
        assert_eq!(out.manifest.policy.sync, SyncPolicy::LocalOnly);
        // 複製はこの端末から出ない置き場に入る
        assert!(e.stores.has(SyncPolicy::LocalOnly, &out.manifest.hash));
        assert!(!e.stores.has(SyncPolicy::Full, &out.manifest.hash));
    }

    #[test]
    fn linking_something_outside_any_repo_is_refused() {
        let e = env();
        let src = e.root.join("loose.txt");
        fs::write(&src, b"loose").unwrap();

        let mut r = req();
        r.keep = Some(Keep::Linked);
        let err = take_at(&e, &src, r).unwrap_err();
        assert!(matches!(
            err.downcast_ref::<ArtifactError>(),
            Some(ArtifactError::UnstableLocator)
        ));
    }

    #[test]
    fn the_vault_itself_is_not_a_client_repo() {
        let e = env();
        let src = e.vault.root.join("note.files").join("img.png");
        fs::create_dir_all(src.parent().unwrap()).unwrap();
        fs::write(&src, b"img").unwrap();

        assert_eq!(detect_client_repo(&e.vault, &src), None);
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
    fn repo_identity_normalizes_url_shapes() {
        let dir = tempdir().unwrap();
        for (url, want) in [
            ("git@github.com:acme/widgets.git", "github.com/acme/widgets"),
            (
                "https://github.com/acme/widgets.git",
                "github.com/acme/widgets",
            ),
            (
                "https://github.com/acme/widgets/",
                "github.com/acme/widgets",
            ),
        ] {
            let root = dir.path().join(url.replace(['/', ':', '@', '.'], "_"));
            fs::create_dir_all(&root).unwrap();
            let repo = git2::Repository::init(&root).unwrap();
            repo.remote("origin", url).unwrap();
            assert_eq!(repo_identity(&root).as_deref(), Some(want), "{url}");
        }
    }
}
