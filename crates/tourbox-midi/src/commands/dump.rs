//! `dump` サブコマンド。設定ファイルを読まずにデバイスへ接続し、DeviceEvent を表示する。

use anyhow::Context;
use tokio::runtime::Runtime;
use tourbox::device::{Device, DeviceEvent};
use tourbox::protocol::HapticConfig;
use tracing::info;

use crate::cli::DumpArgs;

/// `RUST_LOG` がないときのログの絞り込み。device が状態ごとに出す受信バイトの debug ログを含める。
const DEFAULT_FILTER: &str = "tourbox=debug,info";
/// `--verbose` のときのログの絞り込み。
const VERBOSE_FILTER: &str = "debug";

/// デバイスへ接続して DeviceEvent を表示し続け、Ctrl+C で接続を閉じて戻る。
pub fn run(args: &DumpArgs, verbose: bool) -> anyhow::Result<()> {
    super::init_logging(if verbose {
        VERBOSE_FILTER
    } else {
        DEFAULT_FILTER
    })?;
    let runtime = Runtime::new().context("非同期ランタイムを起動できませんでした。")?;
    runtime.block_on(dump(args))
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
