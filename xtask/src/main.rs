//! 開発用の補助コマンド。`cargo xtask <サブコマンド>` で呼び出す。

mod adr;

use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "使い方: cargo xtask lint-adr [ディレクトリ]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("lint-adr") => lint_adr(args.get(1).map(String::as_str).unwrap_or(adr::ADR_DIR)),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn lint_adr(dir: &str) -> ExitCode {
    let entries = match adr::read_directory(Path::new(dir)) {
        Ok(entries) => entries,
        Err(err) => {
            eprintln!("{dir} を読み取れませんでした: {err}");
            return ExitCode::from(1);
        }
    };
    let errors = adr::check_adr_directory(&entries);
    for error in &errors {
        eprintln!("{error}");
    }
    if errors.is_empty() {
        let count = entries.len().saturating_sub(1);
        println!("ADR {count} 件の形式を検査し、問題はありませんでした。");
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "ADR の形式検査で {} 件の問題が見つかりました。",
            errors.len()
        );
        ExitCode::from(1)
    }
}
