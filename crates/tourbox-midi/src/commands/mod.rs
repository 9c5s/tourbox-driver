//! サブコマンドの登録と振り分け。

pub mod dump;
pub mod list_ports;
pub mod run;

use std::io::{self, IsTerminal};

use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, Command};

/// 解析済みのコマンドラインに従ってサブコマンドを実行する。サブコマンドがなければ常駐する。
pub fn run(cli: &Cli) -> anyhow::Result<()> {
    match &cli.command {
        Some(Command::Dump(args)) => dump::run(args, cli.verbose),
        Some(Command::ListPorts) => list_ports::run(),
        None => run::run(cli.config.as_deref(), cli.verbose),
    }
}

/// ログを標準エラー出力に出す。`RUST_LOG` があればその絞り込みに従い、なければ `filter` を使う。
fn init_logging(filter: &str) -> anyhow::Result<()> {
    let filter = if std::env::var_os(EnvFilter::DEFAULT_ENV).is_some() {
        // このエラーは原因を Display と source の両方に含むので、連鎖にせず 1 行にまとめる
        EnvFilter::try_from_default_env().map_err(|error| {
            anyhow::anyhow!("環境変数 RUST_LOG の値を解釈できませんでした: {error}")
        })?
    } else {
        EnvFilter::new(filter)
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .init();
    Ok(())
}
