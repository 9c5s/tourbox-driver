//! BLE (Bluetooth Low Energy) の接続。

use std::collections::BTreeSet;
use std::future::Future;
use std::panic;
use std::pin::Pin;
use std::time::Duration;

use btleplug::api::{
    Central, CentralEvent, CentralState, Characteristic, Manager as _, Peripheral as _, ScanFilter,
    ValueNotification, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral, PeripheralId};
use futures::future::BoxFuture;
use futures::stream::{Stream, StreamExt};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, OnceCell};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tracing::{info, warn};

use super::{Incoming, Transport};
use crate::error::TransportError;

/// 広告名がこれで始まる機器を TourBox とみなす。
const NAME_PREFIX: &str = "TourBox";
/// TourBox のサービスの UUID。
const SERVICE_UUID: u128 = 0x0000fff0_0000_1000_8000_00805f9b34fb;
/// 通知用キャラクタリスティックの UUID。
const NOTIFY_UUID: u128 = 0x0000fff1_0000_1000_8000_00805f9b34fb;
/// 書き込み用キャラクタリスティックの UUID。
const WRITE_UUID: u128 = 0x0000fff2_0000_1000_8000_00805f9b34fb;
/// 1 回の書き込みで送る最大のバイト数。
const FRAME_SIZE: usize = 20;
/// 分割した書き込みの、前の書き込みの完了から次の書き込みの開始までの間隔。
const FRAME_INTERVAL: Duration = Duration::from_millis(10);
/// TourBox を探すスキャンの長さ。
const SCAN_TIMEOUT: Duration = Duration::from_secs(10);
/// スキャン中に、見つかった機器の一覧を確かめ直す間隔。
const SCAN_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// アダプタの取得、スキャンの開始、接続、サービスの探索、購読などの打ち切り時間。
const OPERATION_TIMEOUT: Duration = Duration::from_secs(10);
/// 分割した 1 回の書き込みの打ち切り時間。
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);
/// 後始末 (スキャンの停止、購読の解除、切断) の各段階の打ち切り時間。
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
/// 受信チャネルに溜められるかたまりの数。
const CHANNEL_CAPACITY: usize = 256;

/// プロセスで共有するアダプタ。macOS では取得のたびに CoreBluetooth のスレッドが残るので、取得は 1 回にする。
static ADAPTER: OnceCell<Adapter> = OnceCell::const_new();

/// 通知のストリーム。
type Notifications = Pin<Box<dyn Stream<Item = ValueNotification> + Send>>;
/// アダプタのイベント (機器の発見や切断) のストリーム。
type CentralEvents = Pin<Box<dyn Stream<Item = CentralEvent> + Send>>;

/// TourBox との BLE の接続。
pub struct BleTransport {
    /// 開いている接続。`close` の後は `None`。
    link: Option<Link>,
    receiver: Option<mpsc::Receiver<Incoming>>,
}

/// 開いている接続の部品。
struct Link {
    peripheral: Peripheral,
    notify: Characteristic,
    write: Characteristic,
    /// 通知と切断を受信チャネルへ送るタスク。切断を検知すると終わる。
    receiving: JoinHandle<()>,
}

impl BleTransport {
    /// 広告名が "TourBox" で始まる機器を最大 10 秒スキャンして接続し、通知を購読する。
    ///
    /// 見つからなければ `TransportError::NotFound` を返す。
    /// アダプタを使えない場合 (macOS で Bluetooth の権限がない場合を含む) と、接続の各段階の失敗や時間切れは
    /// `TransportError::Ble` を返す。
    /// 返す future を途中で破棄すると、スキャンの停止と接続の取り消しを別のタスクで行う。
    /// アダプタはプロセスで共有するので、複数の `connect` を同時に呼ばない前提である。
    pub async fn connect() -> Result<Self, TransportError> {
        let adapter = shared_adapter().await?;
        check_state(&adapter).await?;
        let (peripheral, name) = scan(&adapter).await?;
        info!(name = %name, id = %peripheral.id(), "TourBox を見つけました。");
        let disconnect = CleanupOnDrop::new(disconnect(peripheral.clone()));
        match open(&adapter, &peripheral).await {
            Ok(transport) => {
                disconnect.disarm();
                Ok(transport)
            }
            Err(error) => {
                disconnect.run().await;
                Err(error)
            }
        }
    }

    /// 送信に使う接続を返す。閉じた後や切断の後はエラーを返す。
    fn link(&self) -> Result<&Link, TransportError> {
        match &self.link {
            Some(link) if !link.receiving.is_finished() => Ok(link),
            _ => Err(TransportError::Ble(
                "BLE の接続は閉じているか、切断されています。".to_owned(),
            )),
        }
    }
}

impl Transport for BleTransport {
    fn send<'a>(&'a mut self, data: &'a [u8]) -> BoxFuture<'a, Result<(), TransportError>> {
        Box::pin(async move {
            let link = self.link()?;
            write_in_frames(data, move |frame| {
                within(
                    WRITE_TIMEOUT,
                    "書き込み",
                    link.peripheral
                        .write(&link.write, frame, WriteType::WithoutResponse),
                )
            })
            .await
        })
    }

    fn take_receiver(&mut self) -> Option<mpsc::Receiver<Incoming>> {
        self.receiver.take()
    }

    fn close(&mut self) -> BoxFuture<'_, Result<(), TransportError>> {
        Box::pin(async move {
            let Some(Link {
                peripheral,
                notify,
                receiving,
                ..
            }) = self.link.take()
            else {
                return Ok(());
            };
            let lost = receiving.is_finished();
            receiving.abort();
            if let Err(error) = receiving.await {
                if error.is_panic() {
                    panic::resume_unwind(error.into_panic());
                }
            }
            // 切断の後は購読も消えているので、解除しない
            if !lost {
                if let Err(error) = within(
                    CLEANUP_TIMEOUT,
                    "通知の購読の解除",
                    peripheral.unsubscribe(&notify),
                )
                .await
                {
                    warn!("{error}");
                }
            }
            disconnect(peripheral).await;
            Ok(())
        })
    }
}

/// 共有のアダプタを返す。まだ取得していなければ取得する。取得に失敗した場合は次の呼び出しで取得し直す。
async fn shared_adapter() -> Result<Adapter, TransportError> {
    ADAPTER.get_or_try_init(acquire_adapter).await.cloned()
}

/// 最初の Bluetooth のアダプタを取得する。
async fn acquire_adapter() -> Result<Adapter, TransportError> {
    let adapters = within(OPERATION_TIMEOUT, "Bluetooth のアダプタの取得", async {
        Manager::new().await?.adapters().await
    })
    .await
    .inspect_err(|_| guide_bluetooth_setup())?;
    adapters.into_iter().next().ok_or_else(|| {
        guide_bluetooth_setup();
        TransportError::Ble("Bluetooth のアダプタが見つかりません。".to_owned())
    })
}

/// アダプタの状態を確かめ、電源が入っていないか権限がない可能性があればログで案内する。
///
/// 使えるかどうかの判断はスキャンの結果に任せ、状態ではエラーにしない。
async fn check_state(adapter: &Adapter) -> Result<(), TransportError> {
    let state = within(
        OPERATION_TIMEOUT,
        "Bluetooth の状態の取得",
        adapter.adapter_state(),
    )
    .await?;
    if state != CentralState::PoweredOn {
        warn!(state = ?state, "Bluetooth が使用できる状態ではない可能性があります。");
        guide_bluetooth_setup();
    }
    Ok(())
}

/// Bluetooth の状態と macOS の権限を確かめるよう、warn ログで案内する。
fn guide_bluetooth_setup() {
    warn!(
        "Bluetooth がオンになっていることを確認してください。macOS では、ターミナルアプリに Bluetooth の使用を許可する必要があります。README の「macOS の Bluetooth 権限」の手順を参照してください。"
    );
}

/// スキャンして、広告名が TourBox で始まる機器を探す。見つからなければ `TransportError::NotFound` を返す。
async fn scan(adapter: &Adapter) -> Result<(Peripheral, String), TransportError> {
    // 以前のスキャンで見つけた機器を捨て、いま広告している機器だけを候補にする
    within(
        OPERATION_TIMEOUT,
        "以前のスキャン結果の消去",
        adapter.clear_peripherals(),
    )
    .await?;
    let stop = CleanupOnDrop::new(stop_scan(adapter.clone()));
    let found = async {
        within(
            OPERATION_TIMEOUT,
            "スキャンの開始",
            adapter.start_scan(ScanFilter::default()),
        )
        .await?;
        info!("BLE のスキャンを開始しました。");
        timeout(SCAN_TIMEOUT, find_tourbox(adapter))
            .await
            .unwrap_or(Err(TransportError::NotFound))
    }
    .await;
    stop.run().await;
    found
}

/// 見つかった機器の一覧を一定の間隔で確かめ、広告名が TourBox で始まる機器とその名前を返す。
async fn find_tourbox(adapter: &Adapter) -> Result<(Peripheral, String), TransportError> {
    loop {
        let peripherals = adapter
            .peripherals()
            .await
            .map_err(|error| failed("機器の一覧の取得", error))?;
        for peripheral in peripherals {
            let properties = peripheral
                .properties()
                .await
                .map_err(|error| failed("機器の情報の取得", error))?;
            if let Some(name) = properties
                .and_then(|properties| properties.local_name)
                .filter(|name| is_tourbox_name(name))
            {
                return Ok((peripheral, name));
            }
        }
        sleep(SCAN_POLL_INTERVAL).await;
    }
}

/// スキャンを止める。失敗はログに出すだけにする。
async fn stop_scan(adapter: Adapter) {
    if let Err(error) = within(CLEANUP_TIMEOUT, "スキャンの停止", adapter.stop_scan()).await
    {
        warn!("{error}");
    }
}

/// 見つけた TourBox に接続し、キャラクタリスティックを特定して通知を購読する。
async fn open(adapter: &Adapter, peripheral: &Peripheral) -> Result<BleTransport, TransportError> {
    within(OPERATION_TIMEOUT, "接続", peripheral.connect()).await?;
    // 以後の切断を取りこぼさないよう、接続の直後から監視する
    let events = within(OPERATION_TIMEOUT, "切断の監視の開始", adapter.events()).await?;
    info!("TourBox に BLE で接続しました。");
    within(
        OPERATION_TIMEOUT,
        "サービスの探索",
        peripheral.discover_services(),
    )
    .await?;
    let characteristics = peripheral.characteristics();
    let notify = find_characteristic(&characteristics, NOTIFY_UUID)
        .ok_or_else(|| missing_characteristic("通知用 (fff1)"))?;
    let write = find_characteristic(&characteristics, WRITE_UUID)
        .ok_or_else(|| missing_characteristic("書き込み用 (fff2)"))?;
    let notifications = within(
        OPERATION_TIMEOUT,
        "通知の受け取りの準備",
        peripheral.notifications(),
    )
    .await?;
    within(
        OPERATION_TIMEOUT,
        "通知の購読",
        peripheral.subscribe(&notify),
    )
    .await?;
    info!("通知を購読しました。");

    let (sender, receiver) = mpsc::channel(CHANNEL_CAPACITY);
    let receiving = tokio::spawn(forward(notifications, events, peripheral.id(), sender));
    Ok(BleTransport {
        link: Some(Link {
            peripheral: peripheral.clone(),
            notify,
            write,
            receiving,
        }),
        receiver: Some(receiver),
    })
}

/// キャラクタリスティックが見つからないエラーを作る。
fn missing_characteristic(role: &str) -> TransportError {
    TransportError::Ble(format!(
        "サービス fff0 に{role}のキャラクタリスティックが見つかりません。"
    ))
}

/// 通知を受信チャネルへ送り続ける。切断を検知したら `Incoming::Disconnected` を送って終わる。
async fn forward(
    mut notifications: Notifications,
    mut events: CentralEvents,
    id: PeripheralId,
    sender: mpsc::Sender<Incoming>,
) {
    loop {
        tokio::select! {
            // 切断の前に届いていた通知を先に送る
            biased;
            notification = notifications.next() => {
                let Some(notification) = notification else {
                    warn!("BLE の通知のストリームが終わりました。切断として扱います。");
                    break;
                };
                if sender.send(Incoming::Data(notification.value)).await.is_err() {
                    return;
                }
            }
            event = events.next() => match event {
                Some(CentralEvent::DeviceDisconnected(disconnected)) if disconnected == id => {
                    warn!("TourBox との BLE の接続が切れました。");
                    break;
                }
                Some(_) => {}
                None => {
                    warn!("BLE のイベントのストリームが終わりました。切断として扱います。");
                    break;
                }
            },
        }
    }
    let _ = sender.send(Incoming::Disconnected).await;
}

/// 接続を切るか、接続の途中なら取り消す。失敗はログに出すだけにする。
async fn disconnect(peripheral: Peripheral) {
    match within(CLEANUP_TIMEOUT, "切断", peripheral.disconnect()).await {
        Ok(()) => info!("TourBox との BLE の接続を切りました。"),
        Err(error) => warn!("{error}"),
    }
}

/// `operation` を `limit` で打ち切る。失敗と時間切れは、操作の名前を添えて `TransportError::Ble` にする。
async fn within<T>(
    limit: Duration,
    action: &str,
    operation: impl Future<Output = btleplug::Result<T>>,
) -> Result<T, TransportError> {
    match timeout(limit, operation).await {
        Ok(result) => result.map_err(|error| failed(action, error)),
        Err(_) => Err(TransportError::Ble(format!(
            "{action}が {} 秒以内に完了しませんでした。",
            limit.as_secs_f64()
        ))),
    }
}

/// btleplug のエラーを、失敗した操作の名前を添えて `TransportError::Ble` にする。
fn failed(action: &str, error: btleplug::Error) -> TransportError {
    TransportError::Ble(format!("{action}に失敗しました: {error}"))
}

/// 破棄されると、保持している後始末を別のタスクで実行する。`connect` の future が途中で破棄された場合に使う。
struct CleanupOnDrop(Option<BoxFuture<'static, ()>>);

impl CleanupOnDrop {
    fn new(cleanup: impl Future<Output = ()> + Send + 'static) -> Self {
        Self(Some(Box::pin(cleanup)))
    }

    /// 後始末をこの場で実行する。実行中に破棄された場合は、残りを別のタスクで実行する。
    async fn run(mut self) {
        if let Some(cleanup) = &mut self.0 {
            cleanup.await;
        }
        self.0 = None;
    }

    /// 後始末を実行せずに捨てる。
    fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for CleanupOnDrop {
    fn drop(&mut self) {
        if let (Some(cleanup), Ok(runtime)) = (self.0.take(), Handle::try_current()) {
            runtime.spawn(cleanup);
        }
    }
}

/// 広告名が TourBox のものか判定する。
fn is_tourbox_name(name: &str) -> bool {
    name.starts_with(NAME_PREFIX)
}

/// TourBox のサービスにある `uuid` のキャラクタリスティックを探す。UUID は 128 ビットの完全一致で比べる。
fn find_characteristic(
    characteristics: &BTreeSet<Characteristic>,
    uuid: u128,
) -> Option<Characteristic> {
    characteristics
        .iter()
        .find(|characteristic| {
            characteristic.service_uuid.as_u128() == SERVICE_UUID
                && characteristic.uuid.as_u128() == uuid
        })
        .cloned()
}

/// 1 回の書き込みで送れる長さ (20 バイト) ずつに分ける。
fn split_frames(data: &[u8]) -> std::slice::Chunks<'_, u8> {
    data.chunks(FRAME_SIZE)
}

/// `data` を分割して `write` で順に書く。前の書き込みの完了から 10 ms 待って次を書き、最後の書き込みが終わったら戻る。
async fn write_in_frames<'a, W, F>(data: &'a [u8], mut write: W) -> Result<(), TransportError>
where
    W: FnMut(&'a [u8]) -> F,
    F: Future<Output = Result<(), TransportError>>,
{
    for (index, frame) in split_frames(data).enumerate() {
        if index > 0 {
            sleep(FRAME_INTERVAL).await;
        }
        write(frame).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use btleplug::api::CharPropFlags;
    use tokio::time::Instant;

    use super::*;
    use crate::protocol::{HapticConfig, UNLOCK};

    const SERVICE: &str = "0000fff0-0000-1000-8000-00805f9b34fb";
    const NOTIFY: &str = "0000fff1-0000-1000-8000-00805f9b34fb";
    const WRITE: &str = "0000fff2-0000-1000-8000-00805f9b34fb";
    /// 先頭の 32 ビットだけが通知用と同じで、残りが Bluetooth の基底 UUID と異なる UUID。
    const NOTIFY_LOOKALIKE: &str = "0000fff1-0000-0000-0000-000000000000";
    /// TourBox とは別のサービス (Device Information)。
    const OTHER_SERVICE: &str = "0000180a-0000-1000-8000-00805f9b34fb";

    fn characteristic(service: &str, uuid: &str) -> Characteristic {
        Characteristic {
            uuid: uuid.parse().unwrap(),
            service_uuid: service.parse().unwrap(),
            properties: CharPropFlags::empty(),
            descriptors: BTreeSet::new(),
        }
    }

    fn millis(values: &[u64]) -> Vec<Duration> {
        values.iter().copied().map(Duration::from_millis).collect()
    }

    #[test]
    fn haptic_config_is_split_into_four_20_byte_frames_and_14_byte_rest() {
        let message = HapticConfig::default().encode();

        let frames: Vec<&[u8]> = split_frames(&message).collect();

        assert_eq!(
            frames.iter().map(|frame| frame.len()).collect::<Vec<_>>(),
            [20, 20, 20, 20, 14],
            "94 バイトは 20、20、20、20、14 バイトに分ける必要があります。"
        );
        assert_eq!(
            frames.concat(),
            message,
            "分割した順につなぐと元のバイト列に戻る必要があります。"
        );
    }

    #[test]
    fn data_up_to_20_bytes_is_single_frame() {
        let twenty = [0xab; 20];

        assert_eq!(
            split_frames(&UNLOCK).collect::<Vec<_>>(),
            [&UNLOCK[..]],
            "20 バイト未満は分割しない必要があります。"
        );
        assert_eq!(
            split_frames(&twenty).collect::<Vec<_>>(),
            [&twenty[..]],
            "20 バイトちょうどは分割しない必要があります。"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn frames_are_written_10ms_apart_and_returns_after_last_write() {
        let message = HapticConfig::default().encode();
        let start = Instant::now();
        let mut writes = Vec::new();

        let result = write_in_frames(&message, |frame| {
            writes.push((start.elapsed(), frame.to_vec()));
            async { Ok(()) }
        })
        .await;

        assert!(
            result.is_ok(),
            "書き込みは成功する必要があります: {result:?}"
        );
        assert_eq!(
            writes.iter().map(|(at, _)| *at).collect::<Vec<_>>(),
            millis(&[0, 10, 20, 30, 40]),
            "分割は 10 ms 間隔で書く必要があります。"
        );
        assert_eq!(
            writes
                .into_iter()
                .map(|(_, frame)| frame)
                .collect::<Vec<_>>(),
            [
                &message[..20],
                &message[20..40],
                &message[40..60],
                &message[60..80],
                &message[80..],
            ],
            "先頭から 20 バイトずつ順に書く必要があります。"
        );
        assert_eq!(
            start.elapsed(),
            Duration::from_millis(40),
            "最後の分割を書き終えたら、待たずに戻る必要があります。"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn interval_is_counted_from_completion_of_previous_write() {
        let start = Instant::now();
        let mut started = Vec::new();

        let result = write_in_frames(&[0; 45], |_| {
            started.push(start.elapsed());
            async {
                sleep(Duration::from_millis(5)).await;
                Ok(())
            }
        })
        .await;

        assert!(
            result.is_ok(),
            "書き込みは成功する必要があります: {result:?}"
        );
        assert_eq!(
            started,
            millis(&[0, 15, 30]),
            "前の書き込みの完了から 10 ms 後に次の書き込みを始める必要があります。"
        );
        assert_eq!(
            start.elapsed(),
            Duration::from_millis(35),
            "最後の書き込みが終わるまで戻らない必要があります。"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stops_writing_at_first_failed_frame() {
        let start = Instant::now();
        let mut writes = 0;

        let result = write_in_frames(&[0; 94], |_| {
            writes += 1;
            let failed = writes == 2;
            async move {
                if failed {
                    Err(TransportError::Ble("書き込みに失敗しました。".to_owned()))
                } else {
                    Ok(())
                }
            }
        })
        .await;

        assert!(
            matches!(result, Err(TransportError::Ble(_))),
            "書き込みの失敗をそのまま返す必要があります: {result:?}"
        );
        assert_eq!(writes, 2, "失敗した分割より後は書かない必要があります。");
        assert_eq!(
            start.elapsed(),
            Duration::from_millis(10),
            "失敗したら待たずに戻る必要があります。"
        );
    }

    #[test]
    fn names_starting_with_tourbox_are_tourbox() {
        for name in ["TourBox", "TourBox Elite", "TourBoxElite"] {
            assert!(
                is_tourbox_name(name),
                "{name:?} は TourBox とみなす必要があります。"
            );
        }
    }

    #[test]
    fn other_names_are_not_tourbox() {
        for name in ["", "Tour", "tourbox Elite", "TOURBOX", "My TourBox"] {
            assert!(
                !is_tourbox_name(name),
                "{name:?} は TourBox とみなさない必要があります。"
            );
        }
    }

    #[test]
    fn finds_characteristics_by_full_uuid_within_tourbox_service() {
        let notify = characteristic(SERVICE, NOTIFY);
        let write = characteristic(SERVICE, WRITE);
        let characteristics = BTreeSet::from([
            characteristic(OTHER_SERVICE, NOTIFY),
            characteristic(SERVICE, NOTIFY_LOOKALIKE),
            notify.clone(),
            write.clone(),
        ]);

        assert_eq!(
            find_characteristic(&characteristics, NOTIFY_UUID),
            Some(notify),
            "サービス fff0 の通知用キャラクタリスティック fff1 を選ぶ必要があります。"
        );
        assert_eq!(
            find_characteristic(&characteristics, WRITE_UUID),
            Some(write),
            "サービス fff0 の書き込み用キャラクタリスティック fff2 を選ぶ必要があります。"
        );
    }

    #[test]
    fn finds_nothing_when_uuid_matches_only_partially() {
        let characteristics = BTreeSet::from([
            characteristic(OTHER_SERVICE, NOTIFY),
            characteristic(SERVICE, NOTIFY_LOOKALIKE),
        ]);

        assert_eq!(
            find_characteristic(&characteristics, NOTIFY_UUID),
            None,
            "サービスか UUID の一部だけが一致するキャラクタリスティックは選ばない必要があります。"
        );
    }
}
