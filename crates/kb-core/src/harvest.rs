//! 状態行の事実集計と、stdout出力済みdigestの端末ローカル記録。
//! hook以外の会話や本文は読まず、session識別子もhash以外を保存しない。
use crate::session_ledger::{self, EventContext, LedgerSummary, SummaryQuery};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension as _, params};
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Policy {
    pub status_line: bool,
    pub disabled_reason: Option<&'static str>,
}
impl Policy {
    pub(crate) fn resolve(kb_enabled: bool, configured: bool, env: Option<&str>) -> Self {
        let disabled_reason = if !kb_enabled {
            Some("kb_disabled")
        } else if env.is_some_and(|value| value.eq_ignore_ascii_case("off")) {
            Some("environment")
        } else if !configured {
            Some("setting")
        } else {
            None
        };
        Self {
            status_line: disabled_reason.is_none(),
            disabled_reason,
        }
    }
}
impl Default for Policy {
    fn default() -> Self {
        Self::resolve(true, true, None)
    }
}

#[derive(Debug)]
pub struct Emission {
    key: String,
    digest: String,
    at: i64,
}
#[derive(Debug)]
pub struct PreparedStatus {
    pub text: String,
    pub emission: Option<Emission>,
}

fn state_path() -> Result<PathBuf> {
    Ok(crate::app_data_dir()?.join("harvest-status.sqlite3"))
}

pub fn prepare(context: EventContext<'_>, cadence: &serde_json::Value) -> Result<PreparedStatus> {
    let now = session_ledger::now_ms();
    let workspace = context
        .workspace_id
        .ok_or_else(|| anyhow::anyhow!("観測先未確認"))?;
    let summary = session_ledger::summary(&SummaryQuery {
        since_ms: now.saturating_sub(14 * 86_400_000),
        until_ms: now + 1,
        workspace_id: Some(workspace.into()),
    })?;
    prepare_at(&state_path()?, context, cadence, &summary, now)
}

fn prepare_at(
    path: &Path,
    context: EventContext<'_>,
    cadence: &serde_json::Value,
    summary: &LedgerSummary,
    now: i64,
) -> Result<PreparedStatus> {
    let workspace = context
        .workspace_id
        .ok_or_else(|| anyhow::anyhow!("観測先未確認"))?;
    // 集計期間とsurfaceを表示して、日次fallbackや他clientの数値を今の会話と誤認させない。
    let counts = summary
        .workspaces
        .iter()
        .find(|w| w.workspace_id == workspace)
        .and_then(|w| w.surfaces.iter().find(|s| s.surface == context.surface));
    let (emitted, proposed, updated, errors) = counts.map_or((0, 0, 0, 0), |s| {
        (
            s.hook_output_emitted,
            s.propose_successes,
            s.update_successes,
            s.propose_errors + s.update_errors,
        )
    });
    let mut text = if summary.exists && counts.is_some() {
        format!(
            "KB記録（この接続種別・直近14日・今回出力前）: 参照出力={emitted} / 起票成功={proposed} / 更新成功={updated} / 書込エラー={errors}。\n"
        )
    } else {
        "KB記録（この接続種別・直近14日・今回出力前）: 観測なし。\n".into()
    };
    // sessionなしはsurface×KB×UTC日。同じKBでも別会話の初回表示を抑制しない。
    let grouping = context
        .session_id
        .map(str::to_owned)
        .unwrap_or_else(|| format!("day:{}", now / 86_400_000));
    let key = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(
            workspace,
            context.surface,
            context.session_id.is_some(),
            grouping
        ))?)
    );
    let mut emission = None;
    if cadence.get("schema").and_then(serde_json::Value::as_str) == Some("kb-app.cadence-digest/v1")
    {
        let facts: crate::cadence_cache::CadenceDigest = serde_json::from_value(cadence.clone())?;
        ensure!(
            facts.digest.len() == 64 && facts.digest.bytes().all(|b| b.is_ascii_hexdigit()),
            "cadence digestが不正"
        );
        let previous = previous_digest(path, &key)?;
        if previous.as_deref() != Some(&facts.digest) {
            let lanes = facts
                .status
                .due_lanes()
                .iter()
                .map(|l| l.as_str())
                .collect::<Vec<_>>()
                .join(",");
            // state由来の文字列もJSON引用する。改行等を新しい指示行として出さない。
            let accepted = facts
                .status
                .accepted_checkpoint_id
                .as_deref()
                .map(|_| "あり")
                .unwrap_or("なし");
            let last = facts
                .status
                .lanes
                .iter()
                .filter_map(|l| l.last_completed_at.as_deref())
                .max();
            let failure = facts
                .status
                .last_failure
                .as_ref()
                .map(|f| serde_json::json!({"at": f.at, "checks": f.failed_checks}));
            text.push_str(&format!("KB手入れ: 期限到来={} / 受入checkpoint={accepted} / 定期受入最終={} / 前回失敗={} / canonicalへのlineageなしrecord={}。\n", if lanes.is_empty() { "なし" } else { &lanes }, serde_json::to_string(&last)?, serde_json::to_string(&failure)?, facts.records_without_canonical_lineage));
            emission = Some(Emission {
                key,
                digest: facts.digest,
                at: now,
            });
        }
    } else {
        text.push_str("KB手入れ: 未確認（cadence_cache未準備・更新待ち、または取得失敗）。\n");
        // 同じdigestへ復旧しても、直前の未確認を最新状態として残さない。
        emission = Some(Emission {
            key,
            digest: "unavailable".into(),
            at: now,
        });
    }
    Ok(PreparedStatus { text, emission })
}

fn previous_digest(path: &Path, key: &str) -> Result<Option<String>> {
    if !path.try_exists()? {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(std::time::Duration::from_millis(100))?;
    Ok(conn
        .query_row("SELECT digest FROM emitted WHERE key=?1", [key], |r| {
            r.get(0)
        })
        .optional()?)
}

/// stdout write+flushに成功して状態行が実際に出た後だけ呼ぶ。受信確認ではない。
pub fn mark_emitted(emission: &Emission) -> Result<()> {
    mark_emitted_at(&state_path()?, emission)
}
fn mark_emitted_at(path: &Path, emission: &Emission) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_millis(100))?;
    let tx = conn.transaction()?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS emitted(key TEXT PRIMARY KEY, digest TEXT NOT NULL, at INTEGER NOT NULL)")?;
    tx.execute("INSERT INTO emitted VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET digest=excluded.digest,at=excluded.at", params![emission.key,emission.digest,emission.at])?;
    // 長期利用でもsession数に比例して無制限に残さない。
    tx.execute("DELETE FROM emitted WHERE key IN (SELECT key FROM emitted ORDER BY at DESC LIMIT -1 OFFSET 512)", [])?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_surface::ClientSurface;
    const WORKSPACE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    fn context(session: Option<&str>) -> EventContext<'_> {
        EventContext {
            surface: ClientSurface::CodexCli,
            workspace_id: Some(WORKSPACE),
            session_id: session,
            turn_id: None,
            prompt_id: None,
            permission_mode: None,
        }
    }
    fn facts() -> serde_json::Value {
        serde_json::json!({"schema":"kb-app.cadence-digest/v1","digest":"a".repeat(64),"records_without_canonical_lineage":2,"status":{"schema":"kb-app.distillation-cadence-status/v1","checked_at":"2026-09-05T00:00:00Z","state_exists":false,"current_checkpoint_id":"fixture","accepted_checkpoint_id":null,"lanes":[],"last_failure":null}})
    }
    #[test]
    fn policy_precedence_is_kb_then_environment_then_setting() {
        assert_eq!(
            Policy::resolve(false, true, Some("off")).disabled_reason,
            Some("kb_disabled")
        );
        assert_eq!(
            Policy::resolve(true, false, Some("OFF")).disabled_reason,
            Some("environment")
        );
        assert_eq!(
            Policy::resolve(true, false, None).disabled_reason,
            Some("setting")
        );
        assert!(Policy::resolve(true, true, None).status_line);
    }
    /// 2026-09-05: 未出力を既読にせず、別会話の初回cadenceをglobalなdigestで隠さない。
    #[test]
    fn digest_suppression_requires_emission_and_is_scoped_by_session_workspace_and_day() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.sqlite3");
        let summary = session_ledger::summary_at(
            &dir.path().join("absent"),
            &SummaryQuery {
                since_ms: 0,
                until_ms: 100,
                workspace_id: Some(WORKSPACE.into()),
            },
        )
        .unwrap();
        let first = prepare_at(&path, context(Some("a")), &facts(), &summary, 10).unwrap();
        assert!(!path.exists());
        assert!(first.text.contains("lineageなしrecord=2"));
        assert!(
            prepare_at(&path, context(Some("a")), &facts(), &summary, 11)
                .unwrap()
                .emission
                .is_some()
        );
        mark_emitted_at(&path, first.emission.as_ref().unwrap()).unwrap();
        let same = prepare_at(&path, context(Some("a")), &facts(), &summary, 12).unwrap();
        assert!(same.emission.is_none());
        assert!(same.text.contains("観測なし"));
        assert!(!same.text.contains("起票成功=0"));
        assert!(!same.text.contains("KB手入れ"));
        assert!(
            prepare_at(&path, context(Some("b")), &facts(), &summary, 12)
                .unwrap()
                .emission
                .is_some()
        );
        let mut other = context(Some("a"));
        other.workspace_id = Some("01ARZ3NDEKTSV4RRFFQ69G5FAW");
        assert!(
            prepare_at(&path, other, &facts(), &summary, 12)
                .unwrap()
                .emission
                .is_some()
        );
        let daily = prepare_at(&path, context(None), &facts(), &summary, 12).unwrap();
        mark_emitted_at(&path, daily.emission.as_ref().unwrap()).unwrap();
        assert!(
            prepare_at(&path, context(None), &facts(), &summary, 86_400_012)
                .unwrap()
                .emission
                .is_some()
        );
        let unavailable = prepare_at(
            &path,
            context(Some("a")),
            &serde_json::json!({"available":false}),
            &summary,
            12,
        )
        .unwrap();
        mark_emitted_at(&path, unavailable.emission.as_ref().unwrap()).unwrap();
        assert!(
            prepare_at(&path, context(Some("a")), &facts(), &summary, 13)
                .unwrap()
                .emission
                .is_some()
        );
        let mut changed = facts();
        changed["digest"] = serde_json::json!("b".repeat(64));
        assert!(
            prepare_at(&path, context(Some("a")), &changed, &summary, 12)
                .unwrap()
                .emission
                .is_some()
        );
    }
}
