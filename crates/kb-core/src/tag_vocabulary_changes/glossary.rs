//! 語彙節の表だけを差し替え、AIの運用本文・判断の根拠を残す。

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, ensure};

pub(super) fn rewrite(body: &str, entries: &BTreeMap<String, String>) -> Result<String> {
    let mut sections = 0;
    let mut in_section = false;
    let mut section_end = body.len();
    let mut blocks = Vec::<(usize, usize)>::new();
    let mut block = None;
    let mut offset = 0;
    let mut tags = BTreeSet::new();
    for line in body.split_inclusive('\n') {
        let text = line.trim();
        if let Some(heading) = text.strip_prefix('#') {
            if in_section {
                section_end = offset;
            }
            in_section = heading
                .trim_start_matches('#')
                .trim_start()
                .starts_with("語彙");
            if in_section {
                sections += 1;
                section_end = body.len();
            }
        }
        if in_section && text.starts_with('|') {
            let cells = text
                .trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect::<Vec<_>>();
            ensure!(
                cells.len() == 2,
                "語彙表はタグ・説明の2列へ修復してから計画する"
            );
            let tag = cells[0];
            if !matches!(tag, "タグ" | "tag" | "名前") && !tag.chars().all(|c| c == '-' || c == ':')
            {
                crate::tags::validate_shape(tag)?;
                ensure!(
                    !cells[1].is_empty(),
                    "説明の空の語彙表を修復してから計画する"
                );
                ensure!(
                    tags.insert(tag.to_string()),
                    "重複した語彙表の行を修復してから計画する: {tag}"
                );
            }
            if block.is_none() {
                block = Some(offset);
            }
        } else if let Some(start) = block.take() {
            blocks.push((start, offset));
        }
        offset += line.len();
    }
    if let Some(start) = block {
        blocks.push((start, body.len()));
    }
    ensure!(
        sections <= 1 && blocks.len() <= 1,
        "複数の語彙節・表があるため、1つに整理してから計画する"
    );
    let mut table = "| タグ | 説明 |\n| --- | --- |\n".to_string();
    for (tag, description) in entries {
        table.push_str(&format!("| {tag} | {description} |\n"));
    }
    let rewritten = if let Some((start, end)) = blocks.first() {
        format!("{}{table}{}", &body[..*start], &body[*end..])
    } else if sections == 1 {
        format!(
            "{}\n{table}\n{}",
            &body[..section_end],
            &body[section_end..]
        )
    } else {
        format!("{body}\n\n## 語彙\n\n{table}")
    };
    let parsed = crate::tags::parse_glossary(String::new(), &rewritten);
    ensure!(
        parsed.entries == *entries && parsed.skipped.is_empty(),
        "語彙表の再読込が変更案と一致しない"
    );
    Ok(rewritten)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_policy_and_other_tables_byte_for_byte() {
        let prefix = "# タグの方針\n本人の訂正を優先する。\n\n## 語彙\n前置き。\n";
        let suffix = "\n語彙節の後書き。\n## 計測\n| 種類 | 値 |\n| old | 10 |\n";
        let body =
            format!("{prefix}| tag | description |\n| --- | --- |\n| old | 旧語 |\n{suffix}");
        let result = rewrite(&body, &BTreeMap::from([("new".into(), "新語".into())])).unwrap();
        assert!(result.starts_with(prefix));
        assert!(result.ends_with(suffix));
        assert!(!result.contains("| old | 旧語 |"));
    }

    #[test]
    fn refuses_ambiguous_tables_instead_of_losing_prose_or_entries() {
        for body in [
            "## 語彙\n| old | 旧語 |\n| old | 重複 |\n",
            "## 語彙\n| old | 旧語 | 余分 |\n",
            "## 語彙\n| old | 旧語 |\n文章\n| newer | 別表 |\n",
            "## 語彙\n| old | 旧語 |\n## 語彙2\n| newer | 別節 |\n",
        ] {
            assert!(rewrite(body, &BTreeMap::new()).is_err(), "{body}");
        }
    }
}
