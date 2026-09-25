//! 接続の一生 (初期化、イベントの送出、ハプティクスの更新、切断と再接続、停止) の管理。

use std::future::Future;
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{sleep, sleep_until, Instant};
use tracing::{debug, info, warn};

use crate::error::TransportError;
use crate::protocol::{decode, Event, HapticConfig, NotAllowConfigDetector, UNLOCK};
use crate::transport::ble::BleTransport;
use crate::transport::usb::UsbTransport;
use crate::transport::{ConnectionConfig, Incoming, Transport, TransportKind};

/// アンロックの送信完了から、受信の有無にかかわらず Configuring へ進むまでの時間。
const UNLOCK_TIMEOUT: Duration = Duration::from_secs(1);
/// Unlocking で、この時間受信が途切れたら応答が終わったとみなす。
const UNLOCK_QUIET: Duration = Duration::from_millis(200);
/// ハプティクス設定の送信完了から Running へ進むまでの時間。
const CONFIGURE_SETTLE: Duration = Duration::from_millis(200);
/// ハプティクス設定の送信完了から次の送信開始までの最短の間隔。
const HAPTICS_INTERVAL: Duration = Duration::from_millis(50);
/// 再接続の間隔の初期値。待機のたびに 2 倍にし、Running に入ったら初期値に戻す。
const RETRY_INITIAL: Duration = Duration::from_secs(1);
/// 再接続の間隔の上限。
const RETRY_MAX: Duration = Duration::from_secs(30);
/// `DeviceEvent` のチャネルに溜められる数。
const EVENT_CAPACITY: usize = 256;

/// device が送出するイベント。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceEvent {
    /// Running で受信した 1 バイトを復号したもの。未知の値も `Event::Unknown` として送出する。
    Input(Event),
    /// 初期化が完了し、Running に入った。
    Connected,
    /// 接続が切れたか、アンロックが受け付けられず初期化をやり直す。初期化の途中でも送出する。
    Disconnected,
}

/// 接続を開く処理。device は接続と再接続のたびに呼ぶ。
///
/// 返す future は、停止要求で途中で破棄されることがある。
pub trait TransportFactory {
    /// USB の接続を開く。`usb_port` はポート名の明示指定で、`None` なら自動検出する。
    ///
    /// ポートが見つからなければ `TransportError::NotFound` を返す。`Auto` はこのときだけ BLE を試す。
    fn open_usb<'a>(
        &'a self,
        usb_port: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Box<dyn Transport>, TransportError>>;

    /// BLE の接続を開く。
    fn open_ble(&self) -> BoxFuture<'_, Result<Box<dyn Transport>, TransportError>>;
}

/// 実機の USB と BLE の接続を開く。
struct HardwareFactory;

impl TransportFactory for HardwareFactory {
    fn open_usb<'a>(
        &'a self,
        usb_port: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Box<dyn Transport>, TransportError>> {
        let usb_port = usb_port.map(str::to_owned);
        Box::pin(async move {
            let opened =
                tokio::task::spawn_blocking(move || UsbTransport::open(usb_port.as_deref())).await;
            let transport =
                opened.unwrap_or_else(|error| std::panic::resume_unwind(error.into_panic()))?;
            Ok(Box::new(transport) as Box<dyn Transport>)
        })
    }

    fn open_ble(&self) -> BoxFuture<'_, Result<Box<dyn Transport>, TransportError>> {
        Box::pin(async {
            let transport = BleTransport::connect().await?;
            Ok(Box::new(transport) as Box<dyn Transport>)
        })
    }
}

/// 接続の一生を管理するタスクの起動口。
pub struct Device;

impl Device {
    /// 実機の接続を開いて初期化し、イベントを送出するタスクを起動する。
    ///
    /// # Panics
    ///
    /// tokio のランタイムの外で呼んだ場合。
    pub fn run(
        config: ConnectionConfig,
        haptics: HapticConfig,
    ) -> (mpsc::Receiver<DeviceEvent>, DeviceHandle) {
        Self::run_with(config, haptics, HardwareFactory)
    }

    /// `factory` で接続を開いて初期化し、イベントを送出するタスクを起動する。
    ///
    /// # Panics
    ///
    /// tokio のランタイムの外で呼んだ場合。
    pub fn run_with(
        config: ConnectionConfig,
        haptics: HapticConfig,
        factory: impl TransportFactory + Send + Sync + 'static,
    ) -> (mpsc::Receiver<DeviceEvent>, DeviceHandle) {
        let (events_tx, events) = mpsc::channel(EVENT_CAPACITY);
        let (haptics_tx, haptics_rx) = watch::channel(haptics);
        let (stop, stop_rx) = oneshot::channel();
        let task = tokio::spawn(run_device(factory, config, events_tx, haptics_rx, stop_rx));
        (
            events,
            DeviceHandle {
                haptics: haptics_tx,
                stop,
                task,
            },
        )
    }
}

/// 実行中の device を操作するハンドル。破棄すると停止要求を出す (停止の完了は待たない)。
#[derive(Debug)]
pub struct DeviceHandle {
    haptics: watch::Sender<HapticConfig>,
    /// 送出口を捨てると停止要求になる。
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl DeviceHandle {
    /// ハプティクス設定の更新を要求する。すぐ戻り、送信は device のタスクが行う。
    pub fn set_haptics(&self, config: HapticConfig) {
        self.haptics.send_replace(config);
    }

    /// 停止を要求し、接続を閉じてタスクが終わるまで待つ。
    ///
    /// 再接続の待機、初期化、送信のどの途中でも、進行中の処理を待たずに破棄して停止する。
    pub async fn shutdown(self) {
        let Self { stop, task, .. } = self;
        drop(stop);
        if let Err(error) = task.await {
            if error.is_panic() {
                std::panic::resume_unwind(error.into_panic());
            }
        }
    }
}

/// 接続を開いて初期化とイベントの送出を続け、切れたら間隔をあけて開き直す。停止要求で終わる。
async fn run_device<F: TransportFactory>(
    factory: F,
    config: ConnectionConfig,
    events: mpsc::Sender<DeviceEvent>,
    mut haptics: watch::Receiver<HapticConfig>,
    mut stop: oneshot::Receiver<()>,
) {
    let mut retry_delay = RETRY_INITIAL;
    loop {
        let Some(opened) = until_stopped(&mut stop, open(&factory, &config)).await else {
            return;
        };
        match opened {
            Ok(mut transport) => {
                let receiver = transport
                    .take_receiver()
                    .expect("開いた接続から受信チャネルを取り出せませんでした。");
                let mut session = Session::new(&mut *transport, receiver, &mut haptics, &events);
                let stopped = until_stopped(&mut stop, session.run(&mut retry_delay))
                    .await
                    .is_none();
                if let Err(error) = transport.close().await {
                    warn!("接続を閉じる処理でエラーが発生しました: {error}");
                }
                if stopped {
                    return;
                }
            }
            Err(error) => warn!("TourBox に接続できませんでした: {error}"),
        }
        info!("{} 秒後に再接続を試みます。", retry_delay.as_secs());
        if until_stopped(&mut stop, sleep(retry_delay)).await.is_none() {
            return;
        }
        retry_delay = (retry_delay * 2).min(RETRY_MAX);
    }
}

/// 停止要求が先に来たら `future` を破棄して `None` を返す。
async fn until_stopped<T>(
    stop: &mut oneshot::Receiver<()>,
    future: impl Future<Output = T>,
) -> Option<T> {
    tokio::select! {
        biased;
        _ = stop => {
            info!("停止要求を受け付けました。");
            None
        }
        output = future => Some(output),
    }
}

/// 設定の接続方式で接続を開く。
async fn open<F: TransportFactory>(
    factory: &F,
    config: &ConnectionConfig,
) -> Result<Box<dyn Transport>, TransportError> {
    let usb_port = config.usb_port.as_deref();
    match config.transport {
        TransportKind::Usb => factory.open_usb(usb_port).await,
        TransportKind::Ble => factory.open_ble().await,
        TransportKind::Auto => match factory.open_usb(usb_port).await {
            Err(TransportError::NotFound) => factory.open_ble().await,
            result => result,
        },
    }
}

/// 初期化やイベントの送出を中断する理由。
enum Interrupt {
    /// 接続が切れた。
    Lost,
    /// アンロックが受け付けられていない (`NOT_ALLOW_CONFIG` を受信した)。
    Rejected,
}

/// 1 本の接続の上で、初期化とイベントの送出を繰り返す。
struct Session<'a> {
    outbound: Outbound<'a>,
    inbound: Inbound<'a>,
}

/// 接続への送信。
struct Outbound<'a> {
    transport: &'a mut dyn Transport,
    haptics: &'a mut watch::Receiver<HapticConfig>,
    /// この接続で最後に送信を完了したハプティクス設定と、その完了時刻。
    last_haptics: Option<(HapticConfig, Instant)>,
}

/// 接続からの受信と、イベントの送出。
struct Inbound<'a> {
    receiver: mpsc::Receiver<Incoming>,
    detector: NotAllowConfigDetector,
    events: &'a mpsc::Sender<DeviceEvent>,
    /// 受信のログに添える経過時間の起点 (アンロックの送信完了)。
    unlocked_at: Instant,
}

impl<'a> Session<'a> {
    fn new(
        transport: &'a mut dyn Transport,
        receiver: mpsc::Receiver<Incoming>,
        haptics: &'a mut watch::Receiver<HapticConfig>,
        events: &'a mpsc::Sender<DeviceEvent>,
    ) -> Self {
        Self {
            outbound: Outbound {
                transport,
                haptics,
                last_haptics: None,
            },
            inbound: Inbound {
                receiver,
                detector: NotAllowConfigDetector::default(),
                events,
                unlocked_at: Instant::now(),
            },
        }
    }

    /// 接続が切れるまで、初期化とイベントの送出を続ける。Running に入るたびに再接続の間隔を戻す。
    async fn run(&mut self, retry_delay: &mut Duration) {
        loop {
            let interrupt = self.initialize_and_forward(retry_delay).await;
            emit(self.inbound.events, DeviceEvent::Disconnected).await;
            match interrupt {
                Interrupt::Rejected => {
                    warn!("アンロックが受け付けられていません (<!not_allow_config!> を受信しました)。初期化をやり直します。");
                }
                Interrupt::Lost => {
                    warn!("TourBox との接続が切れました。");
                    return;
                }
            }
        }
    }

    /// 初期化し、Running に入ったらイベントを送出し続ける。
    async fn initialize_and_forward(&mut self, retry_delay: &mut Duration) -> Interrupt {
        if let Err(interrupt) = self.unlock().await {
            return interrupt;
        }
        if let Err(interrupt) = self.configure().await {
            return interrupt;
        }
        info!("初期化が完了しました。");
        *retry_delay = RETRY_INITIAL;
        emit(self.inbound.events, DeviceEvent::Connected).await;
        tokio::select! {
            interrupt = self.inbound.forward_events() => interrupt,
            interrupt = self.outbound.keep_haptics_updated() => interrupt,
        }
    }

    /// Unlocking: アンロックを送り、応答が途切れるか打ち切りの時間まで受信を捨てる。
    async fn unlock(&mut self) -> Result<(), Interrupt> {
        self.inbound.detector.reset();
        self.outbound.send(&UNLOCK).await?;
        let sent_at = Instant::now();
        self.inbound.unlocked_at = sent_at;
        info!("アンロックを送信しました。");

        let give_up = sent_at + UNLOCK_TIMEOUT;
        let mut deadline = give_up;
        let mut received = false;
        while let Some(chunk) = self.inbound.next_chunk(deadline).await? {
            self.inbound.log_received("Unlocking", &chunk);
            // Unlocking では検出しても無視し、一致の途中経過だけを次の状態へ持ち越す
            self.inbound.detector.feed(&chunk);
            received = true;
            deadline = give_up.min(Instant::now() + UNLOCK_QUIET);
        }
        if !received {
            info!("アンロックの応答を受信しませんでした。");
        }
        Ok(())
    }

    /// Configuring: 現在のハプティクス設定を送り、一定時間受信を捨てる。
    async fn configure(&mut self) -> Result<(), Interrupt> {
        self.outbound.send_current_haptics().await?;
        info!("ハプティクス設定を送信しました。");

        let deadline = Instant::now() + CONFIGURE_SETTLE;
        while let Some(chunk) = self.inbound.next_chunk(deadline).await? {
            self.inbound.log_received("Configuring", &chunk);
            if self.inbound.detector.feed(&chunk) {
                return Err(Interrupt::Rejected);
            }
        }
        Ok(())
    }
}

impl Outbound<'_> {
    /// `bytes` を送る。送信の失敗は切断として扱う。
    async fn send(&mut self, bytes: &[u8]) -> Result<(), Interrupt> {
        self.transport.send(bytes).await.map_err(|error| {
            warn!("送信でエラーが発生しました: {error}");
            Interrupt::Lost
        })
    }

    /// 現在のハプティクス設定を送る。
    async fn send_current_haptics(&mut self) -> Result<(), Interrupt> {
        let config = self.haptics.borrow_and_update().clone();
        self.send_haptics(config).await
    }

    /// `config` を送り、この接続で最後に送った内容として完了時刻とともに記録する。
    async fn send_haptics(&mut self, config: HapticConfig) -> Result<(), Interrupt> {
        self.send(&config.encode()).await?;
        self.last_haptics = Some((config, Instant::now()));
        Ok(())
    }

    /// Running: ハプティクス設定の更新を待ち、直前に送った内容と違えば送り続ける。
    async fn keep_haptics_updated(&mut self) -> Interrupt {
        loop {
            if self.haptics.changed().await.is_err() {
                // ハンドルが破棄された。停止要求でこの future ごと破棄されるまで待つ
                std::future::pending::<()>().await;
            }
            if let Some((_, sent_at)) = &self.last_haptics {
                let ready_at = *sent_at + HAPTICS_INTERVAL;
                if Instant::now() < ready_at {
                    sleep_until(ready_at).await;
                }
            }
            let config = self.haptics.borrow_and_update().clone();
            if matches!(&self.last_haptics, Some((last, _)) if *last == config) {
                debug!("直前に送った内容と同じなので、ハプティクス設定を送りません。");
                continue;
            }
            if let Err(interrupt) = self.send_haptics(config).await {
                return interrupt;
            }
            debug!("ハプティクス設定を送信しました。");
        }
    }
}

impl Inbound<'_> {
    /// `deadline` までに届いたかたまりを返す。届かなければ `None` を返す。
    async fn next_chunk(&mut self, deadline: Instant) -> Result<Option<Vec<u8>>, Interrupt> {
        tokio::select! {
            biased;
            incoming = self.receiver.recv() => chunk_or_lost(incoming).map(Some),
            () = sleep_until(deadline) => Ok(None),
        }
    }

    /// Running: 受信を 1 バイトずつ復号して送出し続ける。
    async fn forward_events(&mut self) -> Interrupt {
        loop {
            let chunk = match chunk_or_lost(self.receiver.recv().await) {
                Ok(chunk) => chunk,
                Err(interrupt) => return interrupt,
            };
            self.log_received("Running", &chunk);
            if self.detector.feed(&chunk) {
                return Interrupt::Rejected;
            }
            for &byte in &chunk {
                emit(self.events, DeviceEvent::Input(decode(byte))).await;
            }
        }
    }

    /// 受信したかたまりを、状態とアンロックの送信完了からの経過時間とともに debug ログに出す。
    fn log_received(&self, state: &str, chunk: &[u8]) {
        debug!(
            state,
            since_unlock = ?self.unlocked_at.elapsed(),
            bytes = format_args!("{chunk:02x?}"),
            "受信しました。"
        );
    }
}

/// 受信チャネルから取り出したものを、かたまりか切断に分ける。
fn chunk_or_lost(incoming: Option<Incoming>) -> Result<Vec<u8>, Interrupt> {
    match incoming {
        Some(Incoming::Data(chunk)) => Ok(chunk),
        Some(Incoming::Disconnected) => Err(Interrupt::Lost),
        None => {
            warn!("切断の通知なしに受信チャネルが閉じました。切断として扱います。");
            Err(Interrupt::Lost)
        }
    }
}

/// イベントを送出する。受け取り側が破棄されていれば捨てる。
async fn emit(events: &mpsc::Sender<DeviceEvent>, event: DeviceEvent) {
    if events.send(event).await.is_err() {
        debug!("イベントの受け取り側がないため、イベントを破棄しました。");
    }
}
