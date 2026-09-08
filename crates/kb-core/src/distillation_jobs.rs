//! 蒸留待ちをノート保存と同じtransactionで永続化する。
//!
//! relationの存在を意味的な確認済み証明にしない。leaseは世代と本文hashに束縛し、
//! 書換え・再起動・競合後に古いAI応答が最新の未処理を消すことを防ぐ。

use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

// 原記録6件で共有の正本を1回確認できる範囲から始め、AI出力の膨張を抑える。
pub(crate) const MAX_BATCH_NOTES: usize = 6;
const MIN_BATCH_NOTES: usize = 3;
const MAX_BATCH_SOURCE_BYTES: usize = 64 * 1024;
// 少量のInboxも確実に進める。待機開始は保存時刻であり、workerのtickで延長しない。
pub(crate) const BATCH_MAX_WAIT_SECONDS: i64 = 120;
// 0は通常登録、正数は再試行の時刻なので、負数を明示的な即時受付だけに予約する。
const MANUAL_AVAILABLE_AT: i64 = -1;

pub(crate) const SCHEMA_SQL: &str = r#"
CREATE TABLE distillation_jobs(
    note TEXT PRIMARY KEY,
    generation INTEGER NOT NULL CHECK(generation > 0),
    state TEXT NOT NULL CHECK(state IN ('pending', 'running', 'retry_wait', 'completed', 'blocked')),
    reason TEXT NOT NULL,
    queued_at INTEGER NOT NULL,
    available_at INTEGER NOT NULL,
    attempt INTEGER NOT NULL DEFAULT 0,
    lease_token TEXT,
    lease_expires_at INTEGER,
    last_reviewed_at INTEGER,
    reviewed_hash TEXT,
    outcome TEXT CHECK(outcome IN ('applied', 'no_change')),
    last_error TEXT
);
CREATE INDEX distillation_jobs_ready ON distillation_jobs(state, available_at, queued_at);
CREATE TABLE distillation_job_runs(
    run_id TEXT PRIMARY KEY,
    note TEXT NOT NULL,
    generation INTEGER NOT NULL,
    reviewed_at INTEGER NOT NULL,
    outcome TEXT NOT NULL,
    reason TEXT NOT NULL,
    snapshot_digest TEXT NOT NULL,
    before_documents TEXT NOT NULL,
    after_documents TEXT NOT NULL,
    client TEXT NOT NULL
);
CREATE TRIGGER distillation_jobs_insert AFTER INSERT ON notes
WHEN NEW.normal_reference_allowed = 1 AND NEW.distillation_allowed = 1 AND NEW.document <> ''
BEGIN
    INSERT INTO distillation_jobs(note, generation, state, reason, queued_at, available_at)
    VALUES(NEW.id, 1, 'pending', 'created', unixepoch(), 0)
    ON CONFLICT(note) DO UPDATE SET generation=generation+1, state='pending', reason='created',
        queued_at=unixepoch(), available_at=0, attempt=0, lease_token=NULL,
        lease_expires_at=NULL, last_error=NULL;
END;
CREATE TRIGGER distillation_jobs_update AFTER UPDATE OF document, normal_reference_allowed, distillation_allowed, id ON notes
WHEN NEW.normal_reference_allowed = 1 AND NEW.distillation_allowed = 1 AND NEW.document <> ''
    AND (OLD.document IS NOT NEW.document OR OLD.normal_reference_allowed <> 1 OR OLD.distillation_allowed <> 1 OR OLD.id <> NEW.id)
BEGIN
    DELETE FROM distillation_jobs WHERE note=OLD.id AND OLD.id <> NEW.id;
    INSERT INTO distillation_jobs(note, generation, state, reason, queued_at, available_at)
    VALUES(NEW.id, 1, 'pending', 'changed', unixepoch(), 0)
    ON CONFLICT(note) DO UPDATE SET generation=generation+1, state='pending', reason='changed',
        queued_at=unixepoch(), available_at=0, attempt=0, lease_token=NULL,
        lease_expires_at=NULL, last_error=NULL;
END;
CREATE TRIGGER distillation_jobs_hide AFTER UPDATE OF document, normal_reference_allowed, distillation_allowed ON notes
WHEN NEW.normal_reference_allowed <> 1 OR NEW.distillation_allowed <> 1 OR NEW.document = ''
BEGIN
    DELETE FROM distillation_jobs WHERE note=OLD.id;
END;
CREATE TRIGGER distillation_jobs_delete AFTER DELETE ON notes
BEGIN
    DELETE FROM distillation_jobs WHERE note=OLD.id;
END;
INSERT INTO distillation_jobs(note, generation, state, reason, queued_at, available_at)
SELECT id, 1, 'pending', 'unreviewed', unixepoch(), 0 FROM notes
WHERE normal_reference_allowed = 1 AND distillation_allowed = 1 AND document <> '';
"#;

/// 旧human原記録と専用の提案チケットは通常のAI変更経路で手入れできない。
pub(crate) fn derive_allowed(note: &crate::frontmatter::Note) -> bool {
    note.front.origin.as_deref() == Some("agent")
        && crate::proposal_workflow::derive_normal_reference_allowed(note)
        && crate::proposal_workflow::guard_note_delete(note).is_ok()
}

const TRIGGERS: [&str; 4] = [
    "distillation_jobs_insert",
    "distillation_jobs_update",
    "distillation_jobs_hide",
    "distillation_jobs_delete",
];

/// 強制登録を失ったDBを、通常保存できるDBとして扱わない。
pub(crate) fn verify_schema(conn: &Connection) -> Result<()> {
    for (trigger, definition) in trigger_definitions()? {
        let stored: Option<String> = conn
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE type='trigger' AND name=?1",
                [trigger],
                |row| row.get(0),
            )
            .optional()?;
        let expected = normalize_sql(definition);
        if stored.is_none_or(|stored| normalize_sql(&stored) != expected) {
            bail!("蒸留の強制登録triggerが欠落または不整合: {trigger}");
        }
    }
    Ok(())
}

/// 復旧ではtable作成や初期job登録をせず、検証と同じtrigger定義だけを戻す。
pub(crate) fn trigger_definitions() -> Result<Vec<(&'static str, &'static str)>> {
    let mut definitions = Vec::new();
    for trigger in TRIGGERS {
        let prefix = format!("CREATE TRIGGER {trigger} ");
        let start = SCHEMA_SQL
            .find(&prefix)
            .ok_or_else(|| anyhow::anyhow!("蒸留triggerのDDL定義がない: {trigger}"))?;
        let tail = &SCHEMA_SQL[start..];
        let end = tail
            .find("END;")
            .ok_or_else(|| anyhow::anyhow!("蒸留triggerのDDL終端がない: {trigger}"))?
            + 3;
        definitions.push((trigger, &tail[..end]));
    }
    Ok(definitions)
}

fn normalize_sql(sql: &str) -> String {
    sql.trim()
        .trim_end_matches(';')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JobLease {
    pub note: String,
    pub generation: i64,
    pub token: String,
    pub input_hash: String,
    pub expires_at: i64,
    pub attempt: u32,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionOutcome {
    Applied,
    NoChange,
}

impl CompletionOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::NoChange => "no_change",
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct JobStatus {
    pub pending: u32,
    pub running: u32,
    pub retry_wait: u32,
    pub blocked: u32,
    pub completed: u32,
    pub oldest_pending_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ImmediateDistillationScope {
    Unreviewed,
    All,
}

#[derive(Debug, Clone, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ImmediateDistillationResult {
    pub registered: u32,
    pub requeued: u32,
    pub expedited: u32,
    pub jobs: JobStatus,
}

/// 即時実行も通常の永続キューへ合流させる。実行中のleaseや本文は変更しない。
pub fn request_now(
    conn: &Connection,
    scope: ImmediateDistillationScope,
    now: i64,
) -> Result<ImmediateDistillationResult> {
    if now < 0 {
        bail!("即時蒸留の受付時刻が範囲外");
    }
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    verify_schema(&tx)?;
    // 一覧を確定してから更新し、同一SELECTの走査中に状態変更で対象が増減しないようにする。
    let ids = tx
        .prepare(
            "SELECT n.id FROM notes n LEFT JOIN distillation_jobs j ON j.note=n.id
         WHERE n.normal_reference_allowed=1 AND n.distillation_allowed=1 AND n.document<>''
             AND (j.state IS NULL OR j.state IN ('pending', 'retry_wait', 'blocked')
                 OR (?1 AND j.state='completed'))
         ORDER BY n.id",
        )?
        .query_map([scope == ImmediateDistillationScope::All], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut registered = 0_usize;
    let mut requeued = 0_usize;
    let mut expedited = 0_usize;
    for id in ids {
        let (document, state, generation): (String, Option<String>, Option<i64>) = tx.query_row(
            "SELECT n.document, j.state, j.generation FROM notes n
             LEFT JOIN distillation_jobs j ON j.note=n.id WHERE n.id=?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let note = crate::frontmatter::Note::parse(&document)?;
        // 派生columnが古くても、human所有・提案票・通常参照不可を再登録しない。
        if !derive_allowed(&note)
            || note.front.authority.as_ref().is_some_and(|authority| {
                authority.role == crate::authority::AuthorityRole::Proposal
            })
        {
            continue;
        }
        match state.as_deref() {
            None => {
                registered += tx.execute(
                    "INSERT INTO distillation_jobs(note, generation, state, reason, queued_at, available_at)
                     VALUES(?1, 1, 'pending', 'manual', ?2, -1)",
                    params![id, now],
                )?;
            }
            Some("pending") => {
                // 二度押しても世代・待機開始時刻・試行回数を変えず、優先順を保つ。
                expedited += tx.execute(
                    "UPDATE distillation_jobs SET available_at=?1
                     WHERE note=?2 AND state='pending' AND available_at<>?1",
                    params![MANUAL_AVAILABLE_AT, id],
                )?;
            }
            Some("retry_wait" | "blocked") => {
                // 明示的な再試行でも失敗済みbatchの単独化を取り消さない。
                requeued += tx.execute(
                    "UPDATE distillation_jobs SET state='pending', available_at=-1,
                         lease_token=NULL, lease_expires_at=NULL, last_error=NULL
                     WHERE note=?1 AND state IN ('retry_wait', 'blocked')",
                    params![id],
                )?;
            }
            Some("completed") if scope == ImmediateDistillationScope::All => {
                let next = generation
                    .and_then(|generation| generation.checked_add(1))
                    .ok_or_else(|| anyhow::anyhow!("蒸留ジョブの世代が範囲外"))?;
                requeued += tx.execute(
                    "UPDATE distillation_jobs SET generation=?1, state='pending', reason='manual',
                         queued_at=?2, available_at=-1, attempt=0,
                         lease_token=NULL, lease_expires_at=NULL, last_error=NULL
                     WHERE note=?3 AND state='completed'",
                    params![next, now, id],
                )?;
            }
            _ => bail!("即時蒸留の対象状態が不整合"),
        }
    }
    let result = ImmediateDistillationResult {
        registered: u32::try_from(registered)?,
        requeued: u32::try_from(requeued)?,
        expedited: u32::try_from(expedited)?,
        jobs: status(&tx)?,
    };
    tx.commit()?;
    Ok(result)
}

/// 失効leaseの回収と新しい所有者の確定を1transactionで行う。
pub fn claim(conn: &Connection, now: i64, lease_seconds: i64) -> Result<Option<JobLease>> {
    if lease_seconds <= 0 {
        bail!("蒸留leaseの有効秒数は正数で指定する");
    }
    let expires_at = now
        .checked_add(lease_seconds)
        .ok_or_else(|| anyhow::anyhow!("蒸留leaseの期限が範囲外"))?;
    let tx = conn.unchecked_transaction()?;
    // 最初のUPDATEでwrite lockを取得するため、複数workerが同じ候補をclaimしない。
    tx.execute(
        "UPDATE distillation_jobs SET state='retry_wait', available_at=?1,
             lease_token=NULL, lease_expires_at=NULL, last_error='lease_expired'
         WHERE state='running' AND lease_expires_at<=?1",
        [now],
    )?;
    let candidate = tx
        .query_row(
            "SELECT j.note, j.generation, n.document, j.attempt FROM distillation_jobs j
             JOIN notes n ON n.id=j.note
             WHERE j.state IN ('pending', 'retry_wait') AND j.available_at<=?1
                 AND n.normal_reference_allowed=1 AND n.distillation_allowed=1 AND n.document<>''
             ORDER BY CASE WHEN j.queued_at<=?1-86400 THEN 0
                 WHEN j.reason IN ('created', 'changed') THEN 1 ELSE 2 END,
                 j.queued_at, j.note LIMIT 1",
            [now],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u32>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((note, generation, document, previous_attempt)) = candidate else {
        tx.commit()?;
        return Ok(None);
    };
    let attempt = previous_attempt.saturating_add(1);
    let token = format!("{:032x}", rand::random::<u128>());
    tx.execute(
        "UPDATE distillation_jobs SET state='running', lease_token=?1, lease_expires_at=?2,
             attempt=?3 WHERE note=?4 AND generation=?5",
        params![token, expires_at, attempt, note, generation],
    )?;
    tx.commit()?;
    Ok(Some(JobLease {
        note,
        generation,
        token,
        input_hash: crate::distillation::sha256(document.as_bytes()),
        expires_at,
        attempt,
    }))
}

#[derive(Debug)]
struct BatchCandidate {
    note: String,
    generation: i64,
    previous_attempt: u32,
    queued_at: i64,
    available_at: i64,
    scope: Option<String>,
    uid: Option<String>,
    document_bytes: usize,
}

impl BatchCandidate {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let document_bytes = row.get::<_, i64>(7)?;
        Ok(Self {
            note: row.get(0)?,
            generation: row.get(1)?,
            previous_attempt: row.get(2)?,
            queued_at: row.get(3)?,
            available_at: row.get(4)?,
            scope: row.get(5)?,
            uid: row.get(6)?,
            document_bytes: usize::try_from(document_bytes)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(7, document_bytes))?,
        })
    }

    fn ready_without_group(&self, now: i64) -> bool {
        self.available_at == MANUAL_AVAILABLE_AT
            || self.previous_attempt > 0
            || self.queued_at <= now.saturating_sub(BATCH_MAX_WAIT_SECONDS)
    }
}

/// 関連する原記録だけを小さくまとめ、全leaseを同じ書込transactionで取得する。
/// claimを繰り返す方式では途中失敗で一部だけ実行中になるため、ここで一括確定する。
pub(crate) fn claim_batch(
    conn: &Connection,
    now: i64,
    lease_seconds: i64,
) -> Result<Vec<JobLease>> {
    if now < 0 || lease_seconds <= 0 {
        bail!("蒸留batchの時刻とleaseの有効秒数が範囲外");
    }
    let expires_at = now
        .checked_add(lease_seconds)
        .ok_or_else(|| anyhow::anyhow!("蒸留leaseの期限が範囲外"))?;
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    tx.execute(
        "UPDATE distillation_jobs SET state='retry_wait', available_at=?1,
             lease_token=NULL, lease_expires_at=NULL, last_error='lease_expired'
         WHERE state='running' AND lease_expires_at<=?1",
        [now],
    )?;
    // 全体snapshotを使う間は同時batchを走らせない。別processのworkerにも同じ制約を適用する。
    let running: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM distillation_jobs WHERE state='running')",
        [],
        |row| row.get(0),
    )?;
    if running {
        tx.commit()?;
        return Ok(Vec::new());
    }
    // 直近の新規1件が蓄積待ちでも、既に期限を迎えた後続の仕事を進める。
    // 候補32件だけを読むので、大きなInboxの全本文をworkerのtickごとに取り込まない。
    let seeds = tx
        .prepare(
            "SELECT j.note, j.generation, j.attempt, j.queued_at, j.available_at,
                 n.authority_scope, n.note_uid, length(CAST(n.document AS BLOB))
             FROM distillation_jobs j JOIN notes n ON n.id=j.note
             WHERE j.state IN ('pending', 'retry_wait') AND j.available_at<=?1
                 AND n.normal_reference_allowed=1 AND n.distillation_allowed=1 AND n.document<>''
             ORDER BY CASE WHEN j.queued_at<=?1-86400 THEN 0
                 WHEN j.reason IN ('created', 'changed') THEN 1 ELSE 2 END,
                 j.queued_at, j.note LIMIT 32",
        )?
        .query_map([now], BatchCandidate::from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut selected = Vec::new();
    for seed in seeds {
        let ready = seed.ready_without_group(now);
        let candidates = batch_for_seed(&tx, seed, now)?;
        if ready || candidates.len() >= MIN_BATCH_NOTES {
            selected = candidates;
            break;
        }
    }
    if selected.is_empty() {
        // 2026-09-07: 新規32件がすべて蓄積待ちだと、後ろの期限済み定期レビューが
        // 見えずworkerが空転した。本文の読出し上限を増やさず、実行可能な先頭1件を補う。
        let overdue = tx
            .query_row(
                "SELECT j.note, j.generation, j.attempt, j.queued_at, j.available_at,
                 n.authority_scope, n.note_uid, length(CAST(n.document AS BLOB))
             FROM distillation_jobs j JOIN notes n ON n.id=j.note
             WHERE j.state IN ('pending', 'retry_wait') AND j.available_at<=?1
                 AND (j.queued_at<=?2 OR j.available_at=-1 OR j.attempt>0)
                 AND n.normal_reference_allowed=1 AND n.distillation_allowed=1 AND n.document<>''
             ORDER BY CASE WHEN j.queued_at<=?1-86400 THEN 0
                 WHEN j.reason IN ('created', 'changed') THEN 1 ELSE 2 END,
                 j.queued_at, j.note LIMIT 1",
                params![now, now.saturating_sub(BATCH_MAX_WAIT_SECONDS)],
                BatchCandidate::from_row,
            )
            .optional()?;
        if let Some(seed) = overdue {
            selected = batch_for_seed(&tx, seed, now)?;
        }
    }
    let mut leases = Vec::with_capacity(selected.len());
    for candidate in selected {
        let document: String = tx.query_row(
            "SELECT document FROM notes WHERE id=?1",
            [&candidate.note],
            |row| row.get(0),
        )?;
        let token = format!("{:032x}", rand::random::<u128>());
        let attempt = candidate.previous_attempt.saturating_add(1);
        let changed = tx.execute(
            "UPDATE distillation_jobs SET state='running', lease_token=?1, lease_expires_at=?2,
                 attempt=?3 WHERE note=?4 AND generation=?5
                 AND state IN ('pending', 'retry_wait') AND available_at<=?6",
            params![
                token,
                expires_at,
                attempt,
                candidate.note,
                candidate.generation,
                now
            ],
        )?;
        if changed != 1 {
            return Err(LeaseChanged.into());
        }
        leases.push(JobLease {
            note: candidate.note,
            generation: candidate.generation,
            token,
            input_hash: crate::distillation::sha256(document.as_bytes()),
            expires_at,
            attempt,
        });
    }
    tx.commit()?;
    Ok(leases)
}

fn batch_for_seed(
    conn: &Connection,
    seed: BatchCandidate,
    now: i64,
) -> Result<Vec<BatchCandidate>> {
    // 失敗した集合を同じ形で再送し続けず、再試行時は原記録単位に切り分ける。
    let mut related = if seed.previous_attempt == 0 && seed.document_bytes < MAX_BATCH_SOURCE_BYTES
    {
        related_batch_candidates(conn, &seed, now)?
    } else {
        Vec::new()
    };
    let mut bytes = seed.document_bytes;
    related.retain(|candidate| {
        if bytes.saturating_add(candidate.document_bytes) > MAX_BATCH_SOURCE_BYTES {
            return false;
        }
        bytes += candidate.document_bytes;
        true
    });
    related.truncate(MAX_BATCH_NOTES - 1);
    let mut candidates = vec![seed];
    candidates.extend(related);
    Ok(candidates)
}

fn related_batch_candidates(
    conn: &Connection,
    seed: &BatchCandidate,
    now: i64,
) -> Result<Vec<BatchCandidate>> {
    // 共通タグだけでは意味的な近さを保証しない。同じscope、明示的な参照先、
    // その参照先を共有する記録を候補にし、最終的な統合可否はAIの全文確認に委ねる。
    Ok(conn
        .prepare(
            "WITH anchors(uid) AS (
                 SELECT ?4 WHERE ?4 IS NOT NULL
                 UNION SELECT target_uid FROM note_relations WHERE src_uid=?4
             )
             SELECT j.note, j.generation, j.attempt, j.queued_at, j.available_at,
                 n.authority_scope, n.note_uid, length(CAST(n.document AS BLOB))
             FROM distillation_jobs j JOIN notes n ON n.id=j.note
             WHERE j.note<>?2 AND j.attempt=0
                 AND j.state IN ('pending', 'retry_wait') AND j.available_at<=?1
                 AND n.normal_reference_allowed=1 AND n.distillation_allowed=1 AND n.document<>''
                 AND ((?3 IS NOT NULL AND trim(?3)<>'' AND n.authority_scope=?3)
                     OR n.note_uid IN (SELECT uid FROM anchors)
                     OR EXISTS(SELECT 1 FROM note_relations relation
                         WHERE relation.src_uid=n.note_uid
                             AND relation.target_uid IN (SELECT uid FROM anchors)))
             ORDER BY CASE WHEN j.queued_at<=?1-86400 THEN 0
                 WHEN j.reason IN ('created', 'changed') THEN 1 ELSE 2 END,
                 j.queued_at, j.note LIMIT 24",
        )?
        .query_map(
            params![now, seed.note, seed.scope, seed.uid],
            BatchCandidate::from_row,
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?)
}

fn require_transaction(conn: &Connection) -> Result<()> {
    if conn.is_autocommit() {
        bail!("蒸留の検証と完了記録には同一write transactionが必要");
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct LeaseChanged;

impl std::fmt::Display for LeaseChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("蒸留leaseが失効したか、対象ノートの版が変更された")
    }
}

impl std::error::Error for LeaseChanged {}

/// 呼出側は書込lockを取得後、本文を変更する前に検証する。
pub fn verify_lease_in_tx(conn: &Connection, lease: &JobLease, now: i64) -> Result<()> {
    require_transaction(conn)?;
    let document = conn
        .query_row(
            "SELECT n.document FROM distillation_jobs j JOIN notes n ON n.id=j.note
             WHERE j.note=?1 AND j.generation=?2 AND j.state='running'
                 AND j.lease_token=?3 AND j.lease_expires_at>?4
                 AND n.normal_reference_allowed=1 AND n.distillation_allowed=1 AND n.document<>''",
            params![lease.note, lease.generation, lease.token, now],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if document
        .is_none_or(|document| crate::distillation::sha256(document.as_bytes()) != lease.input_hash)
    {
        return Err(LeaseChanged.into());
    }
    Ok(())
}

/// 対象本文が変わらない確認も完了として残す。関連正本のみの変更にも使える。
pub fn finish_lease_in_tx(
    conn: &Connection,
    lease: &JobLease,
    now: i64,
    outcome: CompletionOutcome,
    reason: &str,
) -> Result<()> {
    verify_lease_in_tx(conn, lease, now)?;
    validate_reason(reason)?;
    conn.execute(
        "UPDATE distillation_jobs SET state='completed', reason=?1, last_reviewed_at=?2,
             reviewed_hash=?3, outcome=?4, lease_token=NULL, lease_expires_at=NULL,
             last_error=NULL, available_at=0
         WHERE note=?5 AND generation=?6 AND lease_token=?7",
        params![
            reason,
            now,
            lease.input_hash,
            outcome.as_str(),
            lease.note,
            lease.generation,
            lease.token
        ],
    )?;
    Ok(())
}

/// 呼出側が同じtransactionでleaseを検証し、自分で変更した出力だけを渡す。
/// 更新triggerが作った次世代を、このwaveで確認した出力hashに限って閉じる。
pub fn settle_outputs_in_tx(
    conn: &Connection,
    outputs: &[(String, String)],
    now: i64,
    reason: &str,
) -> Result<()> {
    require_transaction(conn)?;
    validate_reason(reason)?;
    for (note, expected_hash) in outputs {
        let document: String = conn.query_row(
            "SELECT document FROM notes WHERE id=?1 AND normal_reference_allowed=1 AND distillation_allowed=1",
            [note],
            |row| row.get(0),
        )?;
        if crate::distillation::sha256(document.as_bytes()) != *expected_hash {
            bail!("蒸留出力の版が変更された: {note}");
        }
        let affected = conn.execute(
            "UPDATE distillation_jobs SET state='completed', reason=?1,
                 last_reviewed_at=?2, reviewed_hash=?3, outcome='applied',
                 lease_token=NULL, lease_expires_at=NULL, last_error=NULL, available_at=0
             WHERE note=?4 AND state='pending'",
            params![reason, now, expected_hash, note],
        )?;
        if affected != 1 {
            bail!("このwaveの蒸留出力が処理待ちとして登録されていない: {note}");
        }
    }
    Ok(())
}

fn validate_reason(reason: &str) -> Result<()> {
    if reason.trim().is_empty() {
        bail!("蒸留の完了・保留には確認理由が必要");
    }
    Ok(())
}

/// 古いworkerからの失敗通知は最新の世代や新しい所有者に影響させない。
pub fn fail(
    conn: &Connection,
    lease: &JobLease,
    now: i64,
    reason: &str,
    blocked: bool,
) -> Result<bool> {
    validate_reason(reason)?;
    // 連続障害時にAPIを叩き続けない。初回30秒から最大1時間まで待つ。
    let delay = 30_i64
        .saturating_mul(1_i64 << lease.attempt.saturating_sub(1).min(7))
        .min(3_600);
    let available_at = now.saturating_add(delay);
    Ok(conn.execute(
        "UPDATE distillation_jobs SET state=?1, available_at=?2, last_error=?3,
             lease_token=NULL, lease_expires_at=NULL
         WHERE note=?4 AND generation=?5 AND lease_token=?6 AND state='running'
             AND lease_expires_at>?7",
        params![
            if blocked { "blocked" } else { "retry_wait" },
            available_at,
            reason,
            lease.note,
            lease.generation,
            lease.token,
            now
        ],
    )? == 1)
}

/// 設定した間隔を過ぎた確認済みノートだけを再登録する。
/// 同じ版でも新しい世代になり、次のtickで繰り返し登録されることはない。
pub fn enqueue_due_reviews(conn: &Connection, now: i64, interval_seconds: i64) -> Result<usize> {
    if interval_seconds <= 0 {
        bail!("再蒸留間隔は正数で指定する");
    }
    Ok(conn.execute(
        "UPDATE distillation_jobs SET generation=generation+1, state='pending',
             reason='periodic', queued_at=?1, available_at=0, attempt=0,
             lease_token=NULL, lease_expires_at=NULL, last_error=NULL
         WHERE state='completed' AND last_reviewed_at<=?2
             AND EXISTS(SELECT 1 FROM notes n WHERE n.id=distillation_jobs.note
                 AND n.normal_reference_allowed=1 AND n.distillation_allowed=1 AND n.document<>'')",
        params![now, now.saturating_sub(interval_seconds)],
    )?)
}

/// 認証復旧や設定変更後の明示的な再試行。実行中のleaseを横取りせず、単独化を保つ。
pub fn retry_failed(conn: &Connection, now: i64) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE distillation_jobs SET state='pending', available_at=?1,
             lease_token=NULL, lease_expires_at=NULL, last_error=NULL
         WHERE state IN ('retry_wait', 'blocked')",
        [now],
    )?)
}

#[derive(Debug, Clone, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct NoteJobStatus {
    pub state: String,
    pub generation: i64,
    pub reviewed_hash: Option<String>,
    pub last_reviewed_at: Option<i64>,
    pub attempt: u32,
    pub last_error: Option<String>,
}

/// 過去の確認hashはpendingでも残るため、現行版の確認済み判定にはstateも必須。
pub fn note_status(conn: &Connection, id: &str) -> Result<Option<NoteJobStatus>> {
    Ok(conn
        .query_row(
            "SELECT state, generation, reviewed_hash, last_reviewed_at, attempt, last_error
         FROM distillation_jobs WHERE note=?1",
            [id],
            |row| {
                Ok(NoteJobStatus {
                    state: row.get(0)?,
                    generation: row.get(1)?,
                    reviewed_hash: row.get(2)?,
                    last_reviewed_at: row.get(3)?,
                    attempt: row.get(4)?,
                    last_error: row.get(5)?,
                })
            },
        )
        .optional()?)
}

#[derive(Debug, Clone, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct JobIssue {
    pub note: String,
    pub title: Option<String>,
    pub state: String,
    pub last_error: Option<String>,
    pub available_at: i64,
    pub attempt: u32,
}

/// 対処を要する失敗を先に示す。本文は読み出さず、設定画面へ出す件数を制限する。
pub fn issues(conn: &Connection, limit: usize) -> Result<Vec<JobIssue>> {
    let mut statement = conn.prepare(
        "SELECT j.note, n.title, j.state, j.last_error, j.available_at, j.attempt
         FROM distillation_jobs j JOIN notes n ON n.id=j.note
         WHERE j.state IN ('blocked', 'retry_wait')
             AND n.normal_reference_allowed=1 AND n.distillation_allowed=1 AND n.document<>''
         ORDER BY CASE j.state WHEN 'blocked' THEN 0 ELSE 1 END, j.queued_at, j.note
         LIMIT ?1",
    )?;
    Ok(statement
        .query_map([i64::try_from(limit.min(20))?], |row| {
            Ok(JobIssue {
                note: row.get(0)?,
                title: row.get(1)?,
                state: row.get(2)?,
                last_error: row.get(3)?,
                available_at: row.get(4)?,
                attempt: row.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?)
}

pub fn status(conn: &Connection) -> Result<JobStatus> {
    let mut status = JobStatus::default();
    let mut statement =
        conn.prepare("SELECT state, count(*) FROM distillation_jobs GROUP BY state")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
    })?;
    for row in rows {
        let (state, count) = row?;
        match state.as_str() {
            "pending" => status.pending = count,
            "running" => status.running = count,
            "retry_wait" => status.retry_wait = count,
            "blocked" => status.blocked = count,
            "completed" => status.completed = count,
            _ => bail!("未知の蒸留状態: {state}"),
        }
    }
    status.oldest_pending_at = conn.query_row(
        "SELECT min(queued_at) FROM distillation_jobs WHERE state<>'completed'",
        [],
        |row| row.get(0),
    )?;
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE notes(id TEXT PRIMARY KEY, title TEXT, document TEXT NOT NULL,
                 authority_scope TEXT, note_uid TEXT,
                 normal_reference_allowed INTEGER NOT NULL,
                 distillation_allowed INTEGER NOT NULL DEFAULT 1);
             CREATE TABLE note_relations(src_uid TEXT, kind TEXT, target_uid TEXT,
                 PRIMARY KEY(src_uid, kind, target_uid));",
        )
        .unwrap();
        conn.execute_batch(SCHEMA_SQL).unwrap();
        conn
    }

    fn insert(conn: &Connection, note: &str, document: &str) {
        conn.execute(
            "INSERT INTO notes(id,document,normal_reference_allowed) VALUES(?1, ?2, 1)",
            params![note, document],
        )
        .unwrap();
    }

    fn finish(conn: &Connection, lease: &JobLease, now: i64) {
        let tx = conn.unchecked_transaction().unwrap();
        finish_lease_in_tx(&tx, lease, now, CompletionOutcome::NoChange, "確認済み").unwrap();
        tx.commit().unwrap();
    }

    fn fixture_note(title: &str) -> crate::frontmatter::Note {
        use crate::authority::{Authority, AuthorityRole, AuthorityStatus, NoteNamespace, NoteUid};
        let mut front = crate::frontmatter::Frontmatter::new_note(title);
        front.origin = Some("agent".into());
        front.tags = vec!["test".into()];
        front.note_uid = Some(NoteUid::new());
        front.authority = Some(Authority {
            namespace: NoteNamespace::Records,
            role: AuthorityRole::Record,
            status: AuthorityStatus::Historical,
            scope: "tests/immediate-distillation".into(),
        });
        crate::frontmatter::Note {
            front,
            body: "そのまま保持する原記録。".into(),
        }
    }

    fn queue_row(conn: &Connection, id: &str) -> Vec<rusqlite::types::Value> {
        conn.query_row(
            "SELECT * FROM distillation_jobs WHERE note=?1",
            [id],
            |row| {
                (0..row.as_ref().column_count())
                    .map(|index| row.get(index))
                    .collect()
            },
        )
        .unwrap()
    }

    fn documents(conn: &Connection) -> Vec<(String, String)> {
        conn.prepare("SELECT id,document FROM notes ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }

    fn insert_batch_candidate(conn: &Connection, id: &str, scope: &str, queued_at: i64) {
        insert(conn, id, &fixture_note(id).to_file_string().unwrap());
        conn.execute(
            "UPDATE notes SET authority_scope=?1, note_uid=?2 WHERE id=?2",
            params![scope, id],
        )
        .unwrap();
        conn.execute(
            "UPDATE distillation_jobs SET queued_at=?1 WHERE note=?2",
            params![queued_at, id],
        )
        .unwrap();
    }

    fn finish_batch(conn: &Connection, leases: &[JobLease], now: i64) {
        let tx = conn.unchecked_transaction().unwrap();
        for lease in leases {
            finish_lease_in_tx(&tx, lease, now, CompletionOutcome::NoChange, "確認済み").unwrap();
        }
        tx.commit().unwrap();
    }

    /// 2026-09-07: 保存ごとのAI起動を止めても、少量のInboxを永久に待たせない。
    #[test]
    fn batch_waits_for_related_threshold_or_original_maximum_age() {
        let conn = setup();
        insert_batch_candidate(&conn, "a", "recipes/nikujaga", 100);
        insert_batch_candidate(&conn, "b", "recipes/nikujaga", 110);
        assert!(claim_batch(&conn, 120, 60).unwrap().is_empty());
        assert!(claim_batch(&conn, 219, 60).unwrap().is_empty());
        let expired_wait = claim_batch(&conn, 220, 60).unwrap();
        assert_eq!(expired_wait.len(), 2);
        finish_batch(&conn, &expired_wait, 221);
        for id in ["c", "d", "e"] {
            insert_batch_candidate(&conn, id, "recipes/nikujaga", 230);
        }
        let threshold = claim_batch(&conn, 230, 60).unwrap();
        assert_eq!(threshold.len(), 3);
        assert_eq!(status(&conn).unwrap().running, 3);
        assert_eq!(threshold[0].expires_at, threshold[2].expires_at);
        assert_ne!(threshold[0].token, threshold[1].token);
        assert_ne!(threshold[1].token, threshold[2].token);
    }

    #[test]
    fn batch_groups_explicit_shared_targets_but_not_common_tags_or_unrelated_scopes() {
        let conn = setup();
        for (id, scope) in [
            ("a", "record/a"),
            ("b", "record/b"),
            ("c", "record/c"),
            ("unrelated", "record/unrelated"),
        ] {
            insert_batch_candidate(&conn, id, scope, 100);
        }
        // 全ノートは同じtestタグだが、その一致だけでは集合にしない。
        assert!(claim_batch(&conn, 100, 60).unwrap().is_empty());
        for id in ["a", "b", "c"] {
            conn.execute(
                "INSERT INTO note_relations(src_uid,kind,target_uid) VALUES(?1,'supports','canonical')",
                [id],
            )
            .unwrap();
        }
        let batch = claim_batch(&conn, 100, 60).unwrap();
        assert_eq!(
            batch
                .iter()
                .map(|lease| lease.note.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        assert_eq!(
            note_status(&conn, "unrelated").unwrap().unwrap().state,
            "pending"
        );
    }

    #[test]
    fn batch_has_bounded_members_and_drains_existing_backlog_without_new_delay() {
        let conn = setup();
        for index in 0..8 {
            insert_batch_candidate(&conn, &format!("note-{index}"), "same-topic", 100);
        }
        let first = claim_batch(&conn, 220, 60).unwrap();
        assert_eq!(first.len(), MAX_BATCH_NOTES);
        assert!(claim_batch(&conn, 220, 60).unwrap().is_empty());
        finish_batch(&conn, &first, 221);
        let second = claim_batch(&conn, 221, 60).unwrap();
        assert_eq!(second.len(), 2);
        assert!(
            second
                .iter()
                .all(|lease| first.iter().all(|old| old.note != lease.note))
        );
    }

    #[test]
    fn batch_source_byte_budget_keeps_large_sources_separate_without_dropping_them() {
        let conn = setup();
        for id in ["a", "b", "c"] {
            insert_batch_candidate(&conn, id, "same-topic", 100);
            conn.execute(
                "UPDATE notes SET document=?1 WHERE id=?2",
                params!["a".repeat(40 * 1024), id],
            )
            .unwrap();
        }
        conn.execute("UPDATE distillation_jobs SET queued_at=100", [])
            .unwrap();
        let first = claim_batch(&conn, 220, 60).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(status(&conn).unwrap().pending, 2);
        finish_batch(&conn, &first, 221);
        conn.execute(
            "UPDATE notes SET document=?1 WHERE id='b'",
            ["b".repeat(70 * 1024)],
        )
        .unwrap();
        conn.execute(
            "UPDATE distillation_jobs SET queued_at=90 WHERE note='b'",
            [],
        )
        .unwrap();
        let oversized = claim_batch(&conn, 221, 60).unwrap();
        assert_eq!(oversized.len(), 1);
        assert_eq!(oversized[0].note, "b");
    }

    /// 2026-09-07: 即時ボタンは蓄積待ちを飛ばすが、実行中のbatchを横取りしない。
    #[test]
    fn batch_manual_flush_survives_repeated_requests_and_preserves_active_work() {
        let conn = setup();
        insert_batch_candidate(&conn, "active", "active-topic", 100);
        request_now(&conn, ImmediateDistillationScope::Unreviewed, 100).unwrap();
        let active = claim_batch(&conn, 100, 60).unwrap();
        assert_eq!(active.len(), 1);
        insert_batch_candidate(&conn, "fresh", "new-topic", 110);
        let active_before = queue_row(&conn, "active");
        request_now(&conn, ImmediateDistillationScope::Unreviewed, 110).unwrap();
        let first_flush = queue_row(&conn, "fresh");
        request_now(&conn, ImmediateDistillationScope::Unreviewed, 111).unwrap();
        assert_eq!(queue_row(&conn, "fresh"), first_flush);
        assert_eq!(queue_row(&conn, "active"), active_before);
        assert!(claim_batch(&conn, 111, 60).unwrap().is_empty());
        finish_batch(&conn, &active, 112);
        let next = claim_batch(&conn, 112, 60).unwrap();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].note, "fresh");
        let queued_at: i64 = conn
            .query_row(
                "SELECT queued_at FROM distillation_jobs WHERE note='fresh'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(queued_at, 110);
    }

    #[test]
    fn batch_claim_rolls_back_every_member_if_any_claim_fails() {
        let conn = setup();
        for id in ["a", "b", "c"] {
            insert_batch_candidate(&conn, id, "same-topic", 100);
        }
        let before: Vec<_> = ["a", "b", "c"]
            .iter()
            .map(|id| queue_row(&conn, id))
            .collect();
        conn.execute_batch(
            "CREATE TRIGGER reject_second_batch_claim BEFORE UPDATE ON distillation_jobs
             WHEN OLD.note='b' AND NEW.state='running'
             BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;",
        )
        .unwrap();
        assert!(claim_batch(&conn, 100, 60).is_err());
        let after: Vec<_> = ["a", "b", "c"]
            .iter()
            .map(|id| queue_row(&conn, id))
            .collect();
        assert_eq!(after, before);
        assert_eq!(status(&conn).unwrap().running, 0);
    }

    #[test]
    fn failed_or_expired_batch_retries_as_singletons_with_backoff() {
        let conn = setup();
        for id in ["a", "b", "c"] {
            insert_batch_candidate(&conn, id, "same-topic", 100);
        }
        let first = claim_batch(&conn, 100, 60).unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        for lease in &first {
            assert!(fail(&tx, lease, 110, "batch_split", false).unwrap());
        }
        tx.commit().unwrap();
        assert!(claim_batch(&conn, 139, 60).unwrap().is_empty());
        let retried = claim_batch(&conn, 140, 60).unwrap();
        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0].attempt, 2);
        // workerが終了しても次の所有者は旧leaseを再利用しない。
        let recovered = claim_batch(&conn, 200, 60).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].attempt, 3);
        assert_ne!(recovered[0].token, retried[0].token);
        assert_eq!(status(&conn).unwrap().retry_wait, 2);
    }

    /// 2026-09-07: 即時操作と設定保存で試行回数を消すと、失敗した集合を再結成してしまう。
    #[test]
    fn explicit_retries_preserve_failed_batch_isolation_and_queue_age() {
        for blocked in [false, true] {
            for retry_via_settings in [false, true] {
                let conn = setup();
                for id in ["a", "b", "c"] {
                    insert_batch_candidate(&conn, id, "same-topic", 100);
                }
                let batch = claim_batch(&conn, 100, 60).unwrap();
                for lease in &batch {
                    assert!(fail(&conn, lease, 110, "batch_split", blocked).unwrap());
                }
                if retry_via_settings {
                    retry_failed(&conn, 111).unwrap();
                } else {
                    request_now(&conn, ImmediateDistillationScope::Unreviewed, 111).unwrap();
                }
                for _ in 0..3 {
                    let retry = claim_batch(&conn, 111, 60).unwrap();
                    assert_eq!(retry.len(), 1);
                    assert_eq!(retry[0].attempt, 2);
                    let queued_at: i64 = conn
                        .query_row(
                            "SELECT queued_at FROM distillation_jobs WHERE note=?1",
                            [&retry[0].note],
                            |row| row.get(0),
                        )
                        .unwrap();
                    assert_eq!(queued_at, 100);
                    finish_batch(&conn, &retry, 112);
                }
                assert_eq!(status(&conn).unwrap().completed, 3);
            }
        }
    }

    #[test]
    fn batch_prioritizes_new_work_but_aged_work_cannot_starve() {
        let conn = setup();
        insert_batch_candidate(&conn, "periodic", "periodic-topic", 100);
        conn.execute(
            "UPDATE distillation_jobs SET reason='periodic' WHERE note='periodic'",
            [],
        )
        .unwrap();
        insert_batch_candidate(&conn, "fresh", "fresh-topic", 200);
        // 新規が蓄積待ちでも、期限済みの後続を止めない。
        let due = claim_batch(&conn, 250, 60).unwrap();
        assert_eq!(due[0].note, "periodic");
        finish_batch(&conn, &due, 251);
        conn.execute("UPDATE distillation_jobs SET state='pending', reason='periodic', queued_at=100 WHERE note='periodic'", []).unwrap();
        let fresh = claim_batch(&conn, 320, 60).unwrap();
        assert_eq!(fresh[0].note, "fresh");
        finish_batch(&conn, &fresh, 321);
        insert_batch_candidate(&conn, "newer", "newer-topic", 86_300);
        let aged = claim_batch(&conn, 86_500, 60).unwrap();
        assert_eq!(aged[0].note, "periodic");
    }

    /// 2026-09-07: 関係のない新規32件の蓄積待ちで、期限済みの後続が隠れて空転した。
    #[test]
    fn fresh_candidate_window_does_not_hide_overdue_work() {
        let conn = setup();
        for index in 0..32 {
            insert_batch_candidate(
                &conn,
                &format!("fresh-{index:02}"),
                &format!("topic-{index}"),
                1_000,
            );
        }
        for id in ["periodic-a", "periodic-b"] {
            insert_batch_candidate(&conn, id, "periodic-topic", 880);
            conn.execute(
                "UPDATE distillation_jobs SET reason='periodic' WHERE note=?1",
                [id],
            )
            .unwrap();
        }
        let leases = claim_batch(&conn, 1_000, 60).unwrap();
        assert_eq!(
            leases
                .iter()
                .map(|lease| lease.note.as_str())
                .collect::<Vec<_>>(),
            ["periodic-a", "periodic-b"]
        );
        assert_eq!(status(&conn).unwrap().pending, 32);
        finish_batch(&conn, &leases, 1_001);
        assert!(claim_batch(&conn, 1_001, 60).unwrap().is_empty());
    }

    #[test]
    #[ignore = "合成キュー1千・1万・5万件の選択時間を明示的に測る"]
    fn batch_queue_scale_benchmark() {
        use std::time::Instant;

        fn fixture(count: usize) -> Connection {
            let conn = setup();
            conn.execute_batch(
                "CREATE UNIQUE INDEX notes_note_uid ON notes(note_uid) WHERE note_uid IS NOT NULL;
                 CREATE INDEX note_relations_target ON note_relations(target_uid);",
            )
            .unwrap();
            let tx = conn.unchecked_transaction().unwrap();
            {
                let mut statement = tx.prepare(
                    "INSERT INTO notes(id,title,document,authority_scope,note_uid,normal_reference_allowed)
                     VALUES(?1,?2,?3,?4,?5,1)"
                ).unwrap();
                for index in 0..count {
                    let id = format!("note-{index:05}");
                    let mut note = fixture_note(&id);
                    let scope = format!("benchmark/topic-{:05}", index / 100);
                    note.front.authority.as_mut().unwrap().scope = scope.clone();
                    note.body = format!(
                        "合成の原記録{index}。同じ話題の関連をまとめて確認する性能試験。実データを含まない。"
                    );
                    statement
                        .execute(params![
                            id,
                            note.front.title,
                            note.to_file_string().unwrap(),
                            scope,
                            note.front.note_uid.as_ref().unwrap().to_string()
                        ])
                        .unwrap();
                }
            }
            tx.execute("UPDATE distillation_jobs SET queued_at=100", [])
                .unwrap();
            tx.commit().unwrap();
            conn
        }

        fn reset_claims(conn: &Connection) {
            conn.execute(
                "UPDATE distillation_jobs SET state='pending', attempt=0,
                     lease_token=NULL, lease_expires_at=NULL, available_at=0",
                [],
            )
            .unwrap();
        }

        let mut measurements = Vec::new();
        for count in [1_000, 10_000, 50_000] {
            let setup_start = Instant::now();
            let conn = fixture(count);
            let setup_ms = setup_start.elapsed().as_secs_f64() * 1_000.0;
            let mut samples_ms = Vec::new();
            for _ in 0..3 {
                let started = Instant::now();
                let leases = claim_batch(&conn, 220, 600).unwrap();
                samples_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
                assert_eq!(leases.len(), MAX_BATCH_NOTES);
                reset_claims(&conn);
            }
            measurements.push(serde_json::json!({
                "notes":count,"scenario":"related_scope_100_notes_per_group",
                "sample_ms":samples_ms,"members":MAX_BATCH_NOTES,"fixture_setup_ms":setup_ms
            }));
            if count == 50_000 {
                // 新規の蓄積待ち32件で候補窓を埋め、期限済みの定期見直しが後ろにある状況。
                // 本文と派生scopeを合わせ、AIの無関係な話題統合を速度として数えない。
                let tx = conn.unchecked_transaction().unwrap();
                for index in 0..32 {
                    let id = format!("note-{index:05}");
                    let stored: String = tx
                        .query_row("SELECT document FROM notes WHERE id=?1", [&id], |row| {
                            row.get(0)
                        })
                        .unwrap();
                    let mut note = crate::frontmatter::Note::parse(&stored).unwrap();
                    let scope = format!("benchmark/fresh-unrelated-{index:05}");
                    note.front.authority.as_mut().unwrap().scope = scope.clone();
                    tx.execute(
                        "UPDATE notes SET authority_scope=?1,document=?2 WHERE id=?3",
                        params![scope, note.to_file_string().unwrap(), id],
                    )
                    .unwrap();
                }
                tx.execute(
                    "UPDATE distillation_jobs SET reason='periodic',queued_at=880",
                    [],
                )
                .unwrap();
                tx.execute("UPDATE distillation_jobs SET reason='created',queued_at=1000 WHERE note<'note-00032'", []).unwrap();
                tx.commit().unwrap();
                let mut samples_ms = Vec::new();
                let mut members = Vec::new();
                for _ in 0..3 {
                    let started = Instant::now();
                    let leases = claim_batch(&conn, 1_000, 600).unwrap();
                    samples_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
                    members.push(leases.len());
                    reset_claims(&conn);
                }
                measurements.push(serde_json::json!({
                    "notes":count,"scenario":"fresh_32_unrelated_before_due_periodic",
                    "sample_ms":samples_ms,"members":members
                }));
                let started = Instant::now();
                let leases = claim_batch(&conn, 1_120, 600).unwrap();
                let waited_ms = started.elapsed().as_secs_f64() * 1_000.0;
                assert_eq!(leases.len(), 1);
                measurements.push(serde_json::json!({
                    "notes":count,"scenario":"unrelated_first_source_after_max_wait",
                    "sample_ms":[waited_ms],"members":leases.len()
                }));
            }
        }
        println!(
            "BATCH_QUEUE_BENCHMARK {}",
            serde_json::to_string(&measurements).unwrap()
        );
    }

    /// 2026-09-07: 即時操作は既存workerへ合流させ、実行中や二度押しの世代を壊さない。
    #[test]
    fn immediate_unreviewed_preserves_active_leases_and_is_idempotent() {
        let conn = setup();
        insert(
            &conn,
            "a-running",
            &fixture_note("実行中").to_file_string().unwrap(),
        );
        let running = claim(&conn, 100, 300).unwrap().unwrap();
        insert(
            &conn,
            "b-expired",
            &fixture_note("期限切れの実行中").to_file_string().unwrap(),
        );
        let expired = claim(&conn, 100, 5).unwrap().unwrap();
        for id in [
            "pending",
            "future",
            "retry",
            "blocked",
            "completed",
            "missing",
        ] {
            insert(&conn, id, &fixture_note(id).to_file_string().unwrap());
        }
        conn.execute("UPDATE distillation_jobs SET state='retry_wait', available_at=900, attempt=7, queued_at=10, last_error='timed_out' WHERE note='retry'", []).unwrap();
        conn.execute("UPDATE distillation_jobs SET state='blocked', attempt=3, queued_at=20, last_error='review_no_progress' WHERE note='blocked'", []).unwrap();
        conn.execute("UPDATE distillation_jobs SET available_at=900, attempt=2, queued_at=30 WHERE note='future'", []).unwrap();
        conn.execute("UPDATE distillation_jobs SET state='completed', reviewed_hash='past-hash', last_reviewed_at=90, outcome='no_change' WHERE note='completed'", []).unwrap();
        conn.execute("DELETE FROM distillation_jobs WHERE note='missing'", [])
            .unwrap();
        let before = documents(&conn);
        let running_before = queue_row(&conn, &running.note);
        let expired_before = queue_row(&conn, &expired.note);
        let completed_before = queue_row(&conn, "completed");
        let pending_before = note_status(&conn, "pending").unwrap().unwrap();
        let result = request_now(&conn, ImmediateDistillationScope::Unreviewed, 110).unwrap();
        assert_eq!(
            (result.registered, result.requeued, result.expedited),
            (1, 2, 2)
        );
        assert_eq!(
            (
                result.jobs.pending,
                result.jobs.running,
                result.jobs.completed
            ),
            (5, 2, 1)
        );
        assert_eq!(queue_row(&conn, &running.note), running_before);
        assert_eq!(queue_row(&conn, &expired.note), expired_before);
        assert_eq!(queue_row(&conn, "completed"), completed_before);
        let pending_after = note_status(&conn, "pending").unwrap().unwrap();
        assert_eq!(pending_after.generation, pending_before.generation);
        assert_eq!(pending_after.attempt, pending_before.attempt);
        let retry = note_status(&conn, "retry").unwrap().unwrap();
        assert_eq!(retry.generation, 1);
        assert_eq!(retry.attempt, 7);
        assert_eq!(retry.last_error, None);
        let queued_at: i64 = conn
            .query_row(
                "SELECT queued_at FROM distillation_jobs WHERE note='retry'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(queued_at, 10);
        let future = note_status(&conn, "future").unwrap().unwrap();
        assert_eq!(future.attempt, 2);
        assert_eq!(future.generation, 1);
        let rows_before_second: Vec<_> = ["missing", "retry", "blocked", "future"]
            .into_iter()
            .map(|id| queue_row(&conn, id))
            .collect();
        let repeated = request_now(&conn, ImmediateDistillationScope::Unreviewed, 111).unwrap();
        assert_eq!(
            (repeated.registered, repeated.requeued, repeated.expedited),
            (0, 0, 0)
        );
        let rows_after_second: Vec<_> = ["missing", "retry", "blocked", "future"]
            .into_iter()
            .map(|id| queue_row(&conn, id))
            .collect();
        assert_eq!(rows_before_second, rows_after_second);
        assert_eq!(documents(&conn), before);
        let tx = conn.unchecked_transaction().unwrap();
        verify_lease_in_tx(&tx, &running, 111).unwrap();
        tx.rollback().unwrap();
    }

    /// 2026-09-07: 全件の再確認は確認済みだけ新世代にし、過去の確認証跡は保持する。
    #[test]
    fn immediate_all_requeues_completed_once_and_retains_review_history() {
        let conn = setup();
        insert(
            &conn,
            "reviewed",
            &fixture_note("確認済み").to_file_string().unwrap(),
        );
        let lease = claim(&conn, 100, 60).unwrap().unwrap();
        finish(&conn, &lease, 110);
        let completed = note_status(&conn, "reviewed").unwrap().unwrap();
        let before = documents(&conn);
        let result = request_now(&conn, ImmediateDistillationScope::All, 120).unwrap();
        assert_eq!(
            (result.registered, result.requeued, result.expedited),
            (0, 1, 0)
        );
        let pending = note_status(&conn, "reviewed").unwrap().unwrap();
        assert_eq!(pending.state, "pending");
        assert_eq!(pending.generation, completed.generation + 1);
        assert_eq!(pending.reviewed_hash, completed.reviewed_hash);
        assert_eq!(pending.last_reviewed_at, completed.last_reviewed_at);
        let rows = queue_row(&conn, "reviewed");
        let repeated = request_now(&conn, ImmediateDistillationScope::All, 121).unwrap();
        assert_eq!(
            (repeated.registered, repeated.requeued, repeated.expedited),
            (0, 0, 0)
        );
        assert_eq!(queue_row(&conn, "reviewed"), rows);
        assert_eq!(documents(&conn), before);
        let next = claim(&conn, 121, 60).unwrap().unwrap();
        assert_eq!(next.input_hash, lease.input_hash);
        assert_eq!(next.generation, lease.generation + 1);
    }

    /// 2026-09-07: 古い適格フラグが残ってもhuman・提案・非参照ノートを即時実行しない。
    #[test]
    fn immediate_requests_recheck_note_eligibility_without_mutating_ineligible_jobs() {
        let conn = setup();
        let mut human = fixture_note("人間所有");
        human.front.origin = Some("human".into());
        let mut proposal = fixture_note("提案分類");
        let authority = proposal.front.authority.as_mut().unwrap();
        authority.namespace = crate::authority::NoteNamespace::Decisions;
        authority.role = crate::authority::AuthorityRole::Proposal;
        authority.status = crate::authority::AuthorityStatus::Active;
        let mut ticket = fixture_note("専用提案票");
        ticket
            .front
            .extra
            .insert("proposal_ticket".into(), serde_yaml::Value::Null);
        for (id, note) in [
            ("human", human),
            ("proposal", proposal),
            ("ticket", ticket),
            ("hidden", fixture_note("非参照")),
            ("excluded", fixture_note("対象外")),
        ] {
            insert(&conn, id, &note.to_file_string().unwrap());
        }
        conn.execute(
            "UPDATE notes SET normal_reference_allowed=0 WHERE id='hidden'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE notes SET distillation_allowed=0 WHERE id='excluded'",
            [],
        )
        .unwrap();
        for id in ["hidden", "excluded"] {
            conn.execute("INSERT INTO distillation_jobs(note,generation,state,reason,queued_at,available_at) VALUES(?1,1,'blocked','stale',0,900)", [id]).unwrap();
        }
        conn.execute(
            "UPDATE distillation_jobs SET state='blocked', last_error='retained'",
            [],
        )
        .unwrap();
        let ids = ["human", "proposal", "ticket", "hidden", "excluded"];
        let rows: Vec<_> = ids.iter().map(|id| queue_row(&conn, id)).collect();
        insert(
            &conn,
            "eligible",
            &fixture_note("適格").to_file_string().unwrap(),
        );
        conn.execute("DELETE FROM distillation_jobs WHERE note='eligible'", [])
            .unwrap();
        let before = documents(&conn);
        let result = request_now(&conn, ImmediateDistillationScope::All, 110).unwrap();
        assert_eq!(
            (result.registered, result.requeued, result.expedited),
            (1, 0, 0)
        );
        assert_eq!(
            ids.iter()
                .map(|id| queue_row(&conn, id))
                .collect::<Vec<_>>(),
            rows
        );
        assert_eq!(documents(&conn), before);
        assert_eq!(
            note_status(&conn, "eligible").unwrap().unwrap().state,
            "pending"
        );
    }

    /// 2026-09-07: 一括受付の途中で失敗しても、一部だけ再試行が始まる状態を残さない。
    #[test]
    fn immediate_request_rolls_back_the_whole_batch_on_failure() {
        let conn = setup();
        for id in ["a", "b"] {
            insert(&conn, id, &fixture_note(id).to_file_string().unwrap());
        }
        conn.execute("UPDATE distillation_jobs SET state='blocked', attempt=3, last_error='review_round_limit'", []).unwrap();
        let before = [queue_row(&conn, "a"), queue_row(&conn, "b")];
        conn.execute_batch("CREATE TRIGGER reject_second_immediate BEFORE UPDATE ON distillation_jobs WHEN OLD.note='b' BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;").unwrap();
        assert!(request_now(&conn, ImmediateDistillationScope::Unreviewed, 110).is_err());
        assert_eq!([queue_row(&conn, "a"), queue_row(&conn, "b")], before);
        assert!(request_now(&conn, ImmediateDistillationScope::All, -1).is_err());
        assert_eq!([queue_row(&conn, "a"), queue_row(&conn, "b")], before);
    }

    #[test]
    fn note_and_pending_registration_commit_or_rollback_together() {
        let conn = setup();
        let tx = conn.unchecked_transaction().unwrap();
        insert(&tx, "a", "original");
        assert_eq!(status(&tx).unwrap().pending, 1);
        tx.rollback().unwrap();
        assert_eq!(status(&conn).unwrap().pending, 0);
        insert(&conn, "a", "original");
        let lease = claim(&conn, 100, 60).unwrap().unwrap();
        finish(&conn, &lease, 110);
        let tx = conn.unchecked_transaction().unwrap();
        tx.execute("UPDATE notes SET document='new' WHERE id='a'", [])
            .unwrap();
        assert_eq!(status(&tx).unwrap().pending, 1);
        tx.rollback().unwrap();
        assert_eq!(status(&conn).unwrap().completed, 1);
    }

    #[test]
    fn unchanged_write_does_not_enqueue_and_hidden_notes_are_excluded() {
        let conn = setup();
        insert(&conn, "a", "original");
        let lease = claim(&conn, 100, 60).unwrap().unwrap();
        finish(&conn, &lease, 110);
        conn.execute("UPDATE notes SET document=document WHERE id='a'", [])
            .unwrap();
        conn.execute("INSERT INTO notes(id,document,normal_reference_allowed) VALUES('proposal', 'ticket', 0)", [])
            .unwrap();
        assert_eq!(status(&conn).unwrap().completed, 1);
        assert!(claim(&conn, 120, 60).unwrap().is_none());
        conn.execute(
            "UPDATE notes SET normal_reference_allowed=1 WHERE id='proposal'",
            [],
        )
        .unwrap();
        assert_eq!(status(&conn).unwrap().pending, 1);
        conn.execute(
            "UPDATE notes SET normal_reference_allowed=0 WHERE id='proposal'",
            [],
        )
        .unwrap();
        assert_eq!(status(&conn).unwrap().pending, 0);
    }

    #[test]
    fn changed_during_work_rejects_stale_completion_and_failure() {
        let conn = setup();
        insert(&conn, "a", "original");
        let old = claim(&conn, 100, 60).unwrap().unwrap();
        conn.execute("UPDATE notes SET document='new' WHERE id='a'", [])
            .unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        assert!(verify_lease_in_tx(&tx, &old, 110).is_err());
        tx.rollback().unwrap();
        assert!(!fail(&conn, &old, 110, "stale worker", false).unwrap());
        let new = claim(&conn, 110, 60).unwrap().unwrap();
        assert_eq!(new.generation, old.generation + 1);
        assert_ne!(new.input_hash, old.input_hash);
        finish(&conn, &new, 115);
    }

    #[test]
    fn crash_expiry_and_backoff_preserve_unfinished_work() {
        let conn = setup();
        insert(&conn, "a", "original");
        let expired = claim(&conn, 100, 60).unwrap().unwrap();
        assert!(claim(&conn, 159, 60).unwrap().is_none());
        let recovered = claim(&conn, 160, 60).unwrap().unwrap();
        assert_eq!(recovered.generation, expired.generation);
        assert_ne!(recovered.token, expired.token);
        let tx = conn.unchecked_transaction().unwrap();
        assert!(verify_lease_in_tx(&tx, &expired, 161).is_err());
        tx.rollback().unwrap();
        assert!(fail(&conn, &recovered, 161, "temporary failure", false).unwrap());
        assert!(claim(&conn, 220, 60).unwrap().is_none());
        let retry = claim(&conn, 221, 60).unwrap().unwrap();
        assert_eq!(retry.attempt, 3);
        assert!(fail(&conn, &retry, 222, "needs authentication", true).unwrap());
        assert!(claim(&conn, 9_000, 60).unwrap().is_none());
        assert_eq!(retry_failed(&conn, 9_000).unwrap(), 1);
        assert!(claim(&conn, 9_000, 60).unwrap().is_some());
    }

    #[test]
    fn own_outputs_finish_atomically_without_infinite_reenqueue() {
        let conn = setup();
        insert(&conn, "a", "original");
        let lease = claim(&conn, 100, 60).unwrap().unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        verify_lease_in_tx(&tx, &lease, 110).unwrap();
        tx.execute("UPDATE notes SET document='distilled' WHERE id='a'", [])
            .unwrap();
        insert(&tx, "canonical", "synthesis");
        settle_outputs_in_tx(
            &tx,
            &[
                ("a".into(), crate::distillation::sha256(b"distilled")),
                (
                    "canonical".into(),
                    crate::distillation::sha256(b"synthesis"),
                ),
            ],
            110,
            "根拠を正本へ反映",
        )
        .unwrap();
        tx.commit().unwrap();
        assert_eq!(status(&conn).unwrap().completed, 2);
        assert!(claim(&conn, 120, 60).unwrap().is_none());
        assert!(!fail(&conn, &lease, 120, "late failure", false).unwrap());
    }

    #[test]
    fn periodic_review_waits_for_due_date_and_enqueues_once() {
        let conn = setup();
        insert(&conn, "a", "original");
        let lease = claim(&conn, 100, 60).unwrap().unwrap();
        finish(&conn, &lease, 110);
        assert_eq!(enqueue_due_reviews(&conn, 209, 100).unwrap(), 0);
        assert_eq!(enqueue_due_reviews(&conn, 210, 100).unwrap(), 1);
        assert_eq!(enqueue_due_reviews(&conn, 211, 100).unwrap(), 0);
        let next = claim(&conn, 211, 60).unwrap().unwrap();
        assert_eq!(next.input_hash, lease.input_hash);
        assert_eq!(next.generation, lease.generation + 1);
        finish(&conn, &next, 220);
        assert_eq!(enqueue_due_reviews(&conn, 319, 100).unwrap(), 0);
    }

    #[test]
    fn failed_registration_rejects_the_note_write() {
        let conn = setup();
        conn.execute_batch(
            "CREATE TRIGGER reject_job BEFORE INSERT ON distillation_jobs
             BEGIN SELECT RAISE(ABORT, 'job registration failed'); END;",
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO notes(id,document,normal_reference_allowed) VALUES('a','original',1)",
                [],
            )
            .is_err()
        );
        let count: i64 = conn
            .query_row("SELECT count(*) FROM notes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn fresh_writes_precede_bootstrap_until_old_work_needs_attention() {
        let conn = setup();
        insert(&conn, "bootstrap", "old");
        insert(&conn, "fresh", "new");
        conn.execute("UPDATE distillation_jobs SET reason='unreviewed', queued_at=100 WHERE note='bootstrap'", []).unwrap();
        conn.execute(
            "UPDATE distillation_jobs SET queued_at=200 WHERE note='fresh'",
            [],
        )
        .unwrap();
        let lease = claim(&conn, 300, 60).unwrap().unwrap();
        assert_eq!(lease.note, "fresh");
        finish(&conn, &lease, 310);
        conn.execute("UPDATE notes SET document='newer' WHERE id='fresh'", [])
            .unwrap();
        conn.execute(
            "UPDATE distillation_jobs SET queued_at=86401 WHERE note='fresh'",
            [],
        )
        .unwrap();
        let aged = claim(&conn, 86500, 60).unwrap().unwrap();
        assert_eq!(aged.note, "bootstrap");
    }

    #[test]
    fn no_op_replacement_of_mandatory_trigger_is_visible() {
        let conn = setup();
        conn.execute_batch(
            "DROP TRIGGER distillation_jobs_insert;
             CREATE TRIGGER distillation_jobs_insert AFTER INSERT ON notes
             BEGIN SELECT 1; END;",
        )
        .unwrap();
        assert!(verify_schema(&conn).is_err());
    }

    #[test]
    fn note_status_preserves_receipt_but_marks_new_version_pending() {
        let conn = setup();
        assert!(note_status(&conn, "missing").unwrap().is_none());
        insert(&conn, "a", "original");
        let pending = note_status(&conn, "a").unwrap().unwrap();
        assert_eq!(pending.state, "pending");
        assert!(pending.reviewed_hash.is_none());
        let lease = claim(&conn, 100, 60).unwrap().unwrap();
        finish(&conn, &lease, 110);
        let reviewed = note_status(&conn, "a").unwrap().unwrap();
        assert_eq!(reviewed.state, "completed");
        assert_eq!(
            reviewed.reviewed_hash.as_deref(),
            Some(lease.input_hash.as_str())
        );
        conn.execute("UPDATE notes SET document='new' WHERE id='a'", [])
            .unwrap();
        let changed = note_status(&conn, "a").unwrap().unwrap();
        assert_eq!(changed.state, "pending");
        assert_eq!(changed.generation, reviewed.generation + 1);
        assert_eq!(changed.reviewed_hash, reviewed.reviewed_hash);
    }

    #[test]
    fn issues_prioritize_blocked_then_age_without_returning_note_bodies() {
        let conn = setup();
        for note in ["blocked-new", "blocked-old", "retry", "pending"] {
            insert(&conn, note, "PRIVATE_BODY_SENTINEL");
        }
        conn.execute(
            "UPDATE distillation_jobs SET state='blocked', queued_at=200,
                 last_error='authentication required', available_at=230, attempt=2
             WHERE note='blocked-new'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE distillation_jobs SET state='blocked', queued_at=100 WHERE note='blocked-old'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE distillation_jobs SET state='retry_wait', queued_at=0 WHERE note='retry'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE notes SET title='対象ノート' WHERE id='blocked-new'",
            [],
        )
        .unwrap();
        conn.execute_batch("PRAGMA query_only=ON").unwrap();
        let found = issues(&conn, 20).unwrap();
        assert_eq!(
            found
                .iter()
                .map(|issue| issue.note.as_str())
                .collect::<Vec<_>>(),
            ["blocked-old", "blocked-new", "retry"]
        );
        assert_eq!(found[1].title.as_deref(), Some("対象ノート"));
        assert_eq!(
            found[1].last_error.as_deref(),
            Some("authentication required")
        );
        assert_eq!(found[1].available_at, 230);
        assert_eq!(found[1].attempt, 2);
        assert!(
            !serde_json::to_string(&found)
                .unwrap()
                .contains("PRIVATE_BODY_SENTINEL")
        );
        assert_eq!(issues(&conn, 1).unwrap().len(), 1);
        assert!(issues(&conn, 0).unwrap().is_empty());
    }

    #[test]
    fn issues_cap_results_and_exclude_stale_ineligible_rows() {
        let conn = setup();
        for index in 0..25 {
            insert(&conn, &format!("note-{index:02}"), "body");
        }
        conn.execute(
            "UPDATE distillation_jobs SET state='blocked', queued_at=100",
            [],
        )
        .unwrap();
        insert(&conn, "hidden", "private ticket");
        conn.execute(
            "UPDATE notes SET distillation_allowed=0 WHERE id='hidden'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO distillation_jobs(note,generation,state,reason,queued_at,available_at)
             VALUES('hidden',1,'blocked','stale',0,0),('removed',1,'blocked','stale',0,0)",
            [],
        )
        .unwrap();
        let found = issues(&conn, usize::MAX).unwrap();
        assert_eq!(found.len(), 20);
        assert_eq!(found[0].note, "note-00");
        assert_eq!(found[19].note, "note-19");
    }

    #[test]
    fn mandatory_trigger_loss_is_visible() {
        let conn = setup();
        verify_schema(&conn).unwrap();
        conn.execute_batch("DROP TRIGGER distillation_jobs_insert")
            .unwrap();
        assert!(verify_schema(&conn).is_err());
    }
}
