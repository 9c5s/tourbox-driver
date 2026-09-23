//! device の統合テスト。フェイクの接続を注入し、tokio の時間を止めて検証する。
//!
//! 時刻はすべて device を起動した時点からのミリ秒で表す。
//! フェイクの送信時間は指定しない限り 0 なので、アンロックは 0 ms に送信が完了する。

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::mpsc::{self, error::TryRecvError};
use tokio::time::{sleep_until, timeout, Instant};
use tourbox::device::{Device, DeviceEvent, DeviceHandle, TransportFactory};
use tourbox::error::TransportError;
use tourbox::protocol::{
    Axis, Button, Direction, Event, HapticConfig, Speed, Strength, NOT_ALLOW_CONFIG, UNLOCK,
};
use tourbox::transport::fake::FakeHandle;
use tourbox::transport::{ConnectionConfig, Incoming, Transport, TransportKind};

/// アンロックの応答を模したバイト列。末尾の `00` は Tall の押下と同じ値である。
const UNLOCK_RESPONSE: [u8; 26] = [
    0x0e, 0x00, 0x18, 0x88, 0x94, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b,
    0x0c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// 無受信のとき、アンロックの送信完了から 1 秒で Configuring に進み、その 200 ms 後に Running に入る。
const CONFIGURING_AT: u64 = 1000;
const RUNNING_AT: u64 = 1200;

/// 接続を開く処理の結果。
#[derive(Debug, Clone, Copy)]
enum Opening {
    Connect,
    NotFound,
    Busy,
}

/// 開く処理の結果と呼び出しの記録。
#[derive(Debug)]
struct FactoryState {
    usb: Opening,
    ble: Opening,
    usb_calls: Vec<Option<String>>,
    ble_calls: usize,
}

/// テスト用の接続の生成元。接続に成功するとフェイクの新しい接続を返す。
#[derive(Debug, Clone)]
struct TestFactory {
    fake: FakeHandle,
    state: Arc<Mutex<FactoryState>>,
}

impl TestFactory {
    fn new(fake: &FakeHandle) -> Self {
        Self {
            fake: fake.clone(),
            state: Arc::new(Mutex::new(FactoryState {
                usb: Opening::Connect,
                ble: Opening::Connect,
                usb_calls: Vec::new(),
                ble_calls: 0,
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, FactoryState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_usb(&self, opening: Opening) {
        self.lock().usb = opening;
    }

    fn set_ble(&self, opening: Opening) {
        self.lock().ble = opening;
    }

    fn usb_calls(&self) -> Vec<Option<String>> {
        self.lock().usb_calls.clone()
    }

    fn ble_calls(&self) -> usize {
        self.lock().ble_calls
    }

    fn open(&self, opening: Opening) -> Result<Box<dyn Transport>, TransportError> {
        match opening {
            Opening::Connect => Ok(Box::new(self.fake.connect())),
            Opening::NotFound => Err(TransportError::NotFound),
            Opening::Busy => Err(TransportError::Busy),
        }
    }
}

impl TransportFactory for TestFactory {
    fn open_usb<'a>(
        &'a self,
        usb_port: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Box<dyn Transport>, TransportError>> {
        let opening = {
            let mut state = self.lock();
            state.usb_calls.push(usb_port.map(str::to_owned));
            state.usb
        };
        Box::pin(std::future::ready(self.open(opening)))
    }

    fn open_ble(&self) -> BoxFuture<'_, Result<Box<dyn Transport>, TransportError>> {
        let opening = {
            let mut state = self.lock();
            state.ble_calls += 1;
            state.ble
        };
        Box::pin(std::future::ready(self.open(opening)))
    }
}

/// 送信の失敗と、切断の通知なしの受信チャネルの閉塞を起こせる接続の共有状態。
#[derive(Debug, Default)]
struct ScriptState {
    /// 最後に開いた接続の受信チャネルへの送出口。
    sender: Option<mpsc::Sender<Incoming>>,
    fail_sends: bool,
    opens: usize,
    closes: usize,
}

/// フェイクでは起こせない異常を起こす接続の生成元。USB だけを開く。
#[derive(Debug, Clone, Default)]
struct ScriptFactory {
    state: Arc<Mutex<ScriptState>>,
}

struct ScriptTransport {
    state: Arc<Mutex<ScriptState>>,
    receiver: Option<mpsc::Receiver<Incoming>>,
}

impl ScriptFactory {
    fn lock(&self) -> MutexGuard<'_, ScriptState> {
        lock_script(&self.state)
    }
}

fn lock_script(state: &Mutex<ScriptState>) -> MutexGuard<'_, ScriptState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl TransportFactory for ScriptFactory {
    fn open_usb<'a>(
        &'a self,
        _usb_port: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Box<dyn Transport>, TransportError>> {
        let (sender, receiver) = mpsc::channel(16);
        let mut state = self.lock();
        state.sender = Some(sender);
        state.opens += 1;
        let transport: Box<dyn Transport> = Box::new(ScriptTransport {
            state: Arc::clone(&self.state),
            receiver: Some(receiver),
        });
        Box::pin(std::future::ready(Ok(transport)))
    }

    fn open_ble(&self) -> BoxFuture<'_, Result<Box<dyn Transport>, TransportError>> {
        panic!("このテストでは BLE を開かない必要があります。");
    }
}

impl Transport for ScriptTransport {
    fn send<'a>(&'a mut self, _data: &'a [u8]) -> BoxFuture<'a, Result<(), TransportError>> {
        let result = if lock_script(&self.state).fail_sends {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe).into())
        } else {
            Ok(())
        };
        Box::pin(std::future::ready(result))
    }

    fn take_receiver(&mut self) -> Option<mpsc::Receiver<Incoming>> {
        self.receiver.take()
    }

    fn close(&mut self) -> BoxFuture<'_, Result<(), TransportError>> {
        lock_script(&self.state).closes += 1;
        Box::pin(std::future::ready(Ok(())))
    }
}

/// 起動した device と、それを観察する手段。
struct Running {
    fake: FakeHandle,
    factory: TestFactory,
    events: mpsc::Receiver<DeviceEvent>,
    handle: DeviceHandle,
    start: Instant,
}

impl Running {
    /// 起動から `ms` ミリ秒の時点まで時間を進める。その間に期限を迎えた device の処理はすべて済む。
    async fn at(&self, ms: u64) {
        sleep_until(self.start + Duration::from_millis(ms)).await;
    }

    /// これまでに送出されたイベントを取り出す。
    fn take_events(&mut self) -> Vec<DeviceEvent> {
        let mut taken = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            taken.push(event);
        }
        taken
    }

    /// 送信が完了した件数。
    fn sent_count(&self) -> usize {
        self.fake.sent().len()
    }
}

/// USB で接続する設定で device を起動する。
fn start(haptics: HapticConfig) -> Running {
    start_with(usb_config(), haptics)
}

fn usb_config() -> ConnectionConfig {
    ConnectionConfig {
        transport: TransportKind::Usb,
        usb_port: None,
    }
}

fn start_with(config: ConnectionConfig, haptics: HapticConfig) -> Running {
    let fake = FakeHandle::new();
    let factory = TestFactory::new(&fake);
    let start = Instant::now();
    let (events, handle) = Device::run_with(config, haptics, factory.clone());
    Running {
        fake,
        factory,
        events,
        handle,
        start,
    }
}

/// `shutdown` が時間の経過を待たずに完了することを確かめる。
async fn shutdown_without_waiting(handle: DeviceHandle) {
    let before = Instant::now();
    timeout(Duration::from_secs(60), handle.shutdown())
        .await
        .expect("shutdown が完了しませんでした。");
    assert_eq!(
        Instant::now(),
        before,
        "shutdown は進行中の待機や送信の完了を待たずに終える必要があります。"
    );
}

/// 送出済みのイベントを読み捨て、device のタスクが終わってチャネルが閉じていることを確かめる。
fn assert_task_ended(events: &mut mpsc::Receiver<DeviceEvent>) {
    while events.try_recv().is_ok() {}
    assert_eq!(
        events.try_recv(),
        Err(TryRecvError::Disconnected),
        "device のタスクが終わり、イベントのチャネルが閉じている必要があります。"
    );
}

/// Knob の全組み合わせだけを既定値から変えた設定。
fn knob(strength: Strength, speed: Speed) -> HapticConfig {
    let mut config = HapticConfig::default();
    config.set_axis(Axis::Knob, strength, speed);
    config
}

fn bytes(config: &HapticConfig) -> Vec<u8> {
    config.encode().to_vec()
}

// ---- 初期化の状態機械 ----

#[tokio::test(start_paused = true)]
async fn initialization_sends_unlock_then_haptic_config() {
    let initial = knob(Strength::Weak, Speed::Slow);
    let device = start(initial.clone());

    device.at(RUNNING_AT + 1).await;

    assert_eq!(
        device.fake.sent(),
        vec![UNLOCK.to_vec(), bytes(&initial)],
        "アンロックの 8 バイト、起動時に渡したハプティクス設定の 94 バイトの順に送る必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn unlocking_without_response_moves_on_one_second_after_unlock() {
    let device = start(HapticConfig::default());

    device.at(201).await;
    assert_eq!(
        device.sent_count(),
        1,
        "無受信のとき、200 ms では Configuring へ進まない必要があります。"
    );
    device.at(CONFIGURING_AT - 1).await;
    assert_eq!(
        device.sent_count(),
        1,
        "無受信のとき、1 秒の直前ではまだ Configuring へ進まない必要があります。"
    );
    device.at(CONFIGURING_AT + 1).await;
    assert_eq!(
        device.sent_count(),
        2,
        "無受信のとき、アンロックの送信から 1 秒で Configuring へ進む必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn unlock_timeout_counts_from_send_completion() {
    let device = start(HapticConfig::default());
    device.fake.set_send_delay(Duration::from_millis(40));

    // アンロックの送信は 40 ms に完了し、94 バイトは 1040 ms に送り始めて 1080 ms に完了する
    device.at(1079).await;
    assert_eq!(
        device.sent_count(),
        1,
        "1 秒の打ち切りは、アンロックの送信開始ではなく送信完了から数える必要があります。"
    );
    device.at(1081).await;
    assert_eq!(
        device.sent_count(),
        2,
        "アンロックの送信完了から 1 秒で Configuring へ進む必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn quiet_timer_restarts_on_each_chunk_during_unlocking() {
    let device = start(HapticConfig::default());

    device.at(1).await;
    device.fake.inject(&UNLOCK_RESPONSE[..10]);
    device.at(151).await;
    device.fake.inject(&UNLOCK_RESPONSE[10..20]);
    device.at(301).await;
    device.fake.inject(&UNLOCK_RESPONSE[20..]);

    // 最後の受信 (301 ms) から 200 ms の 501 ms に進む
    device.at(500).await;
    assert_eq!(
        device.sent_count(),
        1,
        "静穏タイマーは受信のたびに 200 ms に再設定される必要があります。"
    );
    device.at(502).await;
    assert_eq!(
        device.sent_count(),
        2,
        "最後の受信から 200 ms 受信がなければ Configuring へ進む必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn late_first_chunk_moves_on_200ms_after_it() {
    let device = start(HapticConfig::default());

    device.at(500).await;
    device.fake.inject(&UNLOCK_RESPONSE);

    device.at(699).await;
    assert_eq!(
        device.sent_count(),
        1,
        "静穏タイマーは最初の受信で開始する必要があります。"
    );
    device.at(701).await;
    assert_eq!(
        device.sent_count(),
        2,
        "最初の受信から 200 ms 後に Configuring へ進む必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn chunk_just_before_one_second_does_not_extend_unlocking() {
    let device = start(HapticConfig::default());

    // 静穏タイマーだけなら 1199 ms まで待つ
    device.at(CONFIGURING_AT - 1).await;
    device.fake.inject(&UNLOCK_RESPONSE);

    device.at(CONFIGURING_AT + 1).await;
    assert_eq!(
        device.sent_count(),
        2,
        "受信の有無にかかわらず、アンロックの送信から 1 秒で打ち切る必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn not_allow_config_just_before_one_second_is_ignored() {
    let initial = HapticConfig::default();
    let mut device = start(initial.clone());

    device.at(CONFIGURING_AT - 1).await;
    device.fake.inject(NOT_ALLOW_CONFIG);

    device.at(RUNNING_AT + 1).await;
    assert_eq!(
        device.take_events(),
        vec![DeviceEvent::Connected],
        "Unlocking で受信した応答文字列は無視する必要があります。"
    );
    assert_eq!(
        device.fake.sent(),
        vec![UNLOCK.to_vec(), bytes(&initial)],
        "Unlocking で応答文字列を受信しても、初期化をやり直さない必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn not_allow_config_just_after_one_second_restarts_initialization() {
    let initial = HapticConfig::default();
    let mut device = start(initial.clone());

    device.at(CONFIGURING_AT + 1).await;
    device.fake.inject(NOT_ALLOW_CONFIG);

    device.at(CONFIGURING_AT + 2).await;
    assert_eq!(
        device.take_events(),
        vec![DeviceEvent::Disconnected],
        "Configuring で応答文字列を受信したら Disconnected を送出する必要があります。"
    );
    assert_eq!(
        device.fake.sent(),
        vec![UNLOCK.to_vec(), bytes(&initial), UNLOCK.to_vec()],
        "Configuring で応答文字列を受信したら、アンロックからやり直す必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn chunks_during_configuring_are_not_emitted() {
    let mut device = start(HapticConfig::default());

    // アンロック応答の末尾が遅れて届いた場合
    device.at(CONFIGURING_AT + 100).await;
    device.fake.inject(&[0x00, 0x00, 0x00]);

    device.at(RUNNING_AT + 1).await;
    assert_eq!(
        device.take_events(),
        vec![DeviceEvent::Connected],
        "Configuring の待機中の受信はイベントにしない必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn chunks_during_unlocking_are_not_emitted() {
    let mut device = start(HapticConfig::default());

    device.at(1).await;
    device.fake.inject(&UNLOCK_RESPONSE);

    // 応答から 200 ms で Configuring、その 200 ms 後に Running
    device.at(402).await;
    assert_eq!(
        device.take_events(),
        vec![DeviceEvent::Connected],
        "Unlocking の受信はイベントにしない必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn connected_is_emitted_when_running_starts() {
    let mut device = start(HapticConfig::default());

    device.at(RUNNING_AT - 1).await;
    assert_eq!(
        device.take_events(),
        vec![],
        "Running に入る前に Connected を送出しない必要があります。"
    );
    device.at(RUNNING_AT + 1).await;
    assert_eq!(
        device.take_events(),
        vec![DeviceEvent::Connected],
        "ハプティクス設定の送信完了から 200 ms で Running に入り、Connected を送出する必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn queued_chunk_is_handled_in_state_when_taken() {
    let mut device = start(HapticConfig::default());
    device.at(RUNNING_AT + 1).await;
    assert_eq!(device.take_events(), vec![DeviceEvent::Connected]);

    // どちらも Running のうちにチャネルへ入るが、2 つ目は再初期化の後に取り出される
    device.fake.inject(NOT_ALLOW_CONFIG);
    device.fake.inject(&[0x01]);

    device.at(RUNNING_AT + 2).await;
    assert_eq!(
        device.take_events(),
        vec![DeviceEvent::Disconnected],
        "Unlocking に戻った後に取り出したかたまりはイベントにしない必要があります。"
    );
    assert_eq!(
        device.sent_count(),
        3,
        "アンロックを送り直す必要があります。"
    );

    // 2 つ目のかたまりは Unlocking の受信として静穏タイマーを開始させる
    device.at(RUNNING_AT + 1 + 199).await;
    assert_eq!(
        device.sent_count(),
        3,
        "取り出した時点の受信から 200 ms 経つまで Configuring へ進まない必要があります。"
    );
    device.at(RUNNING_AT + 1 + 201).await;
    assert_eq!(
        device.sent_count(),
        4,
        "取り出したかたまりは Unlocking の受信として扱う必要があります。"
    );
}

// ---- Running のイベント送出 ----

#[tokio::test(start_paused = true)]
async fn running_decodes_every_byte_of_long_chunk() {
    let mut device = start(HapticConfig::default());
    device.at(RUNNING_AT + 1).await;
    assert_eq!(device.take_events(), vec![DeviceEvent::Connected]);

    let chunk = [
        0x00, 0x80, 0x01, 0x81, 0x02, 0x82, 0x03, 0x83, 0x44, 0x04, 0x49, 0x09, 0x4f, 0x0f, 0x0a,
        0x8a, 0x10, 0x90, 0x22, 0xa2, 0x84, 0x00,
    ];
    device.fake.inject(&chunk);

    device.at(RUNNING_AT + 2).await;
    let clockwise = Direction::Clockwise;
    let counter = Direction::CounterClockwise;
    let expected: Vec<DeviceEvent> = [
        Event::Press(Button::Tall),
        Event::Release(Button::Tall),
        Event::Press(Button::Side),
        Event::Release(Button::Side),
        Event::Press(Button::Top),
        Event::Release(Button::Top),
        Event::Press(Button::Short),
        Event::Release(Button::Short),
        Event::Rotate(Axis::Knob, clockwise),
        Event::Rotate(Axis::Knob, counter),
        Event::Rotate(Axis::Scroll, clockwise),
        Event::Rotate(Axis::Scroll, counter),
        Event::Rotate(Axis::Dial, clockwise),
        Event::Rotate(Axis::Dial, counter),
        Event::Press(Button::ScrollPress),
        Event::Release(Button::ScrollPress),
        Event::Press(Button::DpadUp),
        Event::Release(Button::DpadUp),
        Event::Press(Button::C1),
        Event::Release(Button::C1),
        Event::Unknown(0x84),
        Event::Press(Button::Tall),
    ]
    .into_iter()
    .map(DeviceEvent::Input)
    .collect();
    assert_eq!(
        device.take_events(),
        expected,
        "Running では 20 バイト以上のかたまりも 1 バイトずつ全件を順に送出する必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn not_allow_config_split_across_configuring_and_running_restarts_initialization() {
    let initial = HapticConfig::default();
    let mut device = start(initial.clone());
    let (head, tail) = NOT_ALLOW_CONFIG.split_at(8);

    device.at(CONFIGURING_AT + 100).await;
    device.fake.inject(head);
    device.at(RUNNING_AT + 1).await;
    device.fake.inject(tail);

    device.at(RUNNING_AT + 2).await;
    assert_eq!(
        device.take_events(),
        vec![DeviceEvent::Connected, DeviceEvent::Disconnected],
        "状態をまたいで分割到着した応答文字列を検出し、Disconnected を送出する必要があります。"
    );
    assert_eq!(
        device.fake.sent(),
        vec![UNLOCK.to_vec(), bytes(&initial), UNLOCK.to_vec()],
        "検出したらアンロックからやり直す必要があります。"
    );
    assert_eq!(
        device.fake.close_count(),
        0,
        "初期化のやり直しでは接続を閉じない必要があります。"
    );
    assert_eq!(
        device.factory.usb_calls().len(),
        1,
        "初期化のやり直しでは接続を開き直さない必要があります。"
    );

    // やり直した初期化も無受信なら 1.2 秒で Running に入る
    device.at(RUNNING_AT + 1 + RUNNING_AT + 1).await;
    assert_eq!(
        device.take_events(),
        vec![DeviceEvent::Connected],
        "やり直した初期化が完了したら、再び Connected を送出する必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn not_allow_config_split_across_unlocking_and_configuring_restarts_initialization() {
    let initial = HapticConfig::default();
    let mut device = start(initial.clone());
    let (head, tail) = NOT_ALLOW_CONFIG.split_at(8);

    device.at(CONFIGURING_AT - 1).await;
    device.fake.inject(head);
    device.at(CONFIGURING_AT + 1).await;
    device.fake.inject(tail);

    device.at(CONFIGURING_AT + 2).await;
    assert_eq!(
        device.take_events(),
        vec![DeviceEvent::Disconnected],
        "Unlocking で始まり Configuring で完成した応答文字列を検出する必要があります。"
    );
    assert_eq!(
        device.fake.sent(),
        vec![UNLOCK.to_vec(), bytes(&initial), UNLOCK.to_vec()],
        "検出したらアンロックからやり直す必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn partial_match_is_not_carried_over_when_initialization_restarts() {
    let initial = HapticConfig::default();
    let mut device = start(initial.clone());
    let (head, tail) = NOT_ALLOW_CONFIG.split_at(8);
    device.at(RUNNING_AT + 1).await;
    assert_eq!(device.take_events(), vec![DeviceEvent::Connected]);

    // 検出したかたまりの末尾に、次の文字列の先頭が続いている
    device.fake.inject(&[NOT_ALLOW_CONFIG, head].concat());
    device.at(RUNNING_AT + 2).await;
    assert_eq!(device.take_events(), vec![DeviceEvent::Disconnected]);

    // やり直した初期化は 1201 ms に始まり、2201 ms から Configuring
    device.at(2301).await;
    device.fake.inject(tail);

    device.at(2402).await;
    assert_eq!(
        device.take_events(),
        vec![DeviceEvent::Connected],
        "Unlocking に入るときに一致の途中経過を捨て、残りだけの到着では検出しない必要があります。"
    );
    assert_eq!(
        device.fake.sent(),
        vec![
            UNLOCK.to_vec(),
            bytes(&initial),
            UNLOCK.to_vec(),
            bytes(&initial)
        ],
        "初期化を 1 回だけやり直す必要があります。"
    );
}

// ---- ハプティクス更新 ----

#[tokio::test(start_paused = true)]
async fn update_during_send_is_sent_50ms_after_completion_with_latest_value() {
    let initial = HapticConfig::default();
    let first = knob(Strength::Off, Speed::Fast);
    let overwritten = knob(Strength::Weak, Speed::Slow);
    let latest = knob(Strength::Weak, Speed::Fast);
    let device = start(initial.clone());
    device.at(RUNNING_AT + 1).await;
    device.fake.set_send_delay(Duration::from_millis(40));

    // 1201 ms に送り始め、1241 ms に完了する
    device.handle.set_haptics(first.clone());
    device.at(RUNNING_AT + 11).await;
    device.handle.set_haptics(overwritten);
    device.at(RUNNING_AT + 21).await;
    device.handle.set_haptics(latest.clone());

    device.at(RUNNING_AT + 40).await;
    assert_eq!(
        device.sent_count(),
        2,
        "送信の完了前に記録されない必要があります。"
    );
    device.at(RUNNING_AT + 42).await;
    assert_eq!(
        device.fake.sent().last(),
        Some(&bytes(&first)),
        "最初の更新は 40 ms の送信の後に完了する必要があります。"
    );

    // 送信中の更新は、完了 (1241 ms) から 50 ms 後の 1291 ms に送り始め、1331 ms に完了する
    device.at(RUNNING_AT + 130).await;
    assert_eq!(
        device.sent_count(),
        3,
        "送信中に届いた更新は、送信完了から 50 ms 経つまで送り始めない必要があります。"
    );
    device.at(RUNNING_AT + 132).await;
    assert_eq!(
        device.fake.sent(),
        vec![
            UNLOCK.to_vec(),
            bytes(&initial),
            bytes(&first),
            bytes(&latest)
        ],
        "送信中に届いた更新は、送信完了から 50 ms 後に最新値だけを送る必要があります。"
    );

    device.at(RUNNING_AT + 500).await;
    assert_eq!(device.sent_count(), 4, "余分な送信をしない必要があります。");
}

#[tokio::test(start_paused = true)]
async fn update_while_idle_waits_until_50ms_after_last_send() {
    let initial = HapticConfig::default();
    let first = knob(Strength::Off, Speed::Fast);
    let second = knob(Strength::Weak, Speed::Slow);
    let device = start(initial.clone());
    device.at(RUNNING_AT + 1).await;

    device.handle.set_haptics(first.clone());
    device.at(RUNNING_AT + 11).await;
    device.handle.set_haptics(second.clone());

    // 最初の送信は 1201 ms に完了したので、次は 1251 ms まで送らない
    device.at(RUNNING_AT + 50).await;
    assert_eq!(
        device.fake.sent(),
        vec![UNLOCK.to_vec(), bytes(&initial), bytes(&first)],
        "直前の送信完了から 50 ms 経つまで次の送信を始めない必要があります。"
    );
    device.at(RUNNING_AT + 52).await;
    assert_eq!(
        device.fake.sent(),
        vec![
            UNLOCK.to_vec(),
            bytes(&initial),
            bytes(&first),
            bytes(&second)
        ],
        "直前の送信完了から 50 ms 経った時点で送る必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn update_after_interval_is_sent_immediately() {
    let first = knob(Strength::Off, Speed::Fast);
    let second = knob(Strength::Weak, Speed::Slow);
    let device = start(HapticConfig::default());
    device.at(RUNNING_AT + 1).await;
    device.handle.set_haptics(first);

    device.at(RUNNING_AT + 101).await;
    device.handle.set_haptics(second.clone());

    device.at(RUNNING_AT + 102).await;
    assert_eq!(
        device.fake.sent().last(),
        Some(&bytes(&second)),
        "直前の送信完了から 50 ms 経過済みなら、すぐに送る必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn same_config_as_last_sent_is_not_sent() {
    let initial = HapticConfig::default();
    let changed = knob(Strength::Off, Speed::Fast);
    let device = start(initial.clone());
    device.at(RUNNING_AT + 1).await;

    device.handle.set_haptics(initial.clone());
    device.at(RUNNING_AT + 101).await;
    assert_eq!(
        device.sent_count(),
        2,
        "Configuring で送った内容と同じ値は送らない必要があります。"
    );

    device.handle.set_haptics(changed.clone());
    device.at(RUNNING_AT + 201).await;
    device.handle.set_haptics(changed.clone());
    device.at(RUNNING_AT + 301).await;
    assert_eq!(
        device.fake.sent(),
        vec![UNLOCK.to_vec(), bytes(&initial), bytes(&changed)],
        "直前に送った内容と同じ値は送らない必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn update_reverted_during_send_is_not_sent() {
    let initial = HapticConfig::default();
    let first = knob(Strength::Off, Speed::Fast);
    let device = start(initial.clone());
    device.at(RUNNING_AT + 1).await;
    device.fake.set_send_delay(Duration::from_millis(40));

    device.handle.set_haptics(first.clone());
    device.at(RUNNING_AT + 11).await;
    device.handle.set_haptics(knob(Strength::Weak, Speed::Slow));
    device.at(RUNNING_AT + 21).await;
    device.handle.set_haptics(first.clone());

    device.at(RUNNING_AT + 500).await;
    assert_eq!(
        device.fake.sent(),
        vec![UNLOCK.to_vec(), bytes(&initial), bytes(&first)],
        "送信中の更新の最新値が送信中の値と同じなら、送らない必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn update_during_configuring_is_sent_when_running_starts() {
    let initial = HapticConfig::default();
    let changed = knob(Strength::Off, Speed::Fast);
    let mut device = start(initial.clone());

    device.at(CONFIGURING_AT + 100).await;
    device.handle.set_haptics(changed.clone());

    device.at(RUNNING_AT - 1).await;
    assert_eq!(
        device.fake.sent(),
        vec![UNLOCK.to_vec(), bytes(&initial)],
        "初期化中の更新は、初期化が完了するまで送らない必要があります。"
    );
    device.at(RUNNING_AT + 1).await;
    assert_eq!(device.take_events(), vec![DeviceEvent::Connected]);
    assert_eq!(
        device.fake.sent(),
        vec![UNLOCK.to_vec(), bytes(&initial), bytes(&changed)],
        "初期化中の更新は、初期化の完了後すぐに送る必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn update_during_unlocking_is_sent_by_configuring() {
    let changed = knob(Strength::Off, Speed::Fast);
    let device = start(HapticConfig::default());

    device.at(500).await;
    device.handle.set_haptics(changed.clone());

    device.at(RUNNING_AT + 300).await;
    assert_eq!(
        device.fake.sent(),
        vec![UNLOCK.to_vec(), bytes(&changed)],
        "Configuring では、その時点の最新の設定を送り、同じ値を初期化後に送り直さない必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn update_during_reinitialization_is_sent_by_configuring() {
    let initial = HapticConfig::default();
    let first = knob(Strength::Off, Speed::Fast);
    let second = knob(Strength::Weak, Speed::Slow);
    let device = start(initial.clone());
    device.at(RUNNING_AT + 1).await;
    device.handle.set_haptics(first.clone());
    device.at(RUNNING_AT + 2).await;
    device.fake.inject(NOT_ALLOW_CONFIG);

    // 1202 ms からやり直した Unlocking の途中で更新する
    device.at(RUNNING_AT + 100).await;
    device.handle.set_haptics(second.clone());

    // 2202 ms の Configuring で送り、2402 ms に Running に入る
    device.at(2700).await;
    assert_eq!(
        device.fake.sent(),
        vec![
            UNLOCK.to_vec(),
            bytes(&initial),
            bytes(&first),
            UNLOCK.to_vec(),
            bytes(&second)
        ],
        "やり直した Configuring では、直前に送った値ではなく最新の設定を送る必要があります。"
    );
}

// ---- 切断と再接続 ----

#[tokio::test(start_paused = true)]
async fn disconnect_emits_disconnected_after_pending_input_and_closes_transport() {
    let mut device = start(HapticConfig::default());
    device.at(RUNNING_AT + 1).await;
    assert_eq!(device.take_events(), vec![DeviceEvent::Connected]);

    device.fake.inject(&[0x01]);
    device.fake.disconnect();

    device.at(RUNNING_AT + 2).await;
    assert_eq!(
        device.take_events(),
        vec![
            DeviceEvent::Input(Event::Press(Button::Side)),
            DeviceEvent::Disconnected
        ],
        "切断の前に届いたかたまりを送出してから Disconnected を送出する必要があります。"
    );
    assert_eq!(
        device.fake.close_count(),
        1,
        "切断を検知したら、再接続の待機の前に接続を閉じる必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn reconnects_one_second_after_disconnect_and_initializes_again() {
    let initial = HapticConfig::default();
    let mut device = start(initial.clone());
    device.at(RUNNING_AT + 1).await;
    device.fake.disconnect();

    device.at(RUNNING_AT + 1 + 999).await;
    assert_eq!(
        device.factory.usb_calls().len(),
        1,
        "切断から 1 秒経つまで再接続しない必要があります。"
    );
    device.at(RUNNING_AT + 1 + 1001).await;
    assert_eq!(
        device.factory.usb_calls().len(),
        2,
        "切断から 1 秒で再接続する必要があります。"
    );

    // 再接続した 2201 ms から初期化をやり直し、無受信なら 1.2 秒で Running に入る
    device.at(RUNNING_AT + 1 + 1000 + RUNNING_AT + 1).await;
    assert_eq!(
        device.take_events(),
        vec![
            DeviceEvent::Connected,
            DeviceEvent::Disconnected,
            DeviceEvent::Connected
        ],
        "再接続した接続の初期化が完了したら Connected を送出する必要があります。"
    );
    assert_eq!(
        device.fake.sent(),
        vec![
            UNLOCK.to_vec(),
            bytes(&initial),
            UNLOCK.to_vec(),
            bytes(&initial)
        ],
        "再接続では初期化を最初からやり直し、変わっていなくても現在の設定を送る必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn reconnect_interval_doubles_up_to_30_seconds_and_resets_after_running() {
    let mut device = start(HapticConfig::default());
    device.at(RUNNING_AT + 1).await;
    device.factory.set_usb(Opening::NotFound);
    device.fake.disconnect();

    // 切断から 1、2、4、8、16、30、30 秒の間隔で試みる
    let mut attempt_at = RUNNING_AT + 1;
    for (index, interval) in [1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000]
        .into_iter()
        .enumerate()
    {
        attempt_at += interval;
        let calls_before = index + 1;
        device.at(attempt_at - 1).await;
        assert_eq!(
            device.factory.usb_calls().len(),
            calls_before,
            "{interval} ms の間隔が経つ前に再接続を試みない必要があります。"
        );
        device.at(attempt_at + 1).await;
        assert_eq!(
            device.factory.usb_calls().len(),
            calls_before + 1,
            "{interval} ms の間隔で再接続を試みる必要があります。"
        );
    }

    // 次の 30 秒後の試行で接続し、Running に入ったら間隔を 1 秒に戻す
    device.factory.set_usb(Opening::Connect);
    attempt_at += 30_000;
    let running_at = attempt_at + RUNNING_AT + 1;
    device.at(running_at).await;
    device.take_events();
    device.fake.disconnect();
    device.at(running_at + 999).await;
    assert_eq!(device.factory.usb_calls().len(), 9);
    device.at(running_at + 1001).await;
    assert_eq!(
        device.factory.usb_calls().len(),
        10,
        "Running に入った接続が切れたら、1 秒の間隔から数え直す必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn update_while_disconnected_is_sent_by_configuring_after_reconnect() {
    let initial = HapticConfig::default();
    let changed = knob(Strength::Off, Speed::Fast);
    let device = start(initial.clone());
    device.at(RUNNING_AT + 1).await;
    device.fake.disconnect();

    device.at(RUNNING_AT + 301).await;
    device.handle.set_haptics(changed.clone());

    // 2201 ms に再接続し、3201 ms の Configuring で送る。3401 ms に Running に入る
    device.at(3200).await;
    assert_eq!(
        device.fake.sent(),
        vec![UNLOCK.to_vec(), bytes(&initial), UNLOCK.to_vec()],
        "切断中の更新は、再接続の Configuring まで送らない必要があります。"
    );
    device.at(3700).await;
    assert_eq!(
        device.fake.sent(),
        vec![
            UNLOCK.to_vec(),
            bytes(&initial),
            UNLOCK.to_vec(),
            bytes(&changed)
        ],
        "切断中の更新は、再接続の Configuring で送り、初期化後に送り直さない必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn receiver_closed_without_notice_is_treated_as_disconnect() {
    let factory = ScriptFactory::default();
    let start = Instant::now();
    let (mut events, _handle) =
        Device::run_with(usb_config(), HapticConfig::default(), factory.clone());
    sleep_until(start + Duration::from_millis(RUNNING_AT + 1)).await;
    assert_eq!(events.try_recv(), Ok(DeviceEvent::Connected));

    // 送出口を捨て、Disconnected を届けずに受信チャネルを閉じる
    factory.lock().sender = None;

    sleep_until(start + Duration::from_millis(RUNNING_AT + 2)).await;
    assert_eq!(
        events.try_recv(),
        Ok(DeviceEvent::Disconnected),
        "切断の通知なしに受信チャネルが閉じたら、切断として扱う必要があります。"
    );
    assert_eq!(factory.lock().closes, 1, "接続を閉じる必要があります。");
    sleep_until(start + Duration::from_millis(RUNNING_AT + 1 + 1001)).await;
    assert_eq!(
        factory.lock().opens,
        2,
        "1 秒後に再接続する必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn send_failure_is_treated_as_disconnect() {
    let factory = ScriptFactory::default();
    let start = Instant::now();
    let (mut events, handle) =
        Device::run_with(usb_config(), HapticConfig::default(), factory.clone());
    sleep_until(start + Duration::from_millis(RUNNING_AT + 1)).await;
    assert_eq!(events.try_recv(), Ok(DeviceEvent::Connected));

    factory.lock().fail_sends = true;
    handle.set_haptics(knob(Strength::Off, Speed::Fast));

    sleep_until(start + Duration::from_millis(RUNNING_AT + 2)).await;
    assert_eq!(
        events.try_recv(),
        Ok(DeviceEvent::Disconnected),
        "送信に失敗したら、切断として Disconnected を送出する必要があります。"
    );
    assert_eq!(factory.lock().closes, 1, "接続を閉じる必要があります。");
    sleep_until(start + Duration::from_millis(RUNNING_AT + 1 + 1001)).await;
    assert_eq!(
        factory.lock().opens,
        2,
        "1 秒後に再接続する必要があります。"
    );
}

// ---- 接続方式の選択 ----

#[tokio::test(start_paused = true)]
async fn usb_kind_opens_only_usb_with_configured_port() {
    let device = start_with(
        ConnectionConfig {
            transport: TransportKind::Usb,
            usb_port: Some("COM3".to_owned()),
        },
        HapticConfig::default(),
    );

    device.at(1).await;
    assert_eq!(
        device.factory.usb_calls(),
        vec![Some("COM3".to_owned())],
        "設定のポート名を渡して USB を開く必要があります。"
    );
    assert_eq!(
        device.factory.ble_calls(),
        0,
        "BLE を開かない必要があります。"
    );
    assert_eq!(device.fake.sent(), vec![UNLOCK.to_vec()]);
}

#[tokio::test(start_paused = true)]
async fn ble_kind_opens_only_ble() {
    let device = start_with(
        ConnectionConfig {
            transport: TransportKind::Ble,
            usb_port: Some("COM3".to_owned()),
        },
        HapticConfig::default(),
    );

    device.at(1).await;
    assert_eq!(device.factory.ble_calls(), 1, "BLE を開く必要があります。");
    assert_eq!(
        device.factory.usb_calls(),
        Vec::<Option<String>>::new(),
        "ポート名の指定があっても USB を開かない必要があります。"
    );
    assert_eq!(device.fake.sent(), vec![UNLOCK.to_vec()]);
}

fn auto_config() -> ConnectionConfig {
    ConnectionConfig {
        transport: TransportKind::Auto,
        usb_port: Some("COM3".to_owned()),
    }
}

#[tokio::test(start_paused = true)]
async fn auto_kind_uses_usb_when_found() {
    let device = start_with(auto_config(), HapticConfig::default());

    device.at(1).await;
    assert_eq!(device.factory.usb_calls(), vec![Some("COM3".to_owned())]);
    assert_eq!(
        device.factory.ble_calls(),
        0,
        "USB で開けたら BLE を開かない必要があります。"
    );
    assert_eq!(device.fake.sent(), vec![UNLOCK.to_vec()]);
}

#[tokio::test(start_paused = true)]
async fn auto_kind_falls_back_to_ble_when_usb_not_found() {
    let device = start_with(auto_config(), HapticConfig::default());
    device.factory.set_usb(Opening::NotFound);

    device.at(1).await;
    assert_eq!(device.factory.usb_calls(), vec![Some("COM3".to_owned())]);
    assert_eq!(
        device.factory.ble_calls(),
        1,
        "USB が見つからなければ BLE を開く必要があります。"
    );
    assert_eq!(device.fake.sent(), vec![UNLOCK.to_vec()]);
}

#[tokio::test(start_paused = true)]
async fn auto_kind_retries_usb_without_ble_when_usb_busy() {
    let device = start_with(auto_config(), HapticConfig::default());
    device.factory.set_usb(Opening::Busy);

    device.at(1001).await;
    assert_eq!(
        device.factory.usb_calls().len(),
        2,
        "USB が使用中なら、再接続の間隔で USB を試し直す必要があります。"
    );
    assert_eq!(
        device.factory.ble_calls(),
        0,
        "USB が使用中なら BLE へ進まない必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn auto_kind_retries_both_when_neither_found() {
    let device = start_with(auto_config(), HapticConfig::default());
    device.factory.set_usb(Opening::NotFound);
    device.factory.set_ble(Opening::NotFound);

    device.at(999).await;
    assert_eq!(device.factory.usb_calls().len(), 1);
    assert_eq!(device.factory.ble_calls(), 1);
    device.at(1001).await;
    assert_eq!(
        (device.factory.usb_calls().len(), device.factory.ble_calls()),
        (2, 2),
        "どちらも見つからなければ、再接続の間隔で USB から試し直す必要があります。"
    );
}

// ---- 停止 ----

#[tokio::test(start_paused = true)]
async fn shutdown_during_reconnect_wait_ends_task_without_reconnecting() {
    let device = start(HapticConfig::default());
    device.at(RUNNING_AT + 1).await;
    device.fake.disconnect();
    device.at(RUNNING_AT + 2).await;
    let Running {
        fake,
        factory,
        mut events,
        handle,
        start,
    } = device;

    shutdown_without_waiting(handle).await;

    assert_task_ended(&mut events);
    sleep_until(start + Duration::from_secs(10)).await;
    assert_eq!(
        factory.usb_calls().len(),
        1,
        "停止した後は再接続しない必要があります。"
    );
    assert_eq!(
        fake.close_count(),
        1,
        "切断で閉じた接続を、停止で重ねて閉じない必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn shutdown_during_initialization_closes_transport_and_ends_task() {
    // Unlocking の待機中と Configuring の待機中
    for stop_at in [500, CONFIGURING_AT + 100] {
        let device = start(HapticConfig::default());
        device.at(stop_at).await;
        let sent_before = device.sent_count();
        let Running {
            fake,
            factory,
            mut events,
            handle,
            start,
        } = device;

        shutdown_without_waiting(handle).await;

        assert_eq!(
            fake.close_count(),
            1,
            "{stop_at} ms の停止で接続を閉じる必要があります。"
        );
        assert_task_ended(&mut events);
        sleep_until(start + Duration::from_secs(10)).await;
        assert_eq!(
            fake.sent().len(),
            sent_before,
            "{stop_at} ms の停止の後は初期化を進めない必要があります。"
        );
        assert_eq!(
            factory.usb_calls().len(),
            1,
            "{stop_at} ms の停止の後は再接続しない必要があります。"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn shutdown_during_haptics_send_discards_send_and_closes_transport() {
    let initial = HapticConfig::default();
    let device = start(initial.clone());
    device.at(RUNNING_AT + 1).await;
    device.fake.set_send_delay(Duration::from_millis(40));
    device.handle.set_haptics(knob(Strength::Off, Speed::Fast));

    // 1201 ms から 1241 ms まで送信している途中
    device.at(RUNNING_AT + 21).await;
    let Running {
        fake,
        mut events,
        handle,
        start,
        ..
    } = device;

    shutdown_without_waiting(handle).await;

    assert_eq!(fake.close_count(), 1, "接続を閉じる必要があります。");
    assert_task_ended(&mut events);
    // フェイクは途中で破棄された送信を記録しない
    sleep_until(start + Duration::from_secs(10)).await;
    assert_eq!(
        fake.sent(),
        vec![UNLOCK.to_vec(), bytes(&initial)],
        "進行中の送信の完了を待たずに破棄する必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn shutdown_during_unlock_send_discards_send_and_closes_transport() {
    let device = start(HapticConfig::default());
    device.fake.set_send_delay(Duration::from_millis(40));

    // 0 ms から 40 ms までアンロックを送信している途中
    device.at(20).await;
    let Running {
        fake,
        mut events,
        handle,
        start,
        ..
    } = device;

    shutdown_without_waiting(handle).await;

    assert_eq!(fake.close_count(), 1, "接続を閉じる必要があります。");
    assert_task_ended(&mut events);
    sleep_until(start + Duration::from_secs(10)).await;
    assert_eq!(
        fake.sent(),
        Vec::<Vec<u8>>::new(),
        "初期化の送信も完了を待たずに破棄し、初期化を進めない必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn dropping_handle_requests_stop_without_waiting() {
    let device = start(HapticConfig::default());
    device.at(RUNNING_AT + 1).await;
    let Running {
        fake,
        factory,
        mut events,
        handle,
        start,
    } = device;

    drop(handle);
    assert_eq!(
        fake.close_count(),
        0,
        "Drop は停止の完了を待たずに戻る必要があります。"
    );

    sleep_until(start + Duration::from_millis(RUNNING_AT + 2)).await;
    assert_eq!(
        fake.close_count(),
        1,
        "Drop の停止要求で、タスクが接続を閉じる必要があります。"
    );
    assert_task_ended(&mut events);
    sleep_until(start + Duration::from_secs(10)).await;
    assert_eq!(
        factory.usb_calls().len(),
        1,
        "Drop の後は再接続しない必要があります。"
    );
}
