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

/// 埋め込みの複合バージョンスタンプ(書式|provider:model:dim|チャンク規則)。
/// モデル・規則を変えたら必ず上げる — 旧 KB の「旧ベクトル混在を検知できない」教訓。
pub const EMBED_STAMP: &str = "v1|ort:bge-m3-int8:1024|whole1500";
const MAX_EMBED_CHARS: usize = 1500;
/// 前出し・意味ヒットの関連閾値(コサイン距離)。旧較正 0.95 をパリティ実測で移植。
pub const RELATED_DISTANCE: f32 = 0.95;

const MODEL_URL: &str = "https://huggingface.co/Xenova/bge-m3/resolve/main/onnx/model_int8.onnx";
const TOKENIZER_URL: &str = "https://huggingface.co/Xenova/bge-m3/resolve/main/tokenizer.json";

pub fn model_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("kb-app")
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
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// 未埋め込み・旧スタンプのノートを埋め込む(最大 `cap` 件)。残件数を返す。
/// cap を超えた分は次回に回る — 残があることは呼び側が劣化情報として見せる。
pub fn embed_pending(conn: &Connection, cap: usize) -> Result<usize> {
    let ids: Vec<(String, String)> = {
        let mut stmt = conn.prepare_cached(
            "SELECT n.id, coalesce(n.title,'') || ' ' || coalesce(n.description,'') || ' ' || n.body
             FROM notes n LEFT JOIN note_vecs v ON v.id = n.id AND v.stamp = ?1
             WHERE v.id IS NULL",
        )?;
        let rows = stmt.query_map([EMBED_STAMP], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let total = ids.len();
    for (id, text) in ids.iter().take(cap) {
        let v = embed_text(text)?;
        conn.execute(
            "INSERT OR REPLACE INTO note_vecs(id, stamp, embedding) VALUES(?1, ?2, ?3)",
            rusqlite::params![id, EMBED_STAMP, to_blob(&v)],
        )?;
    }
    Ok(total.saturating_sub(cap.min(total)))
}

/// クエリベクトルとの総当たり KNN。(id, コサイン距離) を近い順に返す。
pub fn knn(conn: &Connection, query: &[f32], k: usize) -> Result<Vec<(String, f32)>> {
    let mut stmt = conn.prepare_cached("SELECT id, embedding FROM note_vecs WHERE stamp = ?1")?;
    let rows = stmt.query_map([EMBED_STAMP], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
    })?;
    let mut scored: Vec<(String, f32)> = Vec::new();
    for row in rows {
        let (id, blob) = row?;
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
}
