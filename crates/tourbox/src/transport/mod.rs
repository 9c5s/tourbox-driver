//! バイト列の送受信を担う接続の抽象と、接続の設定。

pub mod ble;
#[cfg(feature = "fake")]
pub mod fake;
pub mod usb;

use futures::future::BoxFuture;
use tokio::sync::mpsc;

use crate::error::TransportError;

/// 受信チャネルに届くもの。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    /// 受信したバイト列のかたまり。区切りは OS や通信の都合で決まり、メッセージの境界とは限らない。
    Data(Vec<u8>),
    /// 接続が切れた。これが最後に届き、この後チャネルは閉じる。
    Disconnected,
}

/// デバイスとの 1 本の接続。USB か BLE かは実行時に決まるので `Box<dyn Transport>` として扱う。
///
/// 受信したかたまりと切断の通知は、`take_receiver` で取り出す 1 本のチャネルに起きた順で届く。
/// 実装は `close` 以外で接続が終わった場合に必ず `Incoming::Disconnected` を送る。
/// 受け取る側は、`Disconnected` なしでチャネルが閉じた場合も切断として扱う (ADR-0017)。
pub trait Transport: Send {
    /// `data` を送り、送信が完了するまで待つ。
    ///
    /// 切断した後や `close` の後に呼ぶと `TransportError` を返す。
    ///
    /// 返す future は途中で破棄してよい。破棄した送信が行われたかどうかは不定で、
    /// 破棄の後も `close` は正常に動く (ADR-0017)。
    fn send<'a>(&'a mut self, data: &'a [u8]) -> BoxFuture<'a, Result<(), TransportError>>;

    /// 受信チャネルを取り出す。接続ごとに 1 回だけ取り出せ、2 回目以降は `None` を返す。
    fn take_receiver(&mut self) -> Option<mpsc::Receiver<Incoming>>;

    /// 接続を閉じ、読み取りスレッドや接続の終了を待ってから戻る。
    ///
    /// 切断の後にも呼べる。`close` による終了では `Incoming::Disconnected` を送らず、
    /// 戻った後は受信チャネルに何も送出しない。
    fn close(&mut self) -> BoxFuture<'_, Result<(), TransportError>>;
}

/// 接続方式の選択。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    /// USB のポート検出を先に試し、見つからなければ BLE をスキャンする。
    Auto,
    /// USB だけを使う。
    Usb,
    /// BLE だけを使う。
    Ble,
}

/// 接続の設定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionConfig {
    /// 接続方式。
    pub transport: TransportKind,
    /// USB のポート名の明示指定。`Auto` と `Usb` で使い、`Ble` では無視する。
    pub usb_port: Option<String>,
}
