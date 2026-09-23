//! TourBox Elite の操作を MIDI メッセージに変換する常駐アプリ。

pub mod cli;
pub mod commands;
pub mod config;
pub mod engine;
pub mod midi;
pub mod midi_msg;
pub mod output;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::Cli;

/// コマンドラインアプリの入口。引数を解析してサブコマンドを実行し、終了コードを返す。
///
/// 不正な引数は clap がエラーを表示して終了コード 2 で終了させる。
pub fn run_cli() -> ExitCode {
    let cli = Cli::parse();
    match commands::run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            for cause in error.chain().skip(1) {
                eprintln!("原因: {cause}");
            }
            ExitCode::FAILURE
        }
    }
}
