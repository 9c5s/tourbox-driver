//! `dump` サブコマンド。設定ファイルを読まずにデバイスへ接続し、DeviceEvent を表示する。

use std::io::{self, IsTerminal};

use anyhow::Context;
use tokio::runtime::Runtime;
use tourbox::device::{Device, DeviceEvent};
use tourbox::protocol::HapticConfig;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::cli::DumpArgs;

/// `RUST_LOG` がないときのログの絞り込み。device が状態ごとに出す受信バイトの debug ログを含める。
const DEFAULT_FILTER: &str = "tourbox=debug,info";
/// `--verbose` のときのログの絞り込み。
const VERBOSE_FILTER: &str = "debug";

/// デバイスへ接続して DeviceEvent を表示し続け、Ctrl+C で接続を閉じて戻る。
pub fn run(args: &DumpArgs, verbose: bool) -> anyhow::Result<()> {
    init_logging(verbose)?;
    let runtime = Runtime::new().context("非同期ランタイムを起動できませんでした。")?;
    runtime.block_on(dump(args))
}

/// ログを標準エラー出力に出す。`RUST_LOG` があればその絞り込みに従う。
fn init_logging(verbose: bool) -> anyhow::Result<()> {
    let filter = if std::env::var_os(EnvFilter::DEFAULT_ENV).is_some() {
        // このエラーは原因を Display と source の両方に含むので、連鎖にせず 1 行にまとめる
        EnvFilter::try_from_default_env().map_err(|error| {
            anyhow::anyhow!("環境変数 RUST_LOG の値を解釈できませんでした: {error}")
        })?
    } else if verbose {
        EnvFilter::new(VERBOSE_FILTER)
    } else {
        EnvFilter::new(DEFAULT_FILTER)
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .init();
    Ok(())
}

/// 接続を開始し、Ctrl+C を受け付けるまで DeviceEvent を標準出力に表示する。
async fn dump(args: &DumpArgs) -> anyhow::Result<()> {
    let config = args.connection_config();
    info!(
        transport = ?config.transport,
        usb_port = ?config.usb_port,
        "TourBox への接続を開始します。Ctrl+C で終了します。"
    );
    let (mut events, handle) = Device::run(config, HapticConfig::default());
    let mut ctrl_c = std::pin::pin!(tokio::signal::ctrl_c());
    let result = loop {
        tokio::select! {
            received = &mut ctrl_c => {
                break received.context("Ctrl+C を受け付けられませんでした。");
            }
            event = events.recv() => match event {
                Some(event) => println!("{}", describe(&event)),
                // device のタスクは停止要求まで終わらない。終わるのはパニックしたときで、shutdown がそのパニックを再開する
                None => break Ok(()),
            },
        }
    };
    info!("接続を閉じます。");
    handle.shutdown().await;
    result
}

/// DeviceEvent を表示用の文にする。
fn describe(event: &DeviceEvent) -> String {
    match event {
        DeviceEvent::Connected => "接続しました。".to_owned(),
        DeviceEvent::Disconnected => "切断しました。".to_owned(),
        DeviceEvent::Input(input) => format!("入力: {input:?}"),
    }
}
