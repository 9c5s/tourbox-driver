//! ライブラリのエラー型。

use thiserror::Error;

/// 接続と送受信のエラー。
#[derive(Debug, Error)]
pub enum TransportError {
    /// 入出力のエラー。
    #[error("入出力でエラーが発生しました: {0}")]
    Io(std::io::Error),
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

// `#[from]` だと原因が source にも入り、Display の `{0}` と二重に表示される
impl From<std::io::Error> for TransportError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::io;

    use super::*;

    #[test]
    fn io_error_shows_cause_only_in_display() {
        let error = TransportError::from(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "パイプが切れました。",
        ));

        assert_eq!(
            error.to_string(),
            "入出力でエラーが発生しました: パイプが切れました。",
            "原因を Display に含める必要があります。"
        );
        assert!(
            error.source().is_none(),
            "原因を Display に含めるので、source として重ねて返さない必要があります。"
        );
    }
}
