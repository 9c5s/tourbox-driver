//! サブコマンドの登録と振り分け。

pub mod dump;
pub mod list_ports;
mod reload;
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

/// テストで、出たログを溜めて確かめる補助。
#[cfg(test)]
mod captured_logs {
    use std::io;
    use std::sync::{Arc, Mutex, PoisonError};

    use tracing::subscriber::DefaultGuard;
    use tracing::Level;

    /// このスレッドで出た debug 以上のログを、破棄するまで溜める。
    pub(super) struct CapturedLogs {
        buffer: Arc<Mutex<Vec<u8>>>,
        _guard: DefaultGuard,
    }

    impl CapturedLogs {
        /// このスレッドのログを溜め始める。
        pub(super) fn start() -> Self {
            let buffer = Arc::new(Mutex::new(Vec::new()));
            let writer = Arc::clone(&buffer);
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(Level::DEBUG)
                .with_ansi(false)
                .without_time()
                .with_writer(move || Writer(Arc::clone(&writer)))
                .finish();
            Self {
                buffer,
                _guard: tracing::subscriber::set_default(subscriber),
            }
        }

        /// `level` のログのうち、メッセージに `fragment` を含むものがあるか。
        pub(super) fn contains(&self, level: Level, fragment: &str) -> bool {
            let buffer = self.buffer.lock().unwrap_or_else(PoisonError::into_inner);
            String::from_utf8_lossy(&buffer)
                .lines()
                // 行の先頭は右寄せしたレベル名
                .any(|line| {
                    line.trim_start().starts_with(level.as_str()) && line.contains(fragment)
                })
        }
    }

    /// 溜め先へ書き込む。
    struct Writer(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}
