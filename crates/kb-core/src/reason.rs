//! 操作の理由。**記録の残らない破壊的操作を作らない**という規律の共通部品。
//!
//! 削除・蒸留・initiative の終了などが同じ形の理由を受け取る。同じ検証が
//! 各所に散ると、上限や許可文字を変えるときに一部だけ古いまま残る。

use anyhow::{Result, bail};

/// 1行・1〜500文字。前後の空白は落として返す。
///
/// `label` は拒否したときの言い回しに使う(「purge の理由」「削除理由」など)。
pub fn validate<'a>(reason: &'a str, label: &str) -> Result<&'a str> {
    let trimmed = reason.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 500 || trimmed.contains(['\n', '\r']) {
        bail!("{label}は1〜500文字の一行で指定する");
    }
    Ok(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_short_line_passes_and_is_trimmed() {
        assert_eq!(validate("  壊れている  ", "理由").unwrap(), "壊れている");
    }

    #[test]
    fn empty_multiline_and_too_long_are_refused() {
        assert!(validate("   ", "理由").is_err());
        assert!(validate("一行目\n二行目", "理由").is_err());
        assert!(validate(&"あ".repeat(501), "理由").is_err());
    }
}
