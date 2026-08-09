//! kb-app PoC: ONNX Runtime (ort 2.0.0-rc.13) + bge-m3 int8 量子化モデルの検証。
//!
//! 検証項目:
//!   1. 品質: 日本語中心+日英クロスのテスト文ペアでコサイン距離の分離を確認
//!   2. パリティ: ローカル ollama (bge-m3 F16) と同一テキストの埋め込みを比較
//!   3. 性能: ロード時間 / 短文・長文レイテンシ(ウォームアップ後の中央値)

use std::time::Instant;

use anyhow::{anyhow, Context, Result};

/// ort 2.0.0-rc.13 の `Error<R>` は R を抱えるため Send/Sync でなく、
/// anyhow へ `?` 変換できない。Display 経由で変換するヘルパ。
trait OrtCtx<T> {
    fn ort_err(self, what: &str) -> Result<T>;
}
impl<T, R> OrtCtx<T> for std::result::Result<T, ort::Error<R>> {
    fn ort_err(self, what: &str) -> Result<T> {
        self.map_err(|e| anyhow!("{what}: {e}"))
    }
}
use ndarray::Ix3;
use ort::{
    session::{builder::GraphOptimizationLevel, Session},
    value::TensorRef,
};
use tokenizers::Tokenizer;

const MODEL_PATH: &str = "models/model_int8.onnx";
const TOKENIZER_PATH: &str = "models/tokenizer.json";
const OLLAMA_BASE: &str = "http://localhost:11434";

// ---------------------------------------------------------------- test data

const RELATED_PAIRS: &[(&str, &str)] = &[
    (
        "ベクトル検索が停止していた障害の記録",
        "埋め込みが4日間更新されず検索が劣化した",
    ),
    (
        "ONNX Runtime で量子化した埋め込みモデルをアプリに同梱する",
        "int8 量子化済みのモデルファイルをアプリ内蔵で実行する方式",
    ),
    (
        "ナレッジベースのノートをコサイン距離で検索する仕組み",
        "意味ベクトルの近さで関連ノートを探す検索機構",
    ),
    (
        "毎朝コーヒーを淹れてから仕事を始める",
        "I brew a cup of coffee every morning before starting work.",
    ),
    (
        "請求書の支払い期限は今月末です",
        "The invoice payment is due at the end of this month.",
    ),
];

const UNRELATED_PAIRS: &[(&str, &str)] = &[
    (
        "ベクトル検索が停止していた障害の記録",
        "週末に友人と登山へ行く計画を立てている",
    ),
    (
        "int8 量子化モデルの推論速度を計測する",
        "猫がこたつで丸くなって眠っている",
    ),
    (
        "データベースのマイグレーション手順書",
        "桜の開花予想は3月下旬ごろになる見込みです",
    ),
    (
        "埋め込みモデルの距離較正資産を移植する",
        "The recipe calls for two cups of flour and three eggs.",
    ),
    (
        "検索インデックスの再構築ジョブが失敗した",
        "He plays tennis with his friends every Sunday afternoon.",
    ),
];

const SHORT_TEXT: &str = "ベクトル検索の較正資産を int8 量子化モデルへ移植できるか検証する。"; // ~33 chars
const LONG_TEXT: &str = "ローカルナレッジベース製品の技術検証として、ONNX Runtime と int8 量子化済みの bge-m3 モデルをアプリケーションへ同梱し、外部プロセスに依存せず埋め込みを生成できるかを確認する。既存実装は ollama 経由で bge-m3 の fp16 モデルを呼び出しており、コサイン距離の較正資産として関連ノートは 0.72 から 0.88、無関係ノートは 1.08 から 1.14 の帯域が実測されている。前出しフックは距離 0.95 未満、終了時の安全網は 0.85 未満を閾値として運用しているため、モデルを据え置いたまま実行基盤だけを差し替えた場合に、この較正がそのまま移植できるかどうかが判定の核心になる。加えて、モデルファイルのサイズが数百メガバイト以内に収まること、短文の埋め込みレイテンシが実用範囲であること、日本語と英語をまたいだ意味検索の品質が維持されることも合わせて確認する。"; // ~360 chars

// ---------------------------------------------------------------- helpers

fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// 両ベクトルは正規化済み前提。コサイン類似度 = dot。
fn cos_sim(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

fn cos_dist(a: &[f32], b: &[f32]) -> f32 {
    1.0 - cos_sim(a, b)
}

fn median(mut xs: Vec<f64>) -> f64 {
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = xs.len();
    if n % 2 == 1 { xs[n / 2] } else { (xs[n / 2 - 1] + xs[n / 2]) / 2.0 }
}

// ---------------------------------------------------------------- embedder

struct Embedder {
    session: Session,
    tokenizer: Tokenizer,
    needs_token_type_ids: bool,
    /// Some(name) ならモデル出力に sentence_embedding 系があり直接使う。
    /// None なら last_hidden_state の [CLS] を自前でプーリング。
    sentence_embedding_output: Option<String>,
    hidden_output_name: String,
}

impl Embedder {
    fn load() -> Result<(Self, f64)> {
        let tokenizer = Tokenizer::from_file(TOKENIZER_PATH)
            .map_err(|e| anyhow!("tokenizer load: {e}"))?;

        let t0 = Instant::now();
        let session = Session::builder()
            .ort_err("session builder")?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .ort_err("opt level")?
            .commit_from_file(MODEL_PATH)
            .ort_err("session load")?;
        let load_secs = t0.elapsed().as_secs_f64();

        let input_names: Vec<String> =
            session.inputs().iter().map(|i| i.name().to_string()).collect();
        let output_names: Vec<String> =
            session.outputs().iter().map(|o| o.name().to_string()).collect();
        eprintln!("model inputs:  {:?}", input_names);
        eprintln!("model outputs: {:?}", output_names);

        let needs_token_type_ids = input_names.iter().any(|n| n == "token_type_ids");
        let sentence_embedding_output = output_names
            .iter()
            .find(|n| n.contains("sentence_embedding"))
            .cloned();
        let hidden_output_name = output_names
            .iter()
            .find(|n| n.contains("last_hidden_state"))
            .cloned()
            .unwrap_or_else(|| output_names[0].clone());

        Ok((
            Self { session, tokenizer, needs_token_type_ids, sentence_embedding_output, hidden_output_name },
            load_secs,
        ))
    }

    fn token_count(&self, text: &str) -> Result<usize> {
        let enc = self.tokenizer.encode(text, true).map_err(|e| anyhow!("encode: {e}"))?;
        Ok(enc.len())
    }

    /// 単文埋め込み(batch=1)。L2 正規化済み 1024 次元を返す。
    fn embed(&mut self, text: &str) -> Result<Vec<f32>> {
        let enc = self.tokenizer.encode(text, true).map_err(|e| anyhow!("encode: {e}"))?;
        let ids: Vec<i64> = enc.get_ids().iter().map(|&i| i as i64).collect();
        let mask: Vec<i64> = enc.get_attention_mask().iter().map(|&i| i as i64).collect();
        let len = ids.len();
        let zeros: Vec<i64> = vec![0; len];

        let t_ids = TensorRef::from_array_view(([1usize, len], &*ids)).ort_err("ids tensor")?;
        let t_mask = TensorRef::from_array_view(([1usize, len], &*mask)).ort_err("mask tensor")?;

        let outputs = if self.needs_token_type_ids {
            let t_tt = TensorRef::from_array_view(([1usize, len], &*zeros)).ort_err("tt tensor")?;
            self.session
                .run(ort::inputs![
                    "input_ids" => t_ids,
                    "attention_mask" => t_mask,
                    "token_type_ids" => t_tt,
                ])
                .ort_err("run")?
        } else {
            self.session
                .run(ort::inputs![
                    "input_ids" => t_ids,
                    "attention_mask" => t_mask,
                ])
                .ort_err("run")?
        };

        let mut v: Vec<f32> = if let Some(name) = &self.sentence_embedding_output {
            // モデルがプーリング済み埋め込みを直接出力するケース
            let arr = outputs[name.as_str()].try_extract_array::<f32>().ort_err("extract")?;
            arr.iter().copied().collect()
        } else {
            // dense 埋め込み = 最終層 [CLS](先頭トークン)の hidden state
            let arr = outputs[self.hidden_output_name.as_str()]
                .try_extract_array::<f32>()
                .ort_err("extract")?
                .into_dimensionality::<Ix3>()
                .context("expected [batch, seq, hidden]")?;
            arr.index_axis(ndarray::Axis(0), 0)
                .index_axis(ndarray::Axis(0), 0)
                .iter()
                .copied()
                .collect()
        };
        l2_normalize(&mut v);
        Ok(v)
    }
}

// ---------------------------------------------------------------- ollama

/// ollama /api/embed(新)→ /api/embeddings(旧)の順で試す。L2 正規化して返す。
fn ollama_embed(text: &str) -> Result<Vec<f32>> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(60))
        .build();

    let r = agent
        .post(&format!("{OLLAMA_BASE}/api/embed"))
        .send_json(serde_json::json!({ "model": "bge-m3", "input": text }));
    if let Ok(resp) = r {
        let val: serde_json::Value = resp.into_json()?;
        if let Some(embs) = val.get("embeddings").and_then(|e| e.as_array()) {
            if let Some(first) = embs.first().and_then(|e| e.as_array()) {
                let mut v: Vec<f32> = first
                    .iter()
                    .map(|x| x.as_f64().unwrap_or(0.0) as f32)
                    .collect();
                l2_normalize(&mut v);
                return Ok(v);
            }
        }
    }

    let val: serde_json::Value = agent
        .post(&format!("{OLLAMA_BASE}/api/embeddings"))
        .send_json(serde_json::json!({ "model": "bge-m3", "prompt": text }))?
        .into_json()?;
    let emb = val
        .get("embedding")
        .and_then(|e| e.as_array())
        .ok_or_else(|| anyhow!("no embedding in ollama response"))?;
    let mut v: Vec<f32> = emb.iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect();
    l2_normalize(&mut v);
    Ok(v)
}

fn ollama_available() -> bool {
    ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .get(&format!("{OLLAMA_BASE}/api/tags"))
        .call()
        .map(|r| {
            r.into_string()
                .map(|s| s.contains("bge-m3"))
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------- main

fn main() -> Result<()> {
    println!("== kb-app PoC: ort {} + bge-m3 int8 ==", "2.0.0-rc.13");

    let (mut embedder, load_secs) = Embedder::load()?;
    println!("\n[load] model load time: {:.2}s", load_secs);
    println!(
        "[load] pooling mode: {}",
        match &embedder.sentence_embedding_output {
            Some(n) => format!("model output '{n}' (direct)"),
            None => format!("CLS pooling on '{}' + L2 norm", embedder.hidden_output_name),
        }
    );

    // ---- warmup(初回推論はグラフ初期化を含むため分けて記録)
    let t0 = Instant::now();
    let dim = embedder.embed("ウォームアップ")?.len();
    println!("[load] first inference (incl. warmup): {:.0}ms, dim={}", t0.elapsed().as_secs_f64() * 1000.0, dim);
    for _ in 0..2 {
        embedder.embed(SHORT_TEXT)?;
    }

    // ---- 品質: 距離表
    println!("\n== quality: cosine distance (1 - cos_sim), ort int8 ==");
    let mut related_d = Vec::new();
    let mut unrelated_d = Vec::new();
    println!("--- related pairs ---");
    for (a, b) in RELATED_PAIRS {
        let va = embedder.embed(a)?;
        let vb = embedder.embed(b)?;
        let d = cos_dist(&va, &vb);
        related_d.push(d as f64);
        println!("  {:.4}  {} <-> {}", d, a, b);
    }
    println!("--- unrelated pairs ---");
    for (a, b) in UNRELATED_PAIRS {
        let va = embedder.embed(a)?;
        let vb = embedder.embed(b)?;
        let d = cos_dist(&va, &vb);
        unrelated_d.push(d as f64);
        println!("  {:.4}  {} <-> {}", d, a, b);
    }
    let rel_max = related_d.iter().cloned().fold(f64::MIN, f64::max);
    let unrel_min = unrelated_d.iter().cloned().fold(f64::MAX, f64::min);
    println!(
        "[quality] related: min {:.4} / max {:.4}   unrelated: min {:.4} / max {:.4}",
        related_d.iter().cloned().fold(f64::MAX, f64::min),
        rel_max,
        unrel_min,
        unrelated_d.iter().cloned().fold(f64::MIN, f64::max),
    );
    println!(
        "[quality] separation: {} (margin {:.4})",
        if rel_max < unrel_min { "OK (related < unrelated, no overlap)" } else { "NG (overlap!)" },
        unrel_min - rel_max
    );

    // ---- パリティ: ollama (F16) vs ort (int8)
    println!("\n== parity: ollama bge-m3 (F16) vs ort int8 ==");
    if ollama_available() {
        let mut texts: Vec<&str> = Vec::new();
        for (a, b) in RELATED_PAIRS.iter().chain(UNRELATED_PAIRS.iter()) {
            texts.push(a);
            texts.push(b);
        }
        texts.push(SHORT_TEXT);
        texts.push(LONG_TEXT);

        let mut sims = Vec::new();
        for t in &texts {
            let v_ort = embedder.embed(t)?;
            let v_oll = ollama_embed(t)?;
            if v_ort.len() != v_oll.len() {
                println!("  dim mismatch: ort={} ollama={}", v_ort.len(), v_oll.len());
                break;
            }
            let s = cos_sim(&v_ort, &v_oll) as f64;
            sims.push(s);
            let label: String = t.chars().take(18).collect();
            println!("  cos_sim(ort_int8, ollama_f16) = {:.5}  [{}]", s, label);
        }
        if !sims.is_empty() {
            println!(
                "[parity] n={} min {:.5} / median {:.5} / max {:.5}",
                sims.len(),
                sims.iter().cloned().fold(f64::MAX, f64::min),
                median(sims.clone()),
                sims.iter().cloned().fold(f64::MIN, f64::max)
            );
        }

        // ollama 側でも同じペアの距離を出し、較正帯の一致を見る
        println!("--- pair distances: ort_int8 vs ollama_f16 ---");
        let mut deltas = Vec::new();
        for (kind, pairs) in [("related", RELATED_PAIRS), ("unrelated", UNRELATED_PAIRS)] {
            for (a, b) in pairs {
                let d_ort = cos_dist(&embedder.embed(a)?, &embedder.embed(b)?) as f64;
                let d_oll = cos_dist(&ollama_embed(a)?, &ollama_embed(b)?) as f64;
                deltas.push((d_ort - d_oll).abs());
                println!("  [{kind}] ort {:.4} / ollama {:.4} / delta {:+.4}", d_ort, d_oll, d_ort - d_oll);
            }
        }
        println!(
            "[parity] pair-distance |delta|: median {:.4} / max {:.4}",
            median(deltas.clone()),
            deltas.iter().cloned().fold(f64::MIN, f64::max)
        );
    } else {
        println!("[parity] SKIPPED: ollama (bge-m3) not reachable at {OLLAMA_BASE}");
    }

    // ---- 性能
    println!("\n== performance (after warmup) ==");
    let short_tokens = embedder.token_count(SHORT_TEXT)?;
    let long_tokens = embedder.token_count(LONG_TEXT)?;
    for (name, text, iters, tokens) in
        [("short", SHORT_TEXT, 21usize, short_tokens), ("long", LONG_TEXT, 11, long_tokens)]
    {
        let mut lat = Vec::new();
        for _ in 0..iters {
            let t = Instant::now();
            embedder.embed(text)?;
            lat.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        println!(
            "  {}: {} chars / {} tokens -> median {:.1}ms (min {:.1} / max {:.1}, n={})",
            name,
            text.chars().count(),
            tokens,
            median(lat.clone()),
            lat.iter().cloned().fold(f64::MAX, f64::min),
            lat.iter().cloned().fold(f64::MIN, f64::max),
            iters
        );
    }

    println!("\ndone.");
    Ok(())
}
