//! AIの行動選択評価へ、本番のcontext生成結果だけを渡す合成fixture。
//! cfg(test)限定で、入力はリポジトリ内の固定JSON、DBはメモリ内に閉じる。

use std::collections::BTreeMap;
use std::io::Write as _;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::authority::{Authority, NoteRelation, NoteUid, RelationKind};
use crate::frontmatter::{Frontmatter, Note};

const SUITE: &str = include_str!("../../../schemas/examples/judgment-eval.example.json");

#[derive(Deserialize)]
struct FixtureRelation {
    #[serde(rename = "type")]
    kind: RelationKind,
    target: String,
}

#[derive(Deserialize)]
struct FixtureNote {
    id: String,
    title: String,
    body: String,
    authority: Authority,
    judgment: Option<Value>,
    relations: Vec<FixtureRelation>,
}

#[derive(Deserialize)]
struct FixtureCase {
    id: String,
    scope: Option<String>,
    note_ids: Vec<String>,
}

#[derive(Deserialize)]
struct FixtureSuite {
    schema_version: String,
    notes: Vec<FixtureNote>,
    cases: Vec<FixtureCase>,
}

fn resolve_note(source: &FixtureNote, identities: &BTreeMap<&str, NoteUid>) -> Result<Note> {
    let mut front = Frontmatter::new_note(&source.title);
    front.tags = vec!["review".into()];
    front.note_uid = Some(identities[&source.id.as_str()].clone());
    front.authority = Some(source.authority.clone());
    front.relations = source
        .relations
        .iter()
        .map(|relation| {
            Ok(NoteRelation {
                kind: relation.kind,
                target: identities
                    .get(relation.target.as_str())
                    .context("fixture relationの対象がない")?
                    .clone(),
            })
        })
        .collect::<Result<_>>()?;
    if let Some(mut judgment) = source.judgment.clone() {
        if let Some(references) = judgment
            .get_mut("decision_refs")
            .and_then(Value::as_array_mut)
        {
            for reference in references {
                let symbolic = reference.as_str().context("decision_refsは文字列にする")?;
                let uid = identities
                    .get(symbolic)
                    .context("decision_refsの対象がない")?;
                *reference = json!(uid);
            }
        }
        front.judgment = Some(serde_json::from_value(judgment)?);
    }
    Ok(Note {
        front,
        body: source.body.clone(),
    })
}

fn exported_contexts() -> Result<Value> {
    let raw: Value = serde_json::from_str(SUITE)?;
    let suite: FixtureSuite = serde_json::from_value(raw.clone())?;
    let identities = suite
        .notes
        .iter()
        .enumerate()
        .map(|(index, note)| Ok((note.id.as_str(), format!("{:026}", index + 1).parse()?)))
        .collect::<Result<BTreeMap<_, NoteUid>>>()?;
    ensure!(
        identities.len() == suite.notes.len(),
        "fixture note IDが重複"
    );
    let sources: BTreeMap<_, _> = suite
        .notes
        .iter()
        .map(|note| (note.id.as_str(), note))
        .collect();
    let mut cases = Vec::new();
    for case in &suite.cases {
        // ケース間の決定や行動がリンク展開で混ざると比較条件が変わるため、毎回別DBにする。
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE notes (
                id TEXT PRIMARY KEY, note_uid TEXT UNIQUE, status TEXT,
                document TEXT, normal_reference_allowed INTEGER
             );
             CREATE TABLE note_relations (src_uid TEXT, kind TEXT, target_uid TEXT);",
        )?;
        let mut notes = Vec::new();
        let mut ranked_ids = Vec::new();
        for symbolic in &case.note_ids {
            let source = sources.get(symbolic.as_str()).context("caseのnoteがない")?;
            let note = resolve_note(source, &identities)?;
            for relation in &source.relations {
                ensure!(
                    case.note_ids.contains(&relation.target),
                    "caseのrelation対象が欠損"
                );
            }
            let id = format!("notes/{symbolic}");
            let document = note.to_file_string()?;
            conn.execute(
                "INSERT INTO notes VALUES (?1, ?2, 'stable', ?3, 1)",
                params![id, note.front.note_uid.as_ref().unwrap().as_str(), document],
            )?;
            for relation in &note.front.relations {
                conn.execute(
                    "INSERT INTO note_relations VALUES (?1, ?2, ?3)",
                    params![
                        note.front.note_uid.as_ref().unwrap().as_str(),
                        relation.kind.as_str(),
                        relation.target.as_str()
                    ],
                )?;
            }
            notes.push(json!({"id": id, "document": document}));
            ranked_ids.push(id);
        }
        let context =
            crate::judgment_context::context_for_notes(&conn, &ranked_ids, case.scope.as_deref())?;
        cases.push(json!({"id": case.id, "notes": notes, "judgment_context": context}));
    }
    Ok(json!({
        "schema_version": suite.schema_version,
        "generator": "kb-core::judgment_context::context_for_notes",
        "suite_digest": format!("{:x}", Sha256::digest(serde_json::to_vec(&raw)?)),
        "cases": cases
    }))
}

/// 2026-09-08: 読んだ決定を行動へ適用しなかった事例を、引用数ではなく選択結果で評価する。
#[test]
fn export_synthetic_contexts() -> Result<()> {
    let exported = exported_contexts()?;
    let cases = exported["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 10);
    assert!(
        cases
            .iter()
            .all(|case| case.get("expected_action").is_none())
    );
    assert_eq!(
        cases[0]["judgment_context"]["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        cases[7]["judgment_context"]["entries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    if let Some(path) = std::env::var_os("KB_JUDGMENT_EVAL_EXPORT") {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        file.write_all(&serde_json::to_vec_pretty(&exported)?)?;
        file.write_all(b"\n")?;
    }
    Ok(())
}
