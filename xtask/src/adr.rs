//! ADR の形式検査。書き方の規約は docs/adr/README.md にある。

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::path::Path;

/// ADR の置き場所。
pub const ADR_DIR: &str = "docs/adr";

const SECTIONS: [&str; 4] = ["文脈", "決定", "検討した選択肢", "結果"];
const STATUS_LIST: &str = "「提案」「承認」「廃止」「置換 (ADR-NNNN による)」";

struct Adr {
    name: String,
    number: String,
    title: Option<String>,
    status: Option<String>,
    date: Option<String>,
}

struct Row {
    number: String,
    link: String,
    title: String,
    status: String,
    date: String,
}

/// ディレクトリ直下のファイル名と内容を読む。
pub fn read_directory(dir: &Path) -> io::Result<BTreeMap<String, String>> {
    let mut entries = BTreeMap::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        entries.insert(name, fs::read_to_string(entry.path())?);
    }
    Ok(entries)
}

/// ADR ディレクトリの各ファイル名と内容を受け取り、規約に反する点を文で返す。
/// 問題がなければ空の Vec を返す。
pub fn check_adr_directory(entries: &BTreeMap<String, String>) -> Vec<String> {
    let mut errors = Vec::new();
    let mut adrs = Vec::new();
    for (name, content) in entries {
        if name == "README.md" {
            continue;
        }
        match parse_file_name(name) {
            Some((number, name_title)) => {
                adrs.push(check_adr_file(
                    name,
                    number,
                    name_title,
                    content,
                    &mut errors,
                ));
            }
            None => errors.push(format!(
                "{name}: ファイル名は「NNNN-題名.md」の形式にしてください。"
            )),
        }
    }
    check_numbering(&adrs, &mut errors);
    match entries.get("README.md") {
        Some(readme) => check_list(readme, &adrs, &mut errors),
        None => errors.push("README.md: 一覧を持つ README.md が必要です。".to_string()),
    }
    errors
}

/// `NNNN-題名.md` を番号と題名に分ける。
fn parse_file_name(name: &str) -> Option<(&str, &str)> {
    let stem = name.strip_suffix(".md")?;
    let (number, title) = stem.split_at_checked(4)?;
    if !is_digits(number) {
        return None;
    }
    let title = title.strip_prefix('-')?;
    if title.is_empty() {
        return None;
    }
    Some((number, title))
}

fn is_digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
}

/// 本文の題名をファイル名に使う形にする。空白と読点を除く。
fn file_title(title: &str) -> String {
    title
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '、')
        .collect()
}

/// 状態の先頭の語。括弧の補足は除く。
fn status_keyword(status: &str) -> &str {
    status.split(' ').next().unwrap_or(status)
}

/// 一覧と本文の状態が同じ決定を表すか。置換は置換先まで一致を求める。
fn same_status(a: &str, b: &str) -> bool {
    let keyword = status_keyword(a);
    if keyword != status_keyword(b) {
        return false;
    }
    keyword != "置換" || a == b
}

/// 「提案」「承認」「廃止」(括弧の補足は任意) か「置換 (ADR-NNNN による)」か。
fn is_valid_status(value: &str) -> bool {
    for keyword in ["提案", "承認", "廃止"] {
        if value == keyword {
            return true;
        }
        if let Some(rest) = value.strip_prefix(keyword) {
            if let Some(inner) = rest.strip_prefix(" (").and_then(|r| r.strip_suffix(')')) {
                return !inner.is_empty();
            }
        }
    }
    if let Some(inner) = value
        .strip_prefix("置換 (ADR-")
        .and_then(|r| r.strip_suffix(" による)"))
    {
        return inner.len() == 4 && is_digits(inner);
    }
    false
}

/// YYYY-MM-DD 形式で、暦の上に実在する日付か。
fn is_calendar_date(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    if parts.len() != 3 || parts[0].len() != 4 || parts[1].len() != 2 || parts[2].len() != 2 {
        return false;
    }
    if !parts.iter().all(|p| is_digits(p)) {
        return false;
    }
    let year: u32 = parts[0].parse().unwrap_or(0);
    let month: u32 = parts[1].parse().unwrap_or(0);
    let day: u32 = parts[2].parse().unwrap_or(0);
    if !(1..=12).contains(&month) || day == 0 {
        return false;
    }
    let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if leap => 29,
        _ => 28,
    };
    day <= days
}

/// ADR 1 件の本文を検査し、一覧との突き合わせに使う項目を返す。
fn check_adr_file(
    name: &str,
    number: &str,
    name_title: &str,
    content: &str,
    errors: &mut Vec<String>,
) -> Adr {
    let normalized = content.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let line = |i: usize| lines.get(i).copied().unwrap_or("");
    let mut adr = Adr {
        name: name.to_string(),
        number: number.to_string(),
        title: None,
        status: None,
        date: None,
    };

    match parse_title_line(line(0)) {
        None => errors.push(format!(
            "{name}: 1 行目は「# ADR-{number}: 題名」の形式で書いてください。"
        )),
        Some((title_number, title)) => {
            adr.title = Some(title.to_string());
            if title_number != number {
                errors.push(format!(
                    "{name}: 本文の番号 (ADR-{title_number}) がファイル名の番号と一致しません。"
                ));
            }
            if file_title(title) != name_title {
                errors.push(format!(
                    "{name}: ファイル名は本文の題名から空白と読点を除いた「{number}-{}.md」にしてください。",
                    file_title(title)
                ));
            }
        }
    }

    if !line(1).is_empty() {
        errors.push(format!("{name}: 2 行目は空行にしてください。"));
    }

    match line(2).strip_prefix("- 状態: ").filter(|v| !v.is_empty()) {
        None => errors.push(format!(
            "{name}: 3 行目は「- 状態: 状態」の形式で書いてください。"
        )),
        Some(value) => {
            adr.status = Some(value.to_string());
            if !is_valid_status(value) {
                errors.push(format!(
                    "{name}: 状態は {STATUS_LIST} のいずれかにしてください (現在: {value})。"
                ));
            }
        }
    }

    match line(3).strip_prefix("- 日付: ").filter(|v| !v.is_empty()) {
        None => errors.push(format!(
            "{name}: 4 行目は「- 日付: YYYY-MM-DD」の形式で書いてください。"
        )),
        Some(value) => {
            adr.date = Some(value.to_string());
            if !is_calendar_date(value) {
                errors.push(format!(
                    "{name}: 日付は YYYY-MM-DD 形式の実在する日付にしてください (現在: {value})。"
                ));
            }
        }
    }

    check_sections(name, &lines, errors);
    adr
}

/// `# ADR-NNNN: 題名` を番号と題名に分ける。
fn parse_title_line(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("# ADR-")?;
    let (number, rest) = rest.split_at_checked(4)?;
    if !is_digits(number) {
        return None;
    }
    let title = rest.strip_prefix(": ")?;
    if title.is_empty() {
        return None;
    }
    Some((number, title))
}

/// 節が規定の 4 つを規定の順で持ち、どの節も空でないことを検査する。
fn check_sections(name: &str, lines: &[&str], errors: &mut Vec<String>) {
    let headings: Vec<(String, usize)> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            line.strip_prefix("## ")
                .map(|s| (s.trim().to_string(), index))
        })
        .collect();
    let in_order = headings.len() == SECTIONS.len()
        && headings
            .iter()
            .zip(SECTIONS.iter())
            .all(|((s, _), expected)| s == expected);
    if !in_order {
        let expected: String = SECTIONS.iter().map(|s| format!("「## {s}」")).collect();
        let current: String = headings
            .iter()
            .map(|(s, _)| format!("「## {s}」"))
            .collect();
        let current = if current.is_empty() {
            "なし".to_string()
        } else {
            current
        };
        errors.push(format!(
            "{name}: 節は {expected} の順に、この 4 つだけを書いてください (現在: {current})。"
        ));
        return;
    }
    for (i, (section, index)) in headings.iter().enumerate() {
        let end = headings
            .get(i + 1)
            .map(|(_, idx)| *idx)
            .unwrap_or(lines.len());
        let body = &lines[index + 1..end];
        if body.iter().all(|line| line.trim().is_empty()) {
            errors.push(format!(
                "{name}: 「{section}」の節が空です。本文を書いてください。"
            ));
        }
    }
}

/// 番号が重複せず、0000 からの連番であることを検査する。
fn check_numbering(adrs: &[Adr], errors: &mut Vec<String>) {
    let mut names_by_number: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for adr in adrs {
        names_by_number
            .entry(&adr.number)
            .or_default()
            .push(&adr.name);
    }
    for (number, names) in &names_by_number {
        if names.len() > 1 {
            errors.push(format!(
                "番号 {number} が重複しています: {}",
                names.join("、")
            ));
        }
    }
    let numbers: HashSet<u32> = names_by_number
        .keys()
        .filter_map(|n| n.parse().ok())
        .collect();
    let max = numbers.iter().copied().max();
    if let Some(max) = max {
        for expected in 0..=max {
            if !numbers.contains(&expected) {
                errors.push(format!(
                    "番号 {expected:04} が欠番です。番号は 0000 からの連番にしてください。"
                ));
            }
        }
    }
}

/// `| [NNNN](link) | 題名 | 状態 | 日付 |` の行を読む。
fn parse_row(line: &str) -> Option<Row> {
    let inner = line.strip_prefix("| ")?.strip_suffix(" |")?;
    let cells: Vec<&str> = inner.split(" | ").collect();
    if cells.len() != 4 {
        return None;
    }
    let link_cell = cells[0].strip_prefix('[')?;
    let (number, rest) = link_cell.split_once("](")?;
    if number.len() != 4 || !is_digits(number) {
        return None;
    }
    let link = rest.strip_suffix(')')?;
    if link.is_empty() || cells[1..].iter().any(|c| c.is_empty()) {
        return None;
    }
    Some(Row {
        number: number.to_string(),
        link: link.to_string(),
        title: cells[1].to_string(),
        status: cells[2].to_string(),
        date: cells[3].to_string(),
    })
}

fn parse_rows(readme: &str) -> Vec<Row> {
    readme
        .replace("\r\n", "\n")
        .lines()
        .filter_map(parse_row)
        .collect()
}

/// README の一覧が各 ADR と過不足なく対応し、題名、状態、日付が本文と一致することを検査する。
fn check_list(readme: &str, adrs: &[Adr], errors: &mut Vec<String>) {
    let rows = parse_rows(readme);
    let mut seen = HashSet::new();
    for row in &rows {
        if !seen.insert(row.link.as_str()) {
            errors.push(format!(
                "README.md: 一覧に {} の行が重複しています。",
                row.link
            ));
        }
    }

    let mut consumed: HashSet<&str> = HashSet::new();
    for adr in adrs {
        let candidates: Vec<&Row> = rows.iter().filter(|row| row.number == adr.number).collect();
        let row = candidates
            .iter()
            .find(|c| c.link == adr.name)
            .copied()
            .or_else(|| {
                if candidates.len() == 1 {
                    Some(candidates[0])
                } else {
                    None
                }
            });
        let Some(row) = row else {
            errors.push(format!(
                "README.md: 一覧に {} ({}) の行がありません。",
                adr.number, adr.name
            ));
            continue;
        };
        consumed.insert(row.link.as_str());
        if row.link != adr.name {
            errors.push(format!(
                "README.md: {} の行のリンク先 ({}) がファイル名 ({}) と一致しません。",
                adr.number, row.link, adr.name
            ));
        }
        if let Some(title) = &adr.title {
            if &row.title != title {
                errors.push(format!(
                    "README.md: {} の行の題名 ({}) が本文の題名 ({title}) と一致しません。",
                    adr.number, row.title
                ));
            }
        }
        if let Some(status) = &adr.status {
            if !same_status(&row.status, status) {
                errors.push(format!(
                    "README.md: {} の行の状態 ({}) が本文の状態 ({status}) と一致しません。",
                    adr.number, row.status
                ));
            }
        }
        if let Some(date) = &adr.date {
            if &row.date != date {
                errors.push(format!(
                    "README.md: {} の行の日付 ({}) が本文の日付 ({date}) と一致しません。",
                    adr.number, row.date
                ));
            }
        }
    }

    for row in &rows {
        if !consumed.contains(row.link.as_str()) {
            errors.push(format!(
                "README.md: 一覧の {} の行が指すファイル ({}) がありません。",
                row.number, row.link
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TITLE: &str = "テストを ADR として、記録する";
    const VALID_BODY: &str = "## 文脈\n\n文脈である。\n\n## 決定\n\n決定である。\n\n## 検討した選択肢\n\n- 選択肢である。\n\n## 結果\n\n- 結果である。\n";

    struct AdrOptions {
        number: &'static str,
        title: &'static str,
        status: &'static str,
        date: &'static str,
        body: &'static str,
    }

    impl Default for AdrOptions {
        fn default() -> Self {
            Self {
                number: "0000",
                title: TITLE,
                status: "承認",
                date: "2026-09-06",
                body: VALID_BODY,
            }
        }
    }

    /// 規約どおりの ADR 本文を組み立てる。
    fn adr(o: AdrOptions) -> String {
        format!(
            "# ADR-{}: {}\n\n- 状態: {}\n- 日付: {}\n\n{}",
            o.number, o.title, o.status, o.date, o.body
        )
    }

    /// 本文の題名から規約どおりのファイル名を作る。
    fn file_name(number: &str, title: &str) -> String {
        format!("{number}-{}.md", file_title(title))
    }

    /// 一覧の 1 行を作る。
    fn row(number: &str, title: &str, status: &str, date: &str, link: Option<&str>) -> String {
        let link = link
            .map(String::from)
            .unwrap_or_else(|| file_name(number, title));
        format!("| [{number}]({link}) | {title} | {status} | {date} |")
    }

    /// 一覧を持つ README を作る。
    fn readme(rows: &[String]) -> String {
        let mut lines = vec![
            "# ADR".to_string(),
            String::new(),
            "## 一覧".to_string(),
            String::new(),
            "| 番号 | 題名 | 状態 | 日付 |".to_string(),
            "| --- | --- | --- | --- |".to_string(),
        ];
        lines.extend(rows.iter().cloned());
        lines.push(String::new());
        lines.join("\n")
    }

    fn default_row() -> String {
        row("0000", TITLE, "承認", "2026-09-06", None)
    }

    /// 規約どおりの 1 件と README を持つディレクトリ。
    fn valid_directory() -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert(file_name("0000", TITLE), adr(AdrOptions::default()));
        m.insert("README.md".to_string(), readme(&[default_row()]));
        m
    }

    fn errors_of(entries: &BTreeMap<String, String>) -> Vec<String> {
        check_adr_directory(entries)
    }

    fn assert_single_error_contains(entries: &BTreeMap<String, String>, needle: &str) {
        let errors = errors_of(entries);
        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(errors[0].contains(needle), "errors: {errors:?}");
    }

    #[test]
    fn valid_directory_has_no_errors() {
        assert!(errors_of(&valid_directory()).is_empty());
    }

    #[test]
    fn crlf_content_is_accepted() {
        let mut entries = valid_directory();
        for value in entries.values_mut() {
            *value = value.replace('\n', "\r\n");
        }
        assert!(errors_of(&entries).is_empty());
    }

    #[test]
    fn file_name_must_match_pattern() {
        let mut entries = valid_directory();
        entries.insert("memo.md".to_string(), adr(AdrOptions::default()));
        assert_single_error_contains(&entries, "memo.md: ファイル名は「NNNN-題名.md」");
    }

    #[test]
    fn title_line_must_match() {
        let mut entries = valid_directory();
        let name = file_name("0000", TITLE);
        entries.insert(
            name.clone(),
            adr(AdrOptions::default()).replace("# ADR-0000: ", "# "),
        );
        assert_single_error_contains(&entries, "1 行目は「# ADR-0000: 題名」");
    }

    #[test]
    fn title_number_must_match_file_name() {
        let mut entries = valid_directory();
        let name = file_name("0000", TITLE);
        entries.insert(
            name,
            adr(AdrOptions {
                number: "0001",
                ..Default::default()
            }),
        );
        let errors = errors_of(&entries);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("本文の番号 (ADR-0001) がファイル名の番号と一致しません")),
            "{errors:?}"
        );
    }

    #[test]
    fn file_title_must_match_body_title() {
        let mut entries = valid_directory();
        entries.remove(&file_name("0000", TITLE));
        entries.insert("0000-別の題名.md".to_string(), adr(AdrOptions::default()));
        let errors = errors_of(&entries);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("「0000-テストをADRとして記録する.md」にしてください")),
            "{errors:?}"
        );
    }

    #[test]
    fn second_line_must_be_blank() {
        let mut entries = valid_directory();
        let name = file_name("0000", TITLE);
        entries.insert(
            name,
            adr(AdrOptions::default()).replacen("\n\n- 状態", "\n- 状態", 1),
        );
        let errors = errors_of(&entries);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("2 行目は空行にしてください")),
            "{errors:?}"
        );
    }

    #[test]
    fn status_must_be_known_keyword() {
        let mut entries = valid_directory();
        let name = file_name("0000", TITLE);
        entries.insert(
            name,
            adr(AdrOptions {
                status: "検討中",
                ..Default::default()
            }),
        );
        let errors = errors_of(&entries);
        assert!(errors.iter().any(|e| e.contains("状態は 「提案」「承認」「廃止」「置換 (ADR-NNNN による)」 のいずれかにしてください (現在: 検討中)")), "{errors:?}");
    }

    #[test]
    fn status_with_note_and_replacement_are_accepted() {
        for status in [
            "承認 (2026-09-23、Discord で承認)",
            "置換 (ADR-0003 による)",
            "廃止",
        ] {
            let mut entries = BTreeMap::new();
            entries.insert(
                file_name("0000", TITLE),
                adr(AdrOptions {
                    status,
                    ..Default::default()
                }),
            );
            entries.insert(
                "README.md".to_string(),
                readme(&[row("0000", TITLE, status, "2026-09-06", None)]),
            );
            assert!(errors_of(&entries).is_empty(), "status: {status}");
        }
    }

    #[test]
    fn replacement_status_must_name_an_adr() {
        let mut entries = valid_directory();
        let name = file_name("0000", TITLE);
        entries.insert(
            name,
            adr(AdrOptions {
                status: "置換 (ADR-3 による)",
                ..Default::default()
            }),
        );
        let errors = errors_of(&entries);
        assert!(errors.iter().any(|e| e.contains("状態は")), "{errors:?}");
    }

    #[test]
    fn date_must_be_a_calendar_date() {
        for bad in ["2026-13-01", "2026-02-30", "2026/09/06", "26-09-06"] {
            let mut entries = valid_directory();
            let name = file_name("0000", TITLE);
            entries.insert(
                name,
                adr(AdrOptions {
                    date: bad,
                    ..Default::default()
                }),
            );
            let errors = errors_of(&entries);
            assert!(
                errors
                    .iter()
                    .any(|e| e.contains("実在する日付にしてください")),
                "date {bad}: {errors:?}"
            );
        }
        assert!(is_calendar_date("2024-02-29"));
        assert!(!is_calendar_date("2023-02-29"));
    }

    #[test]
    fn sections_must_be_the_four_in_order() {
        let body = "## 決定\n\n決定である。\n\n## 文脈\n\n文脈である。\n\n## 検討した選択肢\n\n- 選択肢である。\n\n## 結果\n\n- 結果である。\n";
        let mut entries = valid_directory();
        let name = file_name("0000", TITLE);
        entries.insert(
            name,
            adr(AdrOptions {
                body,
                ..Default::default()
            }),
        );
        assert_single_error_contains(
            &entries,
            "節は 「## 文脈」「## 決定」「## 検討した選択肢」「## 結果」 の順に",
        );
    }

    #[test]
    fn extra_section_is_rejected() {
        let body = format!("{VALID_BODY}\n## 補足\n\n補足である。\n");
        let mut entries = valid_directory();
        let name = file_name("0000", TITLE);
        let text = format!("# ADR-0000: {TITLE}\n\n- 状態: 承認\n- 日付: 2026-09-06\n\n{body}");
        entries.insert(name, text);
        assert_single_error_contains(&entries, "この 4 つだけを書いてください");
    }

    #[test]
    fn empty_section_is_rejected() {
        let body = "## 文脈\n\n文脈である。\n\n## 決定\n\n\n## 検討した選択肢\n\n- 選択肢である。\n\n## 結果\n\n- 結果である。\n";
        let mut entries = valid_directory();
        let name = file_name("0000", TITLE);
        entries.insert(
            name,
            adr(AdrOptions {
                body,
                ..Default::default()
            }),
        );
        assert_single_error_contains(&entries, "「決定」の節が空です");
    }

    #[test]
    fn numbers_must_be_contiguous_from_zero() {
        let mut entries = valid_directory();
        entries.insert(
            file_name("0002", "二番目"),
            adr(AdrOptions {
                number: "0002",
                title: "二番目",
                ..Default::default()
            }),
        );
        let rows = [
            default_row(),
            row("0002", "二番目", "承認", "2026-09-06", None),
        ];
        entries.insert("README.md".to_string(), readme(&rows));
        assert_single_error_contains(&entries, "番号 0001 が欠番です");
    }

    #[test]
    fn duplicate_numbers_are_rejected() {
        let mut entries = valid_directory();
        entries.insert(
            "0000-別の題名.md".to_string(),
            adr(AdrOptions {
                title: "別の題名",
                ..Default::default()
            }),
        );
        let rows = [
            default_row(),
            row("0000", "別の題名", "承認", "2026-09-06", None),
        ];
        entries.insert("README.md".to_string(), readme(&rows));
        let errors = errors_of(&entries);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("番号 0000 が重複しています")),
            "{errors:?}"
        );
    }

    #[test]
    fn readme_is_required() {
        let mut entries = valid_directory();
        entries.remove("README.md");
        assert_single_error_contains(&entries, "README.md: 一覧を持つ README.md が必要です");
    }

    #[test]
    fn readme_row_is_required_for_each_adr() {
        let mut entries = valid_directory();
        entries.insert("README.md".to_string(), readme(&[]));
        assert_single_error_contains(
            &entries,
            "一覧に 0000 (0000-テストをADRとして記録する.md) の行がありません",
        );
    }

    #[test]
    fn readme_row_link_title_status_date_must_match() {
        let mut entries = valid_directory();
        let r = row(
            "0000",
            "違う題名",
            "提案",
            "2026-09-07",
            Some("0000-ちがう.md"),
        );
        entries.insert("README.md".to_string(), readme(&[r]));
        let errors = errors_of(&entries);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("リンク先 (0000-ちがう.md) がファイル名")),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("題名 (違う題名) が本文の題名")),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("状態 (提案) が本文の状態")),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("日付 (2026-09-07) が本文の日付")),
            "{errors:?}"
        );
    }

    #[test]
    fn readme_status_note_may_differ_but_keyword_must_match() {
        let mut entries = valid_directory();
        entries.insert(
            "README.md".to_string(),
            readme(&[row(
                "0000",
                TITLE,
                "承認 (結果の一部を ADR-0001 で置換)",
                "2026-09-06",
                None,
            )]),
        );
        assert!(errors_of(&entries).is_empty());
    }

    #[test]
    fn replacement_rows_must_match_exactly() {
        let mut entries = BTreeMap::new();
        entries.insert(
            file_name("0000", TITLE),
            adr(AdrOptions {
                status: "置換 (ADR-0001 による)",
                ..Default::default()
            }),
        );
        entries.insert(
            "README.md".to_string(),
            readme(&[row(
                "0000",
                TITLE,
                "置換 (ADR-0002 による)",
                "2026-09-06",
                None,
            )]),
        );
        let errors = errors_of(&entries);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("状態 (置換 (ADR-0002 による)) が本文の状態")),
            "{errors:?}"
        );
    }

    #[test]
    fn readme_rows_without_files_are_rejected() {
        let mut entries = valid_directory();
        let rows = [
            default_row(),
            row("0001", "存在しない", "承認", "2026-09-06", None),
        ];
        entries.insert("README.md".to_string(), readme(&rows));
        assert_single_error_contains(
            &entries,
            "一覧の 0001 の行が指すファイル (0001-存在しない.md) がありません",
        );
    }

    #[test]
    fn duplicate_readme_rows_are_rejected() {
        let mut entries = valid_directory();
        let rows = [default_row(), default_row()];
        entries.insert("README.md".to_string(), readme(&rows));
        assert_single_error_contains(
            &entries,
            "一覧に 0000-テストをADRとして記録する.md の行が重複しています",
        );
    }
}
