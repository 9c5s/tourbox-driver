//! 常駐コマンド。設定の割り当てに従って、TourBox の操作を MIDI メッセージに変換し続ける。

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::time::Duration;

use anyhow::Context;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tokio::time::{self, Instant, MissedTickBehavior};
use tourbox::device::{Device, DeviceEvent, DeviceHandle};
use tourbox::protocol::{Event, HapticConfig};
use tourbox::transport::ConnectionConfig;
use tracing::{debug, info};

use super::reload::{reload, ReloadTarget};
use crate::config::{default_config_path, watch, Config, HapticsControlConfig, MappingSet};
use crate::engine::Engine;
use crate::haptics::HapticsController;
use crate::input::{InputBackend, InputState, MidiInBackend};
use crate::midi::{parse_control_change, PortMode};
use crate::output::{MidiOutBackend, OutputBackend, OutputState};

/// 出力ポートと入力ポートを開き直す周期。`Existing` では接続中の一覧の再評価もこの周期で行う。
const RETRY_PERIOD: Duration = Duration::from_secs(5);
/// 入力ポートが受信したバイト列を溜めるチャネルの容量。
const INPUT_CAPACITY: usize = 1024;
/// `RUST_LOG` がないときのログの絞り込み。
const DEFAULT_FILTER: &str = "info";
/// `--verbose` のときのログの絞り込み。受信バイトと送信した MIDI メッセージの debug ログを含める。
const VERBOSE_FILTER: &str = "debug";

/// 設定ファイルを読み込んで常駐し、Ctrl+C で押下中のボタンを解放してから戻る。
/// 常駐中は設定ファイルの変更を監視して反映する。
///
/// `config_path` が None なら既定の場所の設定ファイルを読む。
pub fn run(config_path: Option<&Path>, verbose: bool) -> anyhow::Result<()> {
    let path = config_path.map_or_else(default_config_path, Path::to_path_buf);
    let config = Config::load(&path)?;
    super::init_logging(if verbose {
        VERBOSE_FILTER
    } else {
        DEFAULT_FILTER
    })?;
    info!(path = %path.display(), "設定ファイルを読み込みました。");
    let runtime = Runtime::new().context("非同期ランタイムを起動できませんでした。")?;
    runtime.block_on(reside(path, config))
}

/// Ctrl+C の受付、設定ファイルの監視、MIDI 出力の準備、デバイス接続、MIDI 入力の準備の順に始めて
/// 常駐ループを実行する。
async fn reside(path: PathBuf, config: Config) -> anyhow::Result<()> {
    let stop = listen_ctrl_c().context("Ctrl+C の受付を開始できませんでした。")?;
    // 破棄すると監視が止まるので、常駐の終わりまで保持する
    let (_watcher, changes) = watch(&path)?;
    let resident = Resident::start(
        &config,
        MidiOutBackend,
        MidiInBackend,
        PortMode::default_for_os(),
        HardwareLink::default(),
    );
    info!("常駐を開始しました。Ctrl+C で終了します。");
    let config = WatchedConfig {
        path,
        current: config,
        changes,
    };
    serve(resident, config, stop).await;
    Ok(())
}

/// Ctrl+C の受付をこの時点で登録し、受け付けたら完了する future を返す。
fn listen_ctrl_c() -> io::Result<impl Future<Output = ()>> {
    #[cfg(windows)]
    let mut listener = tokio::signal::windows::ctrl_c()?;
    #[cfg(unix)]
    let mut listener = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    Ok(async move {
        listener.recv().await;
    })
}

/// デバイスとの接続。テストでは差し替える。
trait DeviceLink {
    /// 接続を始め、デバイスのイベントの受信チャネルを返す。止まっているときに呼ぶ。
    fn start(
        &mut self,
        connection: ConnectionConfig,
        haptics: HapticConfig,
    ) -> mpsc::Receiver<DeviceEvent>;

    /// ハプティクス設定の更新を要求する。止まっているときは何もしない。
    fn set_haptics(&self, haptics: HapticConfig);

    /// 接続を止め、止まるまで待つ。
    async fn stop(&mut self);
}

/// 実機との接続。None は止まっている。
#[derive(Default)]
struct HardwareLink(Option<DeviceHandle>);

impl DeviceLink for HardwareLink {
    fn start(
        &mut self,
        connection: ConnectionConfig,
        haptics: HapticConfig,
    ) -> mpsc::Receiver<DeviceEvent> {
        let (events, handle) = Device::run(connection, haptics);
        self.0 = Some(handle);
        events
    }

    fn set_haptics(&self, haptics: HapticConfig) {
        if let Some(handle) = &self.0 {
            handle.set_haptics(haptics);
        }
    }

    async fn stop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.shutdown().await;
        }
    }
}

/// 監視している設定ファイル。
struct WatchedConfig {
    path: PathBuf,
    /// 最後に反映した設定。
    current: Config,
    /// 設定ファイルの変更の通知。
    changes: mpsc::Receiver<()>,
}

/// 常駐中に動かす部品。設定の再読込で割り当て、ハプティクス制御、ポート、デバイスとの接続を差し替える。
struct Resident<B: OutputBackend, I: InputBackend, D: DeviceLink> {
    engine: Engine,
    output: OutputState<B>,
    input: HapticsInput<I>,
    device: D,
    /// 今の接続のデバイスのイベント。
    events: mpsc::Receiver<DeviceEvent>,
}

impl<B: OutputBackend, I: InputBackend + Clone, D: DeviceLink> Resident<B, I, D> {
    /// MIDI 出力の準備 (最初の tick まで済ませる)、デバイス接続の開始、MIDI 入力の準備の順に始める。
    fn start(
        config: &Config,
        output_backend: B,
        input_backend: I,
        mode: PortMode,
        mut device: D,
    ) -> Self {
        let mut output = OutputState::new(output_backend, config.midi.output.clone(), mode);
        output.tick();
        let events = device.start(config.to_connection_config(), config.to_haptic_config());
        let input = prepare_input(input_backend, mode, config);
        Self {
            engine: Engine::new(config.resolve_mapping()),
            output,
            input,
            device,
            events,
        }
    }

    /// デバイスのイベントを engine で変換して出力する。
    fn handle_event(&mut self, event: DeviceEvent) {
        log_device_event(&event);
        for outgoing in self.engine.handle(event) {
            self.output.send(outgoing);
        }
    }

    /// 入力ポートが受信した 1 メッセージを反映し、ハプティクス設定が変わったらデバイスへ送る。
    fn handle_input(&mut self, bytes: &[u8]) {
        if let Some(haptics) = self.input.handle(bytes) {
            self.device.set_haptics(haptics);
        }
    }

    /// 出力ポートと入力ポートを再試行する。
    fn tick(&mut self) {
        self.output.tick();
        self.input.tick();
    }

    /// 押下中のボタンと台帳の Off を送ってから出力ポートを閉じ、入力ポートを閉じ、デバイスを止める。
    async fn shutdown(mut self) {
        self.output.shutdown(self.engine.release_all());
        self.input.shutdown();
        self.device.stop().await;
    }
}

impl<B: OutputBackend, I: InputBackend + Clone, D: DeviceLink> ReloadTarget for Resident<B, I, D> {
    fn release_all(&mut self) {
        for off in self.engine.release_all() {
            self.output.send(off);
        }
    }

    fn replace_mapping(&mut self, mapping: MappingSet) {
        self.engine.replace_mapping(mapping);
    }

    fn reset_haptics(&mut self, control: HapticsControlConfig, base: HapticConfig) {
        let base = self.input.reset(control, base);
        self.device.set_haptics(base);
    }

    fn reopen_output(&mut self, name: String) {
        self.output.reopen(name);
    }

    fn reopen_input(&mut self, name: Option<String>) {
        self.input.set_port(name);
    }

    async fn restart_device(&mut self, connection: ConnectionConfig, haptics: HapticConfig) {
        info!("[device] が変わったため、TourBox との接続をやり直します。");
        self.device.stop().await;
        self.events = self.device.start(connection, haptics);
    }
}

/// 入力ポートの状態 (最初の tick まで済ませる) とハプティクス制御を作る。
///
/// `midi.input` がなければ入力ポートを持たず、入力機能は無効である。
fn prepare_input<I: InputBackend + Clone>(
    backend: I,
    mode: PortMode,
    config: &Config,
) -> HapticsInput<I> {
    let (tx, received) = mpsc::channel(INPUT_CAPACITY);
    let mut input = HapticsInput {
        backend,
        mode,
        tx,
        port: None,
        received,
        controller: HapticsController::new(config.haptics_control(), config.to_haptic_config()),
    };
    input.set_port(config.midi.input.clone());
    input
}

/// MIDI 入力の受信と、受信した Control Change によるハプティクス制御 (設計書 6.2 節)。
struct HapticsInput<I: InputBackend> {
    backend: I,
    mode: PortMode,
    /// 入力ポートが受信したバイト列の送り先。入力ポートを作るたびに複製を渡す。
    tx: mpsc::Sender<Vec<u8>>,
    /// None は入力機能が無効。
    port: Option<InputState<I>>,
    /// 入力ポートが受信したバイト列。
    received: mpsc::Receiver<Vec<u8>>,
    controller: HapticsController,
}

impl<I: InputBackend + Clone> HapticsInput<I> {
    fn tick(&mut self) {
        if let Some(port) = &mut self.port {
            port.tick();
        }
    }

    /// 入力ポートを `name` に切り替える。開いているポートは閉じ、`name` があればすぐに開く。
    /// None なら入力機能を無効にする。
    fn set_port(&mut self, name: Option<String>) {
        let Some(name) = name else {
            if let Some(port) = self.port.take() {
                port.shutdown();
            }
            return;
        };
        match &mut self.port {
            Some(port) => port.reopen(name),
            None => {
                let mut port =
                    InputState::new(self.backend.clone(), name, self.mode, self.tx.clone());
                port.tick();
                self.port = Some(port);
            }
        }
    }

    /// 受信した 1 メッセージを反映し、ハプティクス設定が変わったら新しい設定を返す。
    ///
    /// Control Change 以外のメッセージは捨てる。
    fn handle(&mut self, bytes: &[u8]) -> Option<HapticConfig> {
        let Some((channel, cc, value)) = parse_control_change(bytes) else {
            debug!(
                bytes = format_args!("{bytes:02x?}"),
                "Control Change 以外の MIDI メッセージを受信したため、捨てました。"
            );
            return None;
        };
        // ログのチャンネルは設定ファイルと同じ 1 起点で出す
        debug!(
            channel = channel + 1,
            cc, value, "MIDI 入力で Control Change を受信しました。"
        );
        let haptics = self.controller.on_cc(channel, cc, value)?;
        info!(
            channel = channel + 1,
            cc, value, "受信した Control Change でハプティクス設定を変更します。"
        );
        Some(haptics)
    }

    /// ハプティクス制御を作り直し、基準の設定を返す。
    fn reset(&mut self, control: HapticsControlConfig, base: HapticConfig) -> HapticConfig {
        self.controller.reset(control, base)
    }

    fn shutdown(self) {
        if let Some(port) = self.port {
            port.shutdown();
        }
    }
}

/// 常駐ループ。`stop` が完了するまで、デバイスのイベントを engine で変換して出力し、
/// 入力ポートが受信した Control Change で変わったハプティクス設定をデバイスへ送り、
/// 設定ファイルの変更を反映し、[`RETRY_PERIOD`] ごとに出力ポートと入力ポートを再試行する。
///
/// 終わるときは押下中のボタンと台帳の Off を送ってから出力ポートを閉じ、入力ポートを閉じ、
/// デバイスを止める。
async fn serve<B: OutputBackend, I: InputBackend + Clone, D: DeviceLink>(
    mut resident: Resident<B, I, D>,
    mut config: WatchedConfig,
    stop: impl Future<Output = ()>,
) {
    let input_mode = resident.input.mode;
    let mut retry = time::interval_at(Instant::now() + RETRY_PERIOD, RETRY_PERIOD);
    retry.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut stop = pin!(stop);
    loop {
        tokio::select! {
            () = &mut stop => {
                info!("Ctrl+C を受け付けました。押下中のボタンを解放して終了します。");
                break;
            }
            received = resident.events.recv() => {
                // device のタスクは停止要求まで終わらない。終わるのはパニックしたときで、shutdown がそのパニックを再開する
                let Some(event) = received else { break };
                resident.handle_event(event);
            }
            Some(bytes) = resident.input.received.recv() => resident.handle_input(&bytes),
            // 監視が止まって送り手がなくなると None になり、この腕は選ばれなくなる
            Some(()) = config.changes.recv() => {
                reload(&mut resident, &config.path, &mut config.current, input_mode).await;
            }
            _ = retry.tick() => resident.tick(),
        }
    }
    resident.shutdown().await;
}

/// 接続状態の変化と未知のイベント値をログに出す。
fn log_device_event(event: &DeviceEvent) {
    match event {
        DeviceEvent::Connected => info!("TourBox と接続しました。"),
        DeviceEvent::Disconnected => {
            info!("TourBox が切断されたため、押下中のボタンを解放します。");
        }
        DeviceEvent::Input(Event::Unknown(value)) => {
            info!("未知のイベント値 {value:#04x} を受信しました。無視します。");
        }
        DeviceEvent::Input(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::rc::Rc;

    use tokio::sync::oneshot;
    use tokio::time::sleep;
    use tourbox::protocol::{Axis, Button, Modifier, Strength};
    use tourbox::transport::TransportKind;

    use super::*;
    use crate::input::fake as input_fake;
    use crate::midi_msg::MidiMessage;
    use crate::output::fake::{Call, FakeBackend};

    const PORT: &str = "loopMIDI Port";
    const NEW_PORT: &str = "TourBox MIDI";
    const INPUT_PORT: &str = "loopMIDI TourBox In";
    /// Side を修飾ボタンにする。Top は基本レイヤでは CC 20、Side の修飾中は Note 70 になる。
    const MAP: &str =
        "side = { note = 50 }\ntop = { cc = 20 }\n[map.with.side]\ntop = { note = 70 }\n";
    /// 時間を止めたテストで、送ったイベントを serve に処理させてから次へ進むための短い待ち。
    const MOMENT: Duration = Duration::from_millis(1);
    /// フェイクのデバイスが止まるまでにかかる時間。
    const STOP_DELAY: Duration = Duration::from_millis(10);

    fn parse(text: &str) -> Config {
        Config::parse(text, Path::new("config.toml"))
            .unwrap_or_else(|error| panic!("検証に通る必要があります: {error}"))
    }

    /// 入力ポートのない設定ファイルの内容。
    fn text() -> String {
        format!("[midi]\noutput = \"{PORT}\"\nchannel = 1\n[map]\n{MAP}")
    }

    /// 入力ポートのない設定。
    fn config() -> Config {
        parse(&text())
    }

    /// 入力ポートが `input` で、チャンネル 2 の CC 100 で Knob の強度を制御する設定ファイルの内容。
    fn text_with_input(input: Option<&str>) -> String {
        let input = input.map_or(String::new(), |name| format!("input = \"{name}\"\n"));
        format!(
            "[midi]\noutput = \"{PORT}\"\n{input}channel = 1\n[map]\n{MAP}[haptics.control]\nchannel = 2\nknob = {{ cc = 100 }}\n"
        )
    }

    /// 入力ポートがあり、チャンネル 2 の CC 100 で Knob の強度を制御する設定。
    fn config_with_input() -> Config {
        parse(&text_with_input(Some(INPUT_PORT)))
    }

    /// 既定のハプティクス設定から、Knob の全組み合わせの強度をなしにした設定。
    fn knob_off() -> HapticConfig {
        let mut haptics = HapticConfig::default();
        for modifier in Modifier::ALL {
            haptics.set_strength(Axis::Knob, modifier, Strength::Off);
        }
        haptics
    }

    /// デバイスとの接続への操作。
    #[derive(Debug, Clone, PartialEq)]
    enum DeviceCall {
        Start(ConnectionConfig, HapticConfig),
        SetHaptics(HapticConfig),
        /// 停止の完了。
        Stopped,
    }

    #[derive(Default)]
    struct FakeDeviceState {
        calls: Vec<DeviceCall>,
        /// 今の接続のイベントの送り手。None は止まっている。
        events: Option<mpsc::Sender<DeviceEvent>>,
    }

    /// 操作を記録し、接続ごとのイベントの送り手をテストに渡すデバイス。複製は状態を共有する。
    #[derive(Clone, Default)]
    struct FakeDevice(Rc<RefCell<FakeDeviceState>>);

    impl FakeDevice {
        /// 今の接続のイベントの送り手。
        fn events(&self) -> mpsc::Sender<DeviceEvent> {
            self.0
                .borrow()
                .events
                .clone()
                .expect("デバイスとの接続が始まっている必要があります。")
        }

        /// これまでの操作を取り出し、記録を空にする。
        fn take_calls(&self) -> Vec<DeviceCall> {
            std::mem::take(&mut self.0.borrow_mut().calls)
        }
    }

    impl DeviceLink for FakeDevice {
        fn start(
            &mut self,
            connection: ConnectionConfig,
            haptics: HapticConfig,
        ) -> mpsc::Receiver<DeviceEvent> {
            let (tx, rx) = mpsc::channel(16);
            let mut state = self.0.borrow_mut();
            state.calls.push(DeviceCall::Start(connection, haptics));
            state.events = Some(tx);
            rx
        }

        fn set_haptics(&self, haptics: HapticConfig) {
            self.0
                .borrow_mut()
                .calls
                .push(DeviceCall::SetHaptics(haptics));
        }

        async fn stop(&mut self) {
            sleep(STOP_DELAY).await;
            let mut state = self.0.borrow_mut();
            state.events = None;
            state.calls.push(DeviceCall::Stopped);
        }
    }

    fn started(config: &Config) -> DeviceCall {
        DeviceCall::Start(config.to_connection_config(), config.to_haptic_config())
    }

    /// run の起動手順 (ポートの開き方は Existing) で常駐の部品を作る。
    fn start(
        config: &Config,
        output: &FakeBackend,
        input: &input_fake::FakeBackend,
        device: &FakeDevice,
    ) -> Resident<FakeBackend, input_fake::FakeBackend, FakeDevice> {
        Resident::start(
            config,
            output.clone(),
            input.clone(),
            PortMode::Existing,
            device.clone(),
        )
    }

    /// 変更の通知が来ない設定ファイル。
    fn unwatched(config: Config) -> WatchedConfig {
        WatchedConfig {
            path: PathBuf::from("config.toml"),
            current: config,
            changes: mpsc::channel(1).1,
        }
    }

    /// 変更をテストから通知できる設定ファイル。
    fn watched(path: &Path, config: Config) -> (WatchedConfig, mpsc::Sender<()>) {
        let (tx, changes) = mpsc::channel(1);
        let config = WatchedConfig {
            path: path.to_owned(),
            current: config,
            changes,
        };
        (config, tx)
    }

    fn input_opened() -> input_fake::Call {
        input_fake::Call::Open(INPUT_PORT.to_owned(), PortMode::Existing)
    }

    fn input_closed() -> input_fake::Call {
        input_fake::Call::Close(INPUT_PORT.to_owned())
    }

    async fn emit(events: &mpsc::Sender<DeviceEvent>, event: DeviceEvent) {
        events
            .send(event)
            .await
            .expect("serve がイベントを受け取れる必要があります。");
    }

    async fn stopped(stop: oneshot::Receiver<()>) {
        let _ = stop.await;
    }

    fn press(button: Button) -> DeviceEvent {
        DeviceEvent::Input(Event::Press(button))
    }

    fn release(button: Button) -> DeviceEvent {
        DeviceEvent::Input(Event::Release(button))
    }

    fn opened() -> Call {
        Call::Open(PORT.to_owned(), PortMode::Existing)
    }

    fn closed() -> Call {
        Call::Close(PORT.to_owned())
    }

    fn sent(message: MidiMessage) -> Call {
        Call::Send(PORT.to_owned(), message)
    }

    fn note_on(note: u8) -> MidiMessage {
        MidiMessage::NoteOn {
            channel: 0,
            note,
            velocity: 127,
        }
    }

    fn note_off(note: u8) -> MidiMessage {
        MidiMessage::NoteOff { channel: 0, note }
    }

    fn cc(cc: u8, value: u8) -> MidiMessage {
        MidiMessage::ControlChange {
            channel: 0,
            cc,
            value,
        }
    }

    /// 接続を始めた時点の出力と入力への呼び出しを記録するデバイス。
    struct StartProbe {
        output: FakeBackend,
        input: input_fake::FakeBackend,
        seen_at_start: Option<(Vec<Call>, Vec<input_fake::Call>)>,
    }

    impl DeviceLink for StartProbe {
        fn start(&mut self, _: ConnectionConfig, _: HapticConfig) -> mpsc::Receiver<DeviceEvent> {
            self.seen_at_start = Some((self.output.take_calls(), self.input.take_calls()));
            mpsc::channel(1).1
        }

        fn set_haptics(&self, _: HapticConfig) {}

        async fn stop(&mut self) {}
    }

    #[test]
    fn start_prepares_output_then_device_then_input() {
        let output = FakeBackend::openable(PORT);
        let input = input_fake::FakeBackend::openable(INPUT_PORT);
        let probe = StartProbe {
            output: output.clone(),
            input: input.clone(),
            seen_at_start: None,
        };

        let resident = Resident::start(
            &config_with_input(),
            output,
            input.clone(),
            PortMode::Existing,
            probe,
        );

        assert_eq!(
            resident.device.seen_at_start,
            Some((vec![opened()], vec![])),
            "デバイスとの接続は、出力ポートを開いた後、入力ポートを開く前に始める必要があります。"
        );
        assert_eq!(
            input.take_calls(),
            [input_opened()],
            "デバイスとの接続を始めた後に入力ポートを開く必要があります。"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn waiting_output_is_retried_every_retry_period() {
        let backend = FakeBackend::unavailable();
        let resident = start(
            &config(),
            &backend,
            &input_fake::FakeBackend::unavailable(),
            &FakeDevice::default(),
        );
        backend.take_calls();
        let (stop_tx, stop) = oneshot::channel();

        let script = async {
            sleep(RETRY_PERIOD - MOMENT).await;
            let before_period = backend.take_calls();
            sleep(MOMENT * 2).await;
            let first_period = backend.take_calls();
            sleep(RETRY_PERIOD).await;
            let second_period = backend.take_calls();
            stop_tx
                .send(())
                .expect("serve が動いている必要があります。");
            (before_period, first_period, second_period)
        };
        let ((), (before_period, first_period, second_period)) =
            tokio::join!(serve(resident, unwatched(config()), stopped(stop)), script);

        assert!(
            before_period.is_empty(),
            "周期が来る前には開き直さない必要があります: {before_period:?}"
        );
        assert_eq!(
            first_period,
            [opened()],
            "周期ごとに開き直す必要があります。"
        );
        assert_eq!(
            second_period,
            [opened()],
            "開けない間は周期ごとに開き直し続ける必要があります。"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn modifier_released_while_output_is_waiting_does_not_remain_after_recovery() {
        let backend = FakeBackend::openable(PORT);
        let device = FakeDevice::default();
        let resident = start(
            &config(),
            &backend,
            &input_fake::FakeBackend::unavailable(),
            &device,
        );
        let events = device.events();
        let (stop_tx, stop) = oneshot::channel();

        let script = async {
            emit(&events, press(Button::Side)).await;
            // loopMIDI の終了に当たる。次の周期の一覧の再評価で消失を検知する
            backend.set_listed(Some(&[]));
            backend.set_openable(None);
            sleep(RETRY_PERIOD + MOMENT).await;
            emit(&events, release(Button::Side)).await;
            backend.set_listed(Some(&[PORT]));
            backend.set_openable(Some(PORT));
            sleep(RETRY_PERIOD).await;
            emit(&events, press(Button::Top)).await;
            sleep(MOMENT).await;
            stop_tx
                .send(())
                .expect("serve が動いている必要があります。");
        };
        tokio::join!(serve(resident, unwatched(config()), stopped(stop)), script);

        assert_eq!(
            backend.take_calls(),
            [
                opened(),
                sent(note_on(50)),
                Call::List,
                closed(),
                opened(),
                // 待機中に離した Side の Off は台帳から送る
                sent(note_off(50)),
                // Side の修飾は解除されているので、Top は基本レイヤの CC 20 になる
                sent(cc(20, 127)),
                sent(cc(20, 0)),
                closed(),
            ],
            "出力の待機中もイベントを engine に渡し、待機中に離した修飾を復帰後に残さない必要があります。"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn disconnect_while_output_is_waiting_resets_engine_state() {
        let backend = FakeBackend::unavailable();
        let device = FakeDevice::default();
        let resident = start(
            &config(),
            &backend,
            &input_fake::FakeBackend::unavailable(),
            &device,
        );
        let events = device.events();
        let (stop_tx, stop) = oneshot::channel();

        let script = async {
            emit(&events, press(Button::Side)).await;
            emit(&events, DeviceEvent::Disconnected).await;
            emit(&events, DeviceEvent::Connected).await;
            backend.set_listed(Some(&[PORT]));
            backend.set_openable(Some(PORT));
            sleep(RETRY_PERIOD + MOMENT).await;
            emit(&events, press(Button::Top)).await;
            sleep(MOMENT).await;
            stop_tx
                .send(())
                .expect("serve が動いている必要があります。");
        };
        tokio::join!(serve(resident, unwatched(config()), stopped(stop)), script);

        assert_eq!(
            backend.take_calls(),
            [
                opened(),
                opened(),
                sent(cc(20, 127)),
                sent(cc(20, 0)),
                closed(),
            ],
            "待機中の切断で engine の修飾と押下の記憶を初期化し、復帰時に Off を送らない必要があります。"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stop_releases_held_buttons_closes_port_and_stops_device() {
        let backend = FakeBackend::unavailable();
        let device = FakeDevice::default();
        let resident = start(
            &config(),
            &backend,
            &input_fake::FakeBackend::unavailable(),
            &device,
        );
        let events = device.events();
        let (stop_tx, stop) = oneshot::channel();

        let script = async {
            emit(&events, press(Button::Side)).await;
            backend.set_listed(Some(&[PORT]));
            backend.set_openable(Some(PORT));
            sleep(RETRY_PERIOD + MOMENT).await;
            emit(&events, press(Button::Top)).await;
            sleep(MOMENT).await;
            stop_tx
                .send(())
                .expect("serve が動いている必要があります。");
        };
        tokio::join!(serve(resident, unwatched(config()), stopped(stop)), script);

        assert_eq!(
            backend.take_calls(),
            [
                opened(),
                opened(),
                // 待機中に押した Side の修飾が有効なので、Top は Note 70 になる
                sent(note_on(70)),
                // Side の On は送っていないので台帳になく、この Off は engine の release_all から出る
                sent(note_off(50)),
                sent(note_off(70)),
                closed(),
            ],
            "停止要求で押下中のボタンの Off を送ってからポートを閉じる必要があります。"
        );
        assert_eq!(
            device.take_calls(),
            [started(&config()), DeviceCall::Stopped],
            "停止要求でデバイスとの接続を止める必要があります。"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn received_control_change_changes_haptics_and_other_messages_are_ignored() {
        let backend = FakeBackend::openable(PORT);
        let input_backend = input_fake::FakeBackend::openable(INPUT_PORT);
        let device = FakeDevice::default();
        let resident = start(&config_with_input(), &backend, &input_backend, &device);
        device.take_calls();
        let (stop_tx, stop) = oneshot::channel();

        let script = async {
            // チャンネル 2 で Knob の強度をなしにする CC の後に、CC として読めば強度を変える
            // Note On と、別のチャンネルの CC と、割り当てのない CC と、設定を変えない同じ CC を送る
            for bytes in [
                [0xb1, 100, 0],
                [0x91, 100, 127],
                [0xb0, 100, 127],
                [0xb1, 101, 0],
                [0xb1, 100, 0],
            ] {
                input_backend.receive(&bytes);
            }
            sleep(MOMENT).await;
            stop_tx
                .send(())
                .expect("serve が動いている必要があります。");
        };
        tokio::join!(
            serve(resident, unwatched(config_with_input()), stopped(stop)),
            script
        );

        assert_eq!(
            device.take_calls(),
            [DeviceCall::SetHaptics(knob_off()), DeviceCall::Stopped],
            "設定を変える Control Change のときだけ、変わったハプティクス設定をデバイスに渡す必要があります。"
        );
        assert_eq!(
            input_backend.take_calls(),
            [input_opened(), input_closed()],
            "起動時に入力ポートを開き、停止時に閉じる必要があります。"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn waiting_input_is_retried_every_retry_period() {
        let backend = FakeBackend::openable(PORT);
        let input_backend = input_fake::FakeBackend::unavailable();
        let resident = start(
            &config_with_input(),
            &backend,
            &input_backend,
            &FakeDevice::default(),
        );
        input_backend.take_calls();
        let (stop_tx, stop) = oneshot::channel();

        let script = async {
            sleep(RETRY_PERIOD - MOMENT).await;
            let before_period = input_backend.take_calls();
            sleep(MOMENT * 2).await;
            let first_period = input_backend.take_calls();
            sleep(RETRY_PERIOD).await;
            let second_period = input_backend.take_calls();
            stop_tx
                .send(())
                .expect("serve が動いている必要があります。");
            (before_period, first_period, second_period)
        };
        let ((), (before_period, first_period, second_period)) = tokio::join!(
            serve(resident, unwatched(config_with_input()), stopped(stop)),
            script
        );

        assert!(
            before_period.is_empty(),
            "周期が来る前には入力ポートを開き直さない必要があります: {before_period:?}"
        );
        assert_eq!(
            first_period,
            [input_opened()],
            "周期ごとに入力ポートを開き直す必要があります。"
        );
        assert_eq!(
            second_period,
            [input_opened()],
            "開けない間は周期ごとに入力ポートを開き直し続ける必要があります。"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn input_changes_haptics_while_output_is_waiting() {
        let backend = FakeBackend::unavailable();
        let input_backend = input_fake::FakeBackend::openable(INPUT_PORT);
        let device = FakeDevice::default();
        let resident = start(&config_with_input(), &backend, &input_backend, &device);
        device.take_calls();
        let (stop_tx, stop) = oneshot::channel();

        let script = async {
            input_backend.receive(&[0xb1, 100, 0]);
            sleep(MOMENT).await;
            stop_tx
                .send(())
                .expect("serve が動いている必要があります。");
        };
        tokio::join!(
            serve(resident, unwatched(config_with_input()), stopped(stop)),
            script
        );

        assert_eq!(
            device.take_calls(),
            [DeviceCall::SetHaptics(knob_off()), DeviceCall::Stopped],
            "出力ポートの待機中も、受信した Control Change でハプティクス設定を変える必要があります。"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn input_is_disabled_without_midi_input() {
        let backend = FakeBackend::openable(PORT);
        let input_backend = input_fake::FakeBackend::openable(INPUT_PORT);
        let resident = start(&config(), &backend, &input_backend, &FakeDevice::default());
        let (stop_tx, stop) = oneshot::channel();

        let script = async {
            sleep(RETRY_PERIOD * 2 + MOMENT).await;
            stop_tx
                .send(())
                .expect("serve が動いている必要があります。");
        };
        tokio::join!(serve(resident, unwatched(config()), stopped(stop)), script);

        assert_eq!(
            input_backend.take_calls(),
            [],
            "midi.input がなければ入力ポートを開かない必要があります。"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn reload_notification_applies_new_config_to_running_components() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れる必要があります。");
        let path = dir.path().join("config.toml");
        let backend = FakeBackend::openable(PORT);
        let input_backend = input_fake::FakeBackend::openable(INPUT_PORT);
        let device = FakeDevice::default();
        let resident = start(&config_with_input(), &backend, &input_backend, &device);
        backend.take_calls();
        input_backend.take_calls();
        device.take_calls();
        let (config, changes) = watched(&path, config_with_input());
        let old_events = device.events();
        let (stop_tx, stop) = oneshot::channel();

        let script = async {
            emit(&old_events, press(Button::Side)).await;
            sleep(MOMENT).await;
            // 出力ポート、Top の割り当て、Knob の強度、接続方式を変える
            fs::write(
                &path,
                format!(
                    "[device]\ntransport = \"usb\"\n[midi]\noutput = \"{NEW_PORT}\"\ninput = \"{INPUT_PORT}\"\nchannel = 1\n[haptics]\nknob = {{ strength = \"off\" }}\n[map]\nside = {{ note = 50 }}\ntop = {{ cc = 21 }}\n[map.with.side]\ntop = {{ note = 70 }}\n"
                ),
            )
            .expect("設定ファイルを書き込める必要があります。");
            backend.set_openable(Some(NEW_PORT));
            changes
                .send(())
                .await
                .expect("serve が通知を受け取れる必要があります。");
            sleep(STOP_DELAY + MOMENT).await;
            emit(&device.events(), press(Button::Top)).await;
            sleep(MOMENT).await;
            stop_tx
                .send(())
                .expect("serve が動いている必要があります。");
        };
        tokio::join!(serve(resident, config, stopped(stop)), script);

        let new_port = |message| Call::Send(NEW_PORT.to_owned(), message);
        assert_eq!(
            backend.take_calls(),
            [
                sent(note_on(50)),
                // 再読込の手順 1 で押下中の Side の Off を古いポートへ送ってから開き直す
                sent(note_off(50)),
                closed(),
                Call::Open(NEW_PORT.to_owned(), PortMode::Existing),
                // Side の修飾は解除され、新しい割り当ての CC 21 になる
                new_port(cc(21, 127)),
                new_port(cc(21, 0)),
                Call::Close(NEW_PORT.to_owned()),
            ],
            "再読込で押下中のボタンを解放し、新しい割り当てと出力ポートに切り替える必要があります。"
        );
        assert_eq!(
            input_backend.take_calls(),
            [input_closed(), input_opened(), input_closed()],
            "midi.input が同じでも、Existing の入力ポートは再読込で開き直す必要があります。"
        );
        let usb = ConnectionConfig {
            transport: TransportKind::Usb,
            usb_port: None,
        };
        assert_eq!(
            device.take_calls(),
            [
                DeviceCall::SetHaptics(knob_off()),
                DeviceCall::Stopped,
                DeviceCall::Start(usb, knob_off()),
                DeviceCall::Stopped,
            ],
            "ハプティクスの基準値を送り、デバイスとの接続を止め終えてから新しい設定で始める必要があります。"
        );
        assert!(
            old_events.is_closed(),
            "接続をやり直した後は、古い接続のイベントを受け取らない必要があります。"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn reload_sends_offs_of_held_buttons_even_when_output_is_unchanged() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れる必要があります。");
        let path = dir.path().join("config.toml");
        fs::write(&path, text()).expect("設定ファイルを書き込める必要があります。");
        let backend = FakeBackend::openable(PORT);
        let device = FakeDevice::default();
        let resident = start(
            &config(),
            &backend,
            &input_fake::FakeBackend::unavailable(),
            &device,
        );
        backend.take_calls();
        let (config, changes) = watched(&path, config());
        let events = device.events();
        let (stop_tx, stop) = oneshot::channel();

        let script = async {
            emit(&events, press(Button::Side)).await;
            emit(&events, press(Button::Top)).await;
            sleep(MOMENT).await;
            let before_reload = backend.take_calls();
            changes
                .send(())
                .await
                .expect("serve が通知を受け取れる必要があります。");
            sleep(MOMENT).await;
            let after_reload = backend.take_calls();
            stop_tx
                .send(())
                .expect("serve が動いている必要があります。");
            (before_reload, after_reload)
        };
        let ((), (before_reload, after_reload)) =
            tokio::join!(serve(resident, config, stopped(stop)), script);

        assert_eq!(
            before_reload,
            [sent(note_on(50)), sent(note_on(70))],
            "Side の修飾中に Top を押すと Note 70 になる必要があります。"
        );
        assert_eq!(
            after_reload,
            [sent(note_off(50)), sent(note_off(70))],
            "出力ポートが変わらない再読込でも、押下中のボタンの Off をその時点で送る必要があります。"
        );
        assert_eq!(
            backend.take_calls(),
            [closed()],
            "再読込で解放したボタンの Off は、終了時に送り直さない必要があります。"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn reload_disables_and_reenables_input_port() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れる必要があります。");
        let path = dir.path().join("config.toml");
        let input_backend = input_fake::FakeBackend::openable(INPUT_PORT);
        let device = FakeDevice::default();
        let resident = start(
            &config_with_input(),
            &FakeBackend::openable(PORT),
            &input_backend,
            &device,
        );
        input_backend.take_calls();
        let (config, changes) = watched(&path, config_with_input());
        let (stop_tx, stop) = oneshot::channel();

        let script = async {
            let mut observed = Vec::new();
            for input in [None, Some(INPUT_PORT)] {
                fs::write(&path, text_with_input(input))
                    .expect("設定ファイルを書き込める必要があります。");
                changes
                    .send(())
                    .await
                    .expect("serve が通知を受け取れる必要があります。");
                sleep(MOMENT).await;
                observed.push(input_backend.take_calls());
            }
            device.take_calls();
            input_backend.receive(&[0xb1, 100, 0]);
            sleep(MOMENT).await;
            stop_tx
                .send(())
                .expect("serve が動いている必要があります。");
            observed
        };
        let ((), observed) = tokio::join!(serve(resident, config, stopped(stop)), script);

        assert_eq!(
            observed,
            [vec![input_closed()], vec![input_opened()]],
            "midi.input を消した再読込で入力ポートを閉じ、戻した再読込で開く必要があります。"
        );
        assert_eq!(
            device.take_calls(),
            [DeviceCall::SetHaptics(knob_off()), DeviceCall::Stopped],
            "入力機能を有効に戻した後も、受信した Control Change でハプティクス設定を変える必要があります。"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn reload_while_output_is_waiting_opens_corrected_port_and_stop_still_works() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れる必要があります。");
        let path = dir.path().join("config.toml");
        let wrong = parse(&format!(
            "[midi]\noutput = \"Wrong Port\"\nchannel = 1\n[map]\n{MAP}"
        ));
        let backend = FakeBackend::unavailable();
        let resident = start(
            &wrong,
            &backend,
            &input_fake::FakeBackend::unavailable(),
            &FakeDevice::default(),
        );
        backend.take_calls();
        let (config, changes) = watched(&path, wrong);
        let (stop_tx, stop) = oneshot::channel();

        let script = async {
            // 最初の再試行の周期より前に、ポート名を直した設定ファイルを保存する
            sleep(RETRY_PERIOD / 5).await;
            fs::write(&path, text()).expect("設定ファイルを書き込める必要があります。");
            backend.set_openable(Some(PORT));
            changes
                .send(())
                .await
                .expect("serve が通知を受け取れる必要があります。");
            sleep(MOMENT).await;
            let after_reload = backend.take_calls();
            stop_tx
                .send(())
                .expect("serve が動いている必要があります。");
            after_reload
        };
        let ((), after_reload) = tokio::join!(serve(resident, config, stopped(stop)), script);

        assert_eq!(
            after_reload,
            [opened()],
            "出力ポートの待機中も再読込が走り、直した名前のポートを周期を待たずに開く必要があります。"
        );
        assert_eq!(
            backend.take_calls(),
            [closed()],
            "再読込の後も Ctrl+C で終了し、開いたポートを閉じる必要があります。"
        );
    }
}
