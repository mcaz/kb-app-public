//! 段1: かしこい検索(埋め込み内蔵)。ort + bge-m3 int8 をアプリ内で実行する。
//! 外部プロセス(ollama)非依存 — 旧 KB の「4日間沈黙停止」障害クラスを構造的に消す。
//! 較正資産(距離閾値)はパリティ実測(docs/poc-report.md)により移植。
//!
//! ベクトル索引は BLOB 列+Rust 側総当たり(個人規模 10^3〜10^4 では sqlite-vec と
//! 同性能クラス・可動部が少ない)。規模が要件を超えたら sqlite-vec へ(PoC 実証済み)。

use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, anyhow, bail};
use ndarray::Ix3;
use ort::{
    session::{Session, builder::GraphOptimizationLevel},
    value::TensorRef,
};
use rusqlite::Connection;
use tokenizers::Tokenizer;

/// 埋め込みのproducerスタンプ(書式|provider:model:dim|チャンク規則)。
/// モデル・規則を変えたら必ず上げる — 旧 KB の「旧ベクトル混在を検知できない」教訓。
/// 保存行の `note_vecs.stamp` はこれ単体ではなく、入力hashを加えた複合形
/// (`embedding_stamp`)を使う — title/description変更のstale検知(R1 A-5)。
pub const EMBED_PRODUCER_STAMP: &str = "v1|ort:bge-m3-int8:1024|whole1500";
const MAX_EMBED_CHARS: usize = 1500;

/// 埋め込み入力の唯一の組み立て。hash・埋め込み生成・invalidationの全てが
/// この文字列を共用する(別実装の分岐が境界衝突バグの温床 — spec S-4)。
/// 単純連結の境界衝突(`"a b"+""` と `"a"+"b"`)は入力文字列自体の同一性なので、
/// 同一入力 → 同一埋め込み → 同一stampとなり意味的に無害。
pub fn embedding_input(title: Option<&str>, description: Option<&str>, body: &str) -> String {
    format!(
        "{} {} {}",
        title.unwrap_or(""),
        description.unwrap_or(""),
        body
    )
}

/// 保存stamp = `{producer}|sha256:{埋め込み入力のhash}` の複合形。
/// 入力が1文字でも変われば別stampになり、既存行は自動的にpending扱いへ落ちる。
pub fn embedding_stamp(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{EMBED_PRODUCER_STAMP}|sha256:{:x}", hasher.finalize())
}

/// 現行producerの複合形stampか(prefix一致)。旧単体形式・他producerの行は
/// 検索(knn等)の対象外 = 全てpending。旧バイナリとの併存は二段階rollout制約:
/// 旧バイナリはtitle/description変更で複合stamp行を無効化しないため、
/// 新旧バイナリが同じDBへ交互に書く期間はstale埋め込みが残り得る。
pub fn is_current_stamp(stamp: &str) -> bool {
    stamp
        .strip_prefix(EMBED_PRODUCER_STAMP)
        .is_some_and(|rest| rest.starts_with("|sha256:"))
}
/// 前出し・意味ヒットの関連閾値(コサイン距離)。旧較正 0.95 をパリティ実測で移植。
pub const RELATED_DISTANCE: f32 = 0.95;

const MODEL_URL: &str = "https://huggingface.co/Xenova/bge-m3/resolve/main/onnx/model_int8.onnx";
const TOKENIZER_URL: &str = "https://huggingface.co/Xenova/bge-m3/resolve/main/tokenizer.json";

pub fn model_dir() -> PathBuf {
    crate::app_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("models")
        .join("bge-m3")
}

pub fn model_installed() -> bool {
    let d = model_dir();
    d.join("model_int8.onnx").exists() && d.join("tokenizer.json").exists()
}

pub fn downloading() -> bool {
    model_dir().join("model_int8.onnx.part").exists()
}

/// モデルの導入(curl でダウンロード → 完了後 rename)。既に導入済みなら no-op。
pub fn install_model() -> Result<()> {
    if model_installed() {
        return Ok(());
    }
    let dir = model_dir();
    fs::create_dir_all(&dir)?;
    for (url, name) in [
        (TOKENIZER_URL, "tokenizer.json"),
        (MODEL_URL, "model_int8.onnx"),
    ] {
        let dest = dir.join(name);
        if dest.exists() {
            continue;
        }
        let part = dir.join(format!("{name}.part"));
        let status = std::process::Command::new("curl")
            .args(["-L", "--fail", "-o"])
            .arg(&part)
            .arg(url)
            .status()
            .context("curl 実行")?;
        if !status.success() {
            let _ = fs::remove_file(&part);
            bail!("{name} のダウンロードに失敗");
        }
        fs::rename(&part, &dest)?;
    }
    Ok(())
}

// ---------------------------------------------------------------- embedder

/// ort rc.13 の Error<R> は Send/Sync でないため Display 経由で変換(PoC の知見)。
macro_rules! ort_err {
    ($e:expr, $what:expr) => {
        $e.map_err(|e| anyhow!("{}: {e}", $what))
    };
}

pub struct Embedder {
    session: Session,
    tokenizer: Tokenizer,
    needs_token_type_ids: bool,
    hidden_output_name: String,
}

impl Embedder {
    fn load() -> Result<Self> {
        let dir = model_dir();
        let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|e| anyhow!("tokenizer load: {e}"))?;
        let session = ort_err!(
            ort_err!(
                ort_err!(Session::builder(), "session builder")?
                    .with_optimization_level(GraphOptimizationLevel::Level3),
                "opt level"
            )?
            .commit_from_file(dir.join("model_int8.onnx")),
            "model load"
        )?;
        let input_names: Vec<String> = session
            .inputs()
            .iter()
            .map(|i| i.name().to_string())
            .collect();
        let output_names: Vec<String> = session
            .outputs()
            .iter()
            .map(|o| o.name().to_string())
            .collect();
        let needs_token_type_ids = input_names.iter().any(|n| n == "token_type_ids");
        let hidden_output_name = output_names
            .iter()
            .find(|n| n.contains("last_hidden_state"))
            .cloned()
            .unwrap_or_else(|| output_names[0].clone());
        Ok(Self {
            session,
            tokenizer,
            needs_token_type_ids,
            hidden_output_name,
        })
    }

    /// 単文埋め込み。L2 正規化済み 1024 次元(dense = 最終層 [CLS])。
    pub fn embed(&mut self, text: &str) -> Result<Vec<f32>> {
        let text: String = text.chars().take(MAX_EMBED_CHARS).collect();
        let enc = self
            .tokenizer
            .encode(text.as_str(), true)
            .map_err(|e| anyhow!("encode: {e}"))?;
        let ids: Vec<i64> = enc.get_ids().iter().map(|&i| i as i64).collect();
        let mask: Vec<i64> = enc.get_attention_mask().iter().map(|&i| i as i64).collect();
        let len = ids.len();
        let zeros: Vec<i64> = vec![0; len];
        let t_ids = ort_err!(
            TensorRef::from_array_view(([1usize, len], &*ids)),
            "ids tensor"
        )?;
        let t_mask = ort_err!(
            TensorRef::from_array_view(([1usize, len], &*mask)),
            "mask tensor"
        )?;
        let outputs = if self.needs_token_type_ids {
            let t_tt = ort_err!(
                TensorRef::from_array_view(([1usize, len], &*zeros)),
                "tt tensor"
            )?;
            ort_err!(
                self.session.run(ort::inputs![
                    "input_ids" => t_ids,
                    "attention_mask" => t_mask,
                    "token_type_ids" => t_tt,
                ]),
                "run"
            )?
        } else {
            ort_err!(
                self.session.run(ort::inputs![
                    "input_ids" => t_ids,
                    "attention_mask" => t_mask,
                ]),
                "run"
            )?
        };
        let arr = ort_err!(
            outputs[self.hidden_output_name.as_str()].try_extract_array::<f32>(),
            "extract"
        )?
        .into_dimensionality::<Ix3>()
        .context("expected [batch, seq, hidden]")?;
        let mut v: Vec<f32> = arr
            .index_axis(ndarray::Axis(0), 0)
            .index_axis(ndarray::Axis(0), 0)
            .iter()
            .copied()
            .collect();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in v.iter_mut() {
                *x /= norm;
            }
        }
        Ok(v)
    }
}

/// アイドル時アンロード(常駐メモリ対策 — ウォーム時 ~1.8GB を解放する)。
/// 最終使用から IDLE_UNLOAD_SECS 経過で番人スレッドが解放し、次回使用時に再ロード
/// (~1秒)。長寿命プロセス(Desktop の MCP・GUI)で効く。CLI は都度プロセスで無関係。
const IDLE_UNLOAD_SECS: u64 = 300;

struct Loaded {
    emb: Embedder,
    last_used: std::time::Instant,
}

static STATE: OnceLock<Mutex<Option<Loaded>>> = OnceLock::new();
static JANITOR: OnceLock<()> = OnceLock::new();

fn state() -> &'static Mutex<Option<Loaded>> {
    STATE.get_or_init(|| Mutex::new(None))
}

fn spawn_janitor() {
    JANITOR.get_or_init(|| {
        std::thread::spawn(|| {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
                if let Ok(mut g) = state().lock()
                    && g.as_ref()
                        .is_some_and(|l| l.last_used.elapsed().as_secs() > IDLE_UNLOAD_SECS)
                {
                    *g = None; // モデル解放(次回使用時に再ロード)
                }
            }
        });
    });
}

pub fn embed_text(text: &str) -> Result<Vec<f32>> {
    if !model_installed() {
        bail!("埋め込みモデル未導入");
    }
    let mut g = state().lock().map_err(|_| anyhow!("embedder lock"))?;
    if g.is_none() {
        *g = Some(Loaded {
            emb: Embedder::load()?,
            last_used: std::time::Instant::now(),
        });
        spawn_janitor();
    }
    let l = g.as_mut().expect("loaded above");
    l.last_used = std::time::Instant::now();
    l.emb.embed(text)
}

// ---------------------------------------------------------------- storage

pub fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

pub fn from_blob(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| f32::from_le_bytes(*bytes))
        .collect()
}

/// 未埋め込み・旧スタンプ・内容とstampが食い違うノートを埋め込む(最大 `cap` 件)。
/// 残件数を返す。cap を超えた分は次回に回る — 残は呼び側が劣化情報として見せる。
/// 期待stampはノートごとの複合形なのでSQL定数比較では判定できず、Rust側で
/// `embedding_input` → `embedding_stamp` を再計算して照合する(単一実装の共用)。
pub fn embed_pending(conn: &Connection, cap: usize) -> Result<usize> {
    let pending: Vec<(String, String, String)> = {
        let mut stmt = conn.prepare_cached(
            "SELECT n.id, n.title, n.description, n.body, v.stamp
             FROM notes n LEFT JOIN note_vecs v ON v.id = n.id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?;
        let mut pending = Vec::new();
        for row in rows {
            let (id, title, description, body, stamp) = row?;
            let input = embedding_input(title.as_deref(), description.as_deref(), &body);
            let expected = embedding_stamp(&input);
            if stamp.as_deref() != Some(expected.as_str()) {
                pending.push((id, input, expected));
            }
        }
        pending
    };
    let total = pending.len();
    for (id, input, stamp) in pending.iter().take(cap) {
        let v = embed_text(input)?;
        conn.execute(
            "INSERT OR REPLACE INTO note_vecs(id, stamp, embedding) VALUES(?1, ?2, ?3)",
            rusqlite::params![id, stamp, to_blob(&v)],
        )?;
    }
    Ok(total.saturating_sub(cap.min(total)))
}

/// クエリベクトルとの総当たり KNN。(id, コサイン距離) を近い順に返す。
/// 現行producerの複合形stamp(`is_current_stamp`)の行だけを対象にする —
/// 旧形式stampの行は再埋め込みが追い付くまで意味検索に混ぜない。
pub fn knn(conn: &Connection, query: &[f32], k: usize) -> Result<Vec<(String, f32)>> {
    let mut stmt =
        conn.prepare_cached("SELECT id, stamp, embedding FROM note_vecs WHERE stamp IS NOT NULL")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Vec<u8>>(2)?,
        ))
    })?;
    let mut scored: Vec<(String, f32)> = Vec::new();
    for row in rows {
        let (id, stamp, blob) = row?;
        if !is_current_stamp(&stamp) {
            continue;
        }
        let v = from_blob(&blob);
        if v.len() != query.len() {
            continue;
        }
        let sim: f32 = v.iter().zip(query.iter()).map(|(a, b)| a * b).sum();
        scored.push((id, 1.0 - sim));
    }
    scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(k);
    Ok(scored)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_roundtrip() {
        let v = vec![0.25f32, -1.5, 3.0];
        assert_eq!(from_blob(&to_blob(&v)), v);
    }

    /// stampは「producer|sha256:入力hash」の複合形。producer単体(旧形式)は
    /// 現行と判定されず、入力のどんな変化も別stampになる。
    #[test]
    fn composite_stamp_is_producer_prefixed_and_input_sensitive() {
        let base = embedding_stamp(&embedding_input(Some("題"), Some("説明"), "本文"));
        assert!(is_current_stamp(&base), "{base}");
        assert!(base.starts_with(EMBED_PRODUCER_STAMP));
        assert!(
            !is_current_stamp(EMBED_PRODUCER_STAMP),
            "旧形式を現行扱いしない"
        );
        assert!(!is_current_stamp("v0|other|sha256:abc"));

        for changed in [
            embedding_stamp(&embedding_input(Some("別題"), Some("説明"), "本文")),
            embedding_stamp(&embedding_input(Some("題"), Some("別説明"), "本文")),
            embedding_stamp(&embedding_input(Some("題"), Some("説明"), "別本文")),
        ] {
            assert_ne!(base, changed);
            assert!(is_current_stamp(&changed));
        }
        // 決定的(同一入力 → 同一stamp)
        assert_eq!(
            base,
            embedding_stamp(&embedding_input(Some("題"), Some("説明"), "本文"))
        );
    }
}
