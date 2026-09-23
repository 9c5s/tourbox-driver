//! ライブラリのエラー型。

use thiserror::Error;

/// 接続と送受信のエラー。
#[derive(Debug, Error)]
pub enum TransportError {
    /// 入出力のエラー。
    #[error("入出力でエラーが発生しました: {0}")]
    Io(#[from] std::io::Error),
    /// BLE の通信のエラー。
    #[error("BLE の通信でエラーが発生しました: {0}")]
    Ble(String),
    /// 接続先のデバイスが見つからない。
    #[error("TourBox が見つかりません。")]
    NotFound,
    /// ポートは見つかったが、他のプロセスが開いている。
    #[error("TourBox のポートは他のプロセスが使用中です。")]
    Busy,
}
