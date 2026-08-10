//! lindera(embedded IPADIC)による分かち書き。主索引(unicode61)の入力を作る。
//! 方式選定の実測根拠は docs/poc-report.md(PoC ②)。

use std::borrow::Cow;
use std::sync::OnceLock;

use lindera::dictionary::load_dictionary;
use lindera::mode::Mode;
use lindera::segmenter::Segmenter;

static SEGMENTER: OnceLock<Segmenter> = OnceLock::new();

fn segmenter() -> &'static Segmenter {
    SEGMENTER.get_or_init(|| {
        let dictionary = load_dictionary("embedded://ipadic").expect("embedded ipadic loads");
        Segmenter::new(Mode::Normal, dictionary, None)
    })
}

/// 本文・クエリ共通の分かち書き(スペース区切り)。
pub fn wakati(text: &str) -> String {
    match segmenter().segment(Cow::Borrowed(text)) {
        Ok(tokens) => tokens
            .iter()
            .map(|t| t.surface.as_ref())
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        // 分かち書き失敗は検索劣化として上位で扱う(fail-open)。素通しで返す。
        Err(_) => text.to_string(),
    }
}

/// クエリを FTS5 MATCH 式へ(空白区切りの各語をフレーズ化して AND)。
pub fn match_expr(query: &str) -> String {
    query
        .split_whitespace()
        .map(|w| format!("\"{}\"", wakati(w).replace('"', "")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 文まるごと用の OR 結合 MATCH 式。分かち書きした内容語(2文字以上または英数)を
/// OR で並べる。助詞・記号は落とし、重複は除く。
pub fn match_expr_any(query: &str) -> String {
    let mut seen = std::collections::BTreeSet::new();
    wakati(query)
        .split(' ')
        .filter(|t| t.chars().count() >= 2 && seen.insert(t.to_string()))
        .map(|t| format!("\"{}\"", t.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_char_word_survives() {
        let w = wakati("認証フローの見直しを行った。");
        assert!(w.split(' ').any(|t| t == "認証"), "wakati: {w}");
    }
}
