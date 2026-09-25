//! テストから操作できるメモリ内の接続。

use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::mpsc;

use super::{Incoming, Transport};
use crate::error::TransportError;

/// 1 本の接続の受信チャネルに溜められる数。テストが受信を待たずに注入できるかたまりの上限になる。
const CHANNEL_CAPACITY: usize = 1024;

/// フェイクの接続を作り、テストから操作するハンドル。複製しても同じ状態を共有する。
///
/// 受信の注入と切断は最後に作った接続に対して行う。
/// 送信の記録と `close` の回数は、作ったすべての接続の合計である。
#[derive(Debug, Clone, Default)]
pub struct FakeHandle {
    state: Arc<Mutex<State>>,
}

/// device に渡すフェイクの接続。`FakeHandle::connect` で作る。
#[derive(Debug)]
pub struct FakeTransport {
    state: Arc<Mutex<State>>,
    /// `State::links` の中でこの接続が占める位置。
    link: usize,
    receiver: Option<mpsc::Receiver<Incoming>>,
}

#[derive(Debug, Default)]
struct State {
    sent: Vec<Vec<u8>>,
    send_delay: Duration,
    close_count: usize,
    /// 作った順の接続ごとの、受信チャネルへの送出口。切断か `close` の後は `None`。
    links: Vec<Option<mpsc::Sender<Incoming>>>,
}

impl FakeHandle {
    /// 接続をまだ持たないハンドルを作る。
    pub fn new() -> Self {
        Self::default()
    }

    /// 新しい接続を作る。以後の注入と切断はこの接続が対象になる。
    pub fn connect(&self) -> FakeTransport {
        let (sender, receiver) = mpsc::channel(CHANNEL_CAPACITY);
        let mut state = lock(&self.state);
        state.links.push(Some(sender));
        FakeTransport {
            state: Arc::clone(&self.state),
            link: state.links.len() - 1,
            receiver: Some(receiver),
        }
    }

    /// 最後に作った接続の受信チャネルへ、受信したかたまりとして `chunk` を届ける。
    ///
    /// # Panics
    ///
    /// 接続がない、最後の接続が切断済みか閉じられている、受信チャネルが満杯か破棄されている場合。
    pub fn inject(&self, chunk: &[u8]) {
        let state = lock(&self.state);
        let sender = state
            .links
            .last()
            .and_then(Option::as_ref)
            .expect("受信を届けられる接続がありません。");
        deliver(sender, Incoming::Data(chunk.to_vec()));
    }

    /// 最後に作った接続を切断する。
    ///
    /// 受信チャネルに `Incoming::Disconnected` を届けてから閉じ、以後の `send` はエラーを返す。
    ///
    /// # Panics
    ///
    /// 接続がない、最後の接続が切断済みか閉じられている、受信チャネルが満杯か破棄されている場合。
    pub fn disconnect(&self) {
        let sender = lock(&self.state)
            .links
            .last_mut()
            .and_then(Option::take)
            .expect("切断できる接続がありません。");
        deliver(&sender, Incoming::Disconnected);
    }

    /// 送信が完了したバイト列を完了した順に返す。
    pub fn sent(&self) -> Vec<Vec<u8>> {
        lock(&self.state).sent.clone()
    }

    /// 以後の `send` が完了するまでの時間を指定する。既定は 0 で、`send` は待たずに完了する。
    pub fn set_send_delay(&self, delay: Duration) {
        lock(&self.state).send_delay = delay;
    }

    /// `close` が呼ばれた回数を返す。
    pub fn close_count(&self) -> usize {
        lock(&self.state).close_count
    }
}

impl Transport for FakeTransport {
    /// 指定された送信時間だけ待ち、その時点で接続が生きていれば記録して成功する。
    fn send<'a>(&'a mut self, data: &'a [u8]) -> BoxFuture<'a, Result<(), TransportError>> {
        Box::pin(async move {
            let delay = lock(&self.state).send_delay;
            // sleep は 0 でも期限を次のミリ秒境界へ切り上げて待つことがあるので呼ばない
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let mut state = lock(&self.state);
            if state.links[self.link].is_none() {
                return Err(io::Error::from(io::ErrorKind::NotConnected).into());
            }
            state.sent.push(data.to_vec());
            Ok(())
        })
    }

    fn take_receiver(&mut self) -> Option<mpsc::Receiver<Incoming>> {
        self.receiver.take()
    }

    /// 呼び出しを記録し、受信チャネルを閉じる。
    fn close(&mut self) -> BoxFuture<'_, Result<(), TransportError>> {
        Box::pin(async move {
            let mut state = lock(&self.state);
            state.close_count += 1;
            state.links[self.link] = None;
            Ok(())
        })
    }
}

/// 状態を読み書きするためにロックする。
fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    // 状態を変更する途中でパニックする箇所はないので、汚染されていてもそのまま使う
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 受信チャネルへ待たずに届ける。
fn deliver(sender: &mpsc::Sender<Incoming>, incoming: Incoming) {
    sender
        .try_send(incoming)
        .expect("受信チャネルへ届けられませんでした。");
}
