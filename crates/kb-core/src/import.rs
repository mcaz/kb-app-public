//! 既存 Markdown KB からの移植(互換レイヤ)。旧 kb(id/scope/status=active 系の
//! frontmatter+`[[wikilink]]`)を OKF v0.2 互換へ変換して vault に取り込む。
//! 旧メタは `legacy:` キーに無損失で保持(OKF §4.1: 未知キーは保持される)。

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_yaml::Value;

use crate::frontmatter::{Frontmatter, Generated, Note};
use crate::index::{import_markdown_snapshot, open_db, sync};
use crate::vault::Vault;

/// 取り込み元1つ。`prefix` が空なら vault 直下へ同じ相対パスで入る。
pub struct Source {
    pub root: PathBuf,
    pub prefix: String,
    pub label: String,
}

#[derive(Debug, Default)]
pub struct ImportReport {
    pub imported: Vec<String>,
    pub skipped: Vec<String>,
    pub unresolved_links: Vec<String>,
}

const SKIP_NAMES: &[&str] = &["INDEX.md", "README.md", "index.md", "log.md"];

fn scan(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            name != ".git"
                && name != "archive"
                && !(e.file_type().is_dir() && name.ends_with(".files"))
        })
        .flatten()
    {
        let path = entry.path();
        if !entry.file_type().is_file()
            || path.extension().and_then(|e| e.to_str()) != Some("md")
            || SKIP_NAMES.contains(
                &path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .as_ref(),
            )
        {
            continue;
        }
        if let Ok(rel) = path.strip_prefix(root) {
            out.push((
                rel.with_extension("").to_string_lossy().to_string(),
                path.to_path_buf(),
            ));
        }
    }
    out.sort();
    out
}

/// 複数ソースをまとめて取り込む(リンク解決はソース横断 — 旧・二層構成の
/// personal → team 参照も新パスへ張り替わる)。1コミット+随時 push。
pub fn import(vault: &Vault, sources: &[Source], allow_new_tags: bool) -> Result<ImportReport> {
    let mut report = ImportReport::default();
    let conn = open_db(vault)?;
    sync(vault, &conn)?;

    // 第1パス: 全ソースの id(ファイル名)→ 新相対パスの対応表
    let mut link_map: HashMap<String, String> = HashMap::new();
    let mut files: Vec<(usize, String, PathBuf)> = Vec::new();
    for (si, src) in sources.iter().enumerate() {
        for (rel, path) in scan(&src.root) {
            let dest = if src.prefix.is_empty() {
                rel.clone()
            } else {
                format!("{}/{rel}", src.prefix)
            };
            let id = Path::new(&rel)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            link_map.entry(id).or_insert_with(|| dest.clone());
            files.push((si, dest, path));
        }
    }

    // 第2パス: 変換して書き込み
    let mut written: Vec<String> = Vec::new();
    for (_si, dest, path) in &files {
        if vault.note_path(dest)?.exists() {
            report.skipped.push(format!("{dest}(既存)"));
            continue;
        }
        let content = fs::read_to_string(path)?;
        match convert(&content, &link_map, &mut report.unresolved_links) {
            Ok(note) => {
                if let Err(error) = vault.write_imported_note(&conn, dest, &note, allow_new_tags) {
                    report.skipped.push(format!("{dest}({error})"));
                    continue;
                }
                // importはMarkdownをDBへ入れる明示経路。通常syncはObsidian等の外部編集を
                // 暗黙に取り込まないため、この入口だけsnapshotを読み直す。
                let imported = import_markdown_snapshot(vault, &conn)?;
                if !imported.degraded.is_empty() {
                    anyhow::bail!(
                        "importしたMarkdownをDBへ反映できない: {}",
                        imported
                            .degraded
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(" / ")
                    );
                }
                written.push(dest.clone());
                report.imported.push(dest.clone());
            }
            Err(e) => report.skipped.push(format!("{dest}({e})")),
        }
    }

    if !written.is_empty() {
        vault.write_index_md()?;
        let labels: Vec<&str> = sources.iter().map(|s| s.label.as_str()).collect();
        vault.append_log(&format!(
            "**Import**: {} からノート {} 本を移植。",
            labels.join(" / "),
            written.len()
        ))?;
        let mut paths: Vec<String> = written.iter().map(|id| format!("{id}.md")).collect();
        paths.push("index.md".into());
        paths.push("log.md".into());
        let refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
        vault.commit(
            &refs,
            &format!("import: {} 本({})", written.len(), labels.join(" / ")),
        )?;
        crate::connect::auto_push(vault);
    }
    Ok(report)
}

/// 旧ノート1本を OKF v0.2 互換へ変換。
fn convert(
    content: &str,
    link_map: &HashMap<String, String>,
    unresolved: &mut Vec<String>,
) -> Result<Note> {
    let rest = content
        .strip_prefix("---\n")
        .context("frontmatter がない")?;
    let end = rest.find("\n---\n").context("frontmatter の終端がない")?;
    let (yaml, body) = rest.split_at(end);
    let body = body.trim_start_matches("\n---\n").trim_start_matches('\n');
    let old: BTreeMap<String, Value> =
        serde_yaml::from_str(yaml).context("旧 frontmatter parse")?;

    let get = |k: &str| old.get(k).and_then(|v| v.as_str()).map(String::from);
    let old_status = get("status").unwrap_or_else(|| "active".into());
    let source = get("source");

    let mut front = Frontmatter {
        // Legacy imports remain authority-less until an explicit migration assigns
        // a stable UID and canonical scope. This preserves lossless compatibility.
        note_uid: None,
        authority: None,
        relations: Vec::new(),
        kind: get("type").unwrap_or_else(|| "Note".into()),
        title: get("title"),
        description: None,
        tags: old
            .get("tags")
            .and_then(|v| v.as_sequence())
            .map(|s| {
                s.iter()
                    .filter_map(|t| t.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        status: match old_status.as_str() {
            "draft" => Some("draft".into()),
            "archived" => Some("deprecated".into()),
            _ => None, // active / open / resolved / done / failed → stable(原文は legacy に保持)
        },
        generated: Some(Generated {
            by: match &source {
                Some(s) if s.starts_with("manual:") => "human:owner".into(),
                Some(s) if s.starts_with("routine:") => {
                    format!(
                        "process:{}",
                        s.trim_start_matches("routine:")
                            .split_whitespace()
                            .next()
                            .unwrap_or("routine")
                    )
                }
                _ => "claude-code/legacy-kb".into(),
            },
            at: format!(
                "{}T00:00:00Z",
                get("updated")
                    .or_else(|| get("created"))
                    .unwrap_or_else(crate::frontmatter::today)
            ),
        }),
        verified: get("verified").map(|d| {
            serde_yaml::to_value(vec![Generated {
                by: "human:owner".into(),
                at: format!("{d}T00:00:00Z"),
            }])
            .expect("verified serializes")
        }),
        sources: source.as_ref().map(|s| {
            serde_yaml::to_value(vec![BTreeMap::from([("resource".to_string(), s.clone())])])
                .expect("sources serializes")
        }),
        stale_after: None,
        created: get("created").map(|d| {
            if d.len() == 10 {
                format!("{d}T00:00:00Z")
            } else {
                d
            }
        }),
        // 旧ノートの作成者は generated.by / legacy に残すが、現行 KB へ取り込んだ
        // 知識の管理主体は AI に統一する(人間所有ノートを新しく作る抜け道にしない)。
        origin: Some("agent".into()),
        extra: BTreeMap::new(),
    };
    // 旧メタを無損失で保持(id/scope/vault/status/share/index など)
    let legacy: BTreeMap<String, Value> = old
        .into_iter()
        .filter(|(k, _)| !matches!(k.as_str(), "title" | "tags" | "type"))
        .collect();
    front
        .extra
        .insert("legacy".into(), serde_yaml::to_value(legacy)?);

    Ok(Note {
        front,
        body: rewrite_wikilinks(body, link_map, unresolved),
    })
}

/// `[[id]]` / `[[id|表示]]` / `[[id#節]]` → `[表示](/新パス.md)`。
/// 解決できないものは原文のまま残す(未執筆の知識 — OKF §6.1 の精神)。
fn rewrite_wikilinks(
    body: &str,
    map: &HashMap<String, String>,
    unresolved: &mut Vec<String>,
) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find("[[") {
        let Some(end_rel) = rest[start..].find("]]") else {
            break;
        };
        let inner = &rest[start + 2..start + end_rel];
        out.push_str(&rest[..start]);
        let (target, alias) = match inner.split_once('|') {
            Some((t, a)) => (t, Some(a)),
            None => (inner, None),
        };
        let id = target.split('#').next().unwrap_or(target).trim();
        match map.get(id) {
            Some(path) => {
                let text = alias.unwrap_or(id);
                out.push_str(&format!("[{text}](/{path}.md)"));
            }
            None => {
                if !unresolved.contains(&id.to_string()) {
                    unresolved.push(id.to_string());
                }
                out.push_str(&rest[start..start + end_rel + 2]);
            }
        }
        rest = &rest[start + end_rel + 2..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_legacy_note() {
        let src = "---\nid: foo\ntitle: 旧ノート\ntype: note\nscope: dev\nvault: personal\nstatus: active\ncreated: 2026-07-01\nupdated: 2026-07-15\nverified: 2026-07-20\ntags: [a, b]\nsource: session:2026-07-01 経緯\nshare: candidate\n---\n\n本文。[[bar]] と [[baz|別名]] を参照。\n";
        let map = HashMap::from([
            ("bar".to_string(), "dev/bar".to_string()),
            ("baz".to_string(), "team/work/baz".to_string()),
        ]);
        let mut unresolved = Vec::new();
        let note = convert(src, &map, &mut unresolved).unwrap();
        assert_eq!(note.front.effective_status(), "stable");
        assert_eq!(note.front.origin.as_deref(), Some("agent"));
        assert_eq!(
            note.front.generated.as_ref().unwrap().at,
            "2026-07-15T00:00:00Z"
        );
        assert!(note.front.extra.contains_key("legacy"));
        assert!(note.body.contains("[bar](/dev/bar.md)"));
        assert!(note.body.contains("[別名](/team/work/baz.md)"));
        assert!(unresolved.is_empty());
        // round-trip で OKF 準拠ファイルになる
        let out = note.to_file_string().unwrap();
        Note::parse(&out).unwrap();
    }

    #[test]
    fn manual_legacy_note_becomes_agent_owned() {
        let src = "---\ntitle: 手書き由来\ntype: note\nsource: manual:owner\n---\n\n本文。\n";
        let note = convert(src, &HashMap::new(), &mut Vec::new()).unwrap();
        assert_eq!(note.front.origin.as_deref(), Some("agent"));
        assert_eq!(
            note.front.generated.as_ref().map(|g| g.by.as_str()),
            Some("human:owner")
        );
    }

    #[test]
    fn unresolved_wikilink_kept_verbatim() {
        let mut unresolved = Vec::new();
        let body = rewrite_wikilinks("[[missing]] のまま", &HashMap::new(), &mut unresolved);
        assert_eq!(body, "[[missing]] のまま");
        assert_eq!(unresolved, vec!["missing".to_string()]);
    }

    #[test]
    fn end_to_end_import_two_sources() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("personal");
        let t = dir.path().join("team");
        fs::create_dir_all(p.join("dev")).unwrap();
        fs::create_dir_all(t.join("work")).unwrap();
        fs::write(p.join("INDEX.md"), "skip me").unwrap();
        fs::write(
            p.join("dev/note-a.md"),
            "---\nid: note-a\ntitle: A\ntype: note\nstatus: active\nupdated: 2026-08-01\ntags: [test]\n---\n\n[[shared-b]] 参照。\n",
        )
        .unwrap();
        fs::write(
            t.join("work/shared-b.md"),
            "---\nid: shared-b\ntitle: B\ntype: decision\nstatus: archived\ntags: [test]\n---\n\n本文 B。\n",
        )
        .unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let report = import(
            &vault,
            &[
                Source {
                    root: p,
                    prefix: String::new(),
                    label: "personal".into(),
                },
                Source {
                    root: t,
                    prefix: "team".into(),
                    label: "team".into(),
                },
            ],
            false,
        )
        .unwrap();
        assert_eq!(report.imported.len(), 2);
        assert!(report.unresolved_links.is_empty());
        let a = vault.read_note("dev/note-a").unwrap();
        assert!(
            a.body.contains("[shared-b](/team/work/shared-b.md)"),
            "{}",
            a.body
        );
        let b = vault.read_note("team/work/shared-b").unwrap();
        assert_eq!(b.front.effective_status(), "deprecated");
    }

    #[test]
    fn import_rejects_structurally_invalid_tags_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::create_dir_all(&source).unwrap();
        for (name, tags) in [
            ("missing", ""),
            ("too-many", "tags: [a, b, c, d, e]\n"),
            ("bad-shape", "tags: [日本語]\n"),
        ] {
            fs::write(
                source.join(format!("{name}.md")),
                format!("---\ntitle: {name}\ntype: note\n{tags}---\n\n本文。\n"),
            )
            .unwrap();
        }
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let report = import(
            &vault,
            &[Source {
                root: source,
                prefix: String::new(),
                label: "legacy".into(),
            }],
            true,
        )
        .unwrap();
        assert!(report.imported.is_empty());
        assert_eq!(report.skipped.len(), 3);
        assert!(vault.list_note_files().unwrap().is_empty());
    }

    #[test]
    fn import_requires_explicit_permission_for_new_vocabulary() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("new-tag.md"),
            "---\ntitle: 新語\ntype: note\ntags: [brand-new]\n---\n\n本文。\n",
        )
        .unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        vault
            .propose_for_test("既存", "本文", None, &["known".into()], "test/client")
            .unwrap();
        let sources = [Source {
            root: source,
            prefix: String::new(),
            label: "legacy".into(),
        }];

        let rejected = import(&vault, &sources, false).unwrap();
        assert!(rejected.imported.is_empty());
        assert!(rejected.skipped[0].contains("語彙にないタグ"));

        let accepted = import(&vault, &sources, true).unwrap();
        assert_eq!(accepted.imported, vec!["new-tag"]);
        assert_eq!(
            vault.read_note("new-tag").unwrap().front.origin.as_deref(),
            Some("agent")
        );
    }
}
