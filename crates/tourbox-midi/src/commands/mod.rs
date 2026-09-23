//! サブコマンドの登録と振り分け。

pub mod dump;
pub mod list_ports;

use crate::cli::{Cli, Command};

/// 解析済みのコマンドラインに従ってサブコマンドを実行する。
pub fn run(cli: &Cli) -> anyhow::Result<()> {
    match &cli.command {
        Some(Command::Dump(args)) => dump::run(args, cli.verbose),
        Some(Command::ListPorts) => list_ports::run(),
        None => anyhow::bail!("常駐機能はまだ実装されていません。"),
    }
}
