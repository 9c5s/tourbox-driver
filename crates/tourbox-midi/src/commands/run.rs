//! 常駐コマンド。設定の割り当てに従って、TourBox の操作を MIDI メッセージに変換し続ける。

use std::future::Future;
use std::io;
use std::path::Path;
use std::pin::pin;
use std::time::Duration;

use anyhow::Context;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tokio::time::{self, Instant, MissedTickBehavior};
use tourbox::device::{Device, DeviceEvent};
use tourbox::protocol::{Event, HapticConfig};
use tracing::{debug, info};

use crate::config::{default_config_path, Config};
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
    runtime.block_on(reside(config))
}

/// Ctrl+C の受付、MIDI 出力の準備、デバイス接続、MIDI 入力の準備の順に始めて常駐ループを回し、
/// 終わったらデバイスを止める。
async fn reside(config: Config) -> anyhow::Result<()> {
    let stop = listen_ctrl_c().context("Ctrl+C の受付を開始できませんでした。")?;
    let mut output = OutputState::new(
        MidiOutBackend,
        config.midi.output.clone(),
        PortMode::default_for_os(),
    );
    output.tick();
    let (events, device) = Device::run(config.to_connection_config(), config.to_haptic_config());
    let input = prepare_input(MidiInBackend, PortMode::default_for_os(), &config);
    info!("常駐を開始しました。Ctrl+C で終了します。");
    serve(
        Engine::new(config.resolve_mapping()),
        output,
        input,
        |haptics| device.set_haptics(haptics),
        events,
        stop,
    )
    .await;
    device.shutdown().await;
    Ok(())
}

/// 入力ポートの状態 (最初の tick まで済ませる) とハプティクス制御を作る。
///
/// `midi.input` がなければ入力ポートを持たず、入力機能は無効である。
fn prepare_input<I: InputBackend>(backend: I, mode: PortMode, config: &Config) -> HapticsInput<I> {
    let (tx, received) = mpsc::channel(INPUT_CAPACITY);
    let port = config.midi.input.clone().map(|name| {
        let mut port = InputState::new(backend, name, mode, tx);
        port.tick();
        port
    });
    HapticsInput {
        port,
        received,
        controller: HapticsController::new(config.haptics_control(), config.to_haptic_config()),
    }
}

/// MIDI 入力の受信と、受信した Control Change によるハプティクス制御 (設計書 6.2 節)。
struct HapticsInput<I: InputBackend> {
    /// None は入力機能が無効。
    port: Option<InputState<I>>,
    /// 入力ポートが受信したバイト列。
    received: mpsc::Receiver<Vec<u8>>,
    controller: HapticsController,
}

impl<I: InputBackend> HapticsInput<I> {
    fn tick(&mut self) {
        if let Some(port) = &mut self.port {
            port.tick();
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

    fn shutdown(self) {
        if let Some(port) = self.port {
            port.shutdown();
        }
    }
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

/// 常駐ループ。`stop` が完了するまで、デバイスのイベントを engine で変換して出力し、
/// 入力ポートが受信した Control Change で変わったハプティクス設定を `set_haptics` に渡し、
/// [`RETRY_PERIOD`] ごとに出力ポートと入力ポートを再試行する。
///
/// 終わるときは押下中のボタンと台帳の Off を送ってから出力ポートを閉じ、入力ポートを閉じる。
async fn serve<B: OutputBackend, I: InputBackend>(
    mut engine: Engine,
    mut output: OutputState<B>,
    mut input: HapticsInput<I>,
    set_haptics: impl Fn(HapticConfig),
    mut events: mpsc::Receiver<DeviceEvent>,
    stop: impl Future<Output = ()>,
) {
    let mut retry = time::interval_at(Instant::now() + RETRY_PERIOD, RETRY_PERIOD);
    retry.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut stop = pin!(stop);
    loop {
        tokio::select! {
            () = &mut stop => {
                info!("Ctrl+C を受け付けました。押下中のボタンを解放して終了します。");
                break;
            }
            received = events.recv() => {
                // device のタスクは停止要求まで終わらない。終わるのはパニックしたときで、shutdown がそのパニックを再開する
                let Some(event) = received else { break };
                log_device_event(&event);
                for outgoing in engine.handle(event) {
                    output.send(outgoing);
                }
            }
            // 入力機能が無効なら送り手がなく None になるので、この腕は選ばれない
            Some(bytes) = input.received.recv() => {
                if let Some(haptics) = input.handle(&bytes) {
                    set_haptics(haptics);
                }
            }
            _ = retry.tick() => {
                output.tick();
                input.tick();
            }
        }
    }
    output.shutdown(engine.release_all());
    input.shutdown();
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

    use tokio::sync::oneshot;
    use tokio::time::sleep;
    use tourbox::protocol::{Axis, Button, Modifier, Strength};

    use super::*;
    use crate::input::fake as input_fake;
    use crate::midi_msg::MidiMessage;
    use crate::output::fake::{Call, FakeBackend};

    const PORT: &str = "loopMIDI Port";
    const INPUT_PORT: &str = "loopMIDI TourBox In";
    /// Side を修飾ボタンにする。Top は基本レイヤでは CC 20、Side の修飾中は Note 70 になる。
    const MAP: &str =
        "side = { note = 50 }\ntop = { cc = 20 }\n[map.with.side]\ntop = { note = 70 }\n";
    /// 時間を止めたテストで、送ったイベントを serve に処理させてから次へ進むための短い待ち。
    const MOMENT: Duration = Duration::from_millis(1);

    fn parse(text: &str) -> Config {
        Config::parse(text, Path::new("config.toml"))
            .unwrap_or_else(|error| panic!("検証に通る必要があります: {error}"))
    }

    /// 入力ポートのない設定。
    fn config() -> Config {
        parse(&format!(
            "[midi]\noutput = \"{PORT}\"\nchannel = 1\n[map]\n{MAP}"
        ))
    }

    /// 入力ポートがあり、チャンネル 2 の CC 100 で Knob の強度を制御する設定。
    fn config_with_input() -> Config {
        parse(&format!(
            "[midi]\noutput = \"{PORT}\"\ninput = \"{INPUT_PORT}\"\nchannel = 1\n[map]\n{MAP}[haptics.control]\nchannel = 2\nknob = {{ cc = 100 }}\n"
        ))
    }

    fn engine() -> Engine {
        Engine::new(config().resolve_mapping())
    }

    /// run の起動手順と同じく、入力ポートを 1 度 tick した入力を作る。
    fn input_after_first_tick(
        backend: &input_fake::FakeBackend,
        config: &Config,
    ) -> HapticsInput<input_fake::FakeBackend> {
        prepare_input(backend.clone(), PortMode::Existing, config)
    }

    /// 入力ポートのない設定の入力。
    fn disabled_input() -> HapticsInput<input_fake::FakeBackend> {
        input_after_first_tick(&input_fake::FakeBackend::unavailable(), &config())
    }

    /// 既定のハプティクス設定から、Knob の全組み合わせの強度をなしにした設定。
    fn knob_off() -> HapticConfig {
        let mut haptics = HapticConfig::default();
        for modifier in Modifier::ALL {
            haptics.set_strength(Axis::Knob, modifier, Strength::Off);
        }
        haptics
    }

    fn input_opened() -> input_fake::Call {
        input_fake::Call::Open(INPUT_PORT.to_owned(), PortMode::Existing)
    }

    fn input_closed() -> input_fake::Call {
        input_fake::Call::Close(INPUT_PORT.to_owned())
    }

    /// run の起動手順と同じく、serve の前に 1 度 tick した出力を作る。
    fn output_after_first_tick(backend: &FakeBackend) -> OutputState<FakeBackend> {
        let mut output = OutputState::new(backend.clone(), PORT.to_owned(), PortMode::Existing);
        output.tick();
        output
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

    fn note_on(note: u8) -> Call {
        sent(MidiMessage::NoteOn {
            channel: 0,
            note,
            velocity: 127,
        })
    }

    fn note_off(note: u8) -> Call {
        sent(MidiMessage::NoteOff { channel: 0, note })
    }

    fn cc(cc: u8, value: u8) -> Call {
        sent(MidiMessage::ControlChange {
            channel: 0,
            cc,
            value,
        })
    }

    #[tokio::test(start_paused = true)]
    async fn waiting_output_is_retried_every_retry_period() {
        let backend = FakeBackend::unavailable();
        let output = output_after_first_tick(&backend);
        backend.take_calls();
        let (_events_tx, events) = mpsc::channel(16);
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
        let ((), (before_period, first_period, second_period)) = tokio::join!(
            serve(
                engine(),
                output,
                disabled_input(),
                |_| {},
                events,
                stopped(stop)
            ),
            script
        );

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
        let output = output_after_first_tick(&backend);
        let (events_tx, events) = mpsc::channel(16);
        let (stop_tx, stop) = oneshot::channel();

        let script = async {
            emit(&events_tx, press(Button::Side)).await;
            // loopMIDI の終了に当たる。次の周期の一覧の再評価で消失を検知する
            backend.set_listed(Some(&[]));
            backend.set_openable(None);
            sleep(RETRY_PERIOD + MOMENT).await;
            emit(&events_tx, release(Button::Side)).await;
            backend.set_listed(Some(&[PORT]));
            backend.set_openable(Some(PORT));
            sleep(RETRY_PERIOD).await;
            emit(&events_tx, press(Button::Top)).await;
            sleep(MOMENT).await;
            stop_tx
                .send(())
                .expect("serve が動いている必要があります。");
        };
        tokio::join!(
            serve(
                engine(),
                output,
                disabled_input(),
                |_| {},
                events,
                stopped(stop)
            ),
            script
        );

        assert_eq!(
            backend.take_calls(),
            [
                opened(),
                note_on(50),
                Call::List,
                closed(),
                opened(),
                // 待機中に離した Side の Off は台帳から送る
                note_off(50),
                // Side の修飾は解除されているので、Top は基本レイヤの CC 20 になる
                cc(20, 127),
                cc(20, 0),
                closed(),
            ],
            "出力の待機中もイベントを engine に渡し、待機中に離した修飾を復帰後に残さない必要があります。"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn disconnect_while_output_is_waiting_resets_engine_state() {
        let backend = FakeBackend::unavailable();
        let output = output_after_first_tick(&backend);
        let (events_tx, events) = mpsc::channel(16);
        let (stop_tx, stop) = oneshot::channel();

        let script = async {
            emit(&events_tx, press(Button::Side)).await;
            emit(&events_tx, DeviceEvent::Disconnected).await;
            emit(&events_tx, DeviceEvent::Connected).await;
            backend.set_listed(Some(&[PORT]));
            backend.set_openable(Some(PORT));
            sleep(RETRY_PERIOD + MOMENT).await;
            emit(&events_tx, press(Button::Top)).await;
            sleep(MOMENT).await;
            stop_tx
                .send(())
                .expect("serve が動いている必要があります。");
        };
        tokio::join!(
            serve(
                engine(),
                output,
                disabled_input(),
                |_| {},
                events,
                stopped(stop)
            ),
            script
        );

        assert_eq!(
            backend.take_calls(),
            [
                opened(),
                opened(),
                cc(20, 127),
                cc(20, 0),
                closed(),
            ],
            "待機中の切断で engine の修飾と押下の記憶を初期化し、復帰時に Off を送らない必要があります。"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stop_releases_held_buttons_and_closes_port() {
        let backend = FakeBackend::unavailable();
        let output = output_after_first_tick(&backend);
        let (events_tx, events) = mpsc::channel(16);
        let (stop_tx, stop) = oneshot::channel();

        let script = async {
            emit(&events_tx, press(Button::Side)).await;
            backend.set_listed(Some(&[PORT]));
            backend.set_openable(Some(PORT));
            sleep(RETRY_PERIOD + MOMENT).await;
            emit(&events_tx, press(Button::Top)).await;
            sleep(MOMENT).await;
            stop_tx
                .send(())
                .expect("serve が動いている必要があります。");
        };
        tokio::join!(
            serve(
                engine(),
                output,
                disabled_input(),
                |_| {},
                events,
                stopped(stop)
            ),
            script
        );

        assert_eq!(
            backend.take_calls(),
            [
                opened(),
                opened(),
                // 待機中に押した Side の修飾が有効なので、Top は Note 70 になる
                note_on(70),
                // Side の On は送っていないので台帳になく、この Off は engine の release_all から出る
                note_off(50),
                note_off(70),
                closed(),
            ],
            "停止要求で押下中のボタンの Off を送ってからポートを閉じる必要があります。"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn received_control_change_changes_haptics_and_other_messages_are_ignored() {
        let backend = FakeBackend::openable(PORT);
        let output = output_after_first_tick(&backend);
        let input_backend = input_fake::FakeBackend::openable(INPUT_PORT);
        let input = input_after_first_tick(&input_backend, &config_with_input());
        let (_events_tx, events) = mpsc::channel(16);
        let (stop_tx, stop) = oneshot::channel();
        let applied = RefCell::new(Vec::new());

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
            serve(
                engine(),
                output,
                input,
                |haptics| applied.borrow_mut().push(haptics),
                events,
                stopped(stop)
            ),
            script
        );

        assert_eq!(
            applied.into_inner(),
            [knob_off()],
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
        let output = output_after_first_tick(&backend);
        let input_backend = input_fake::FakeBackend::unavailable();
        let input = input_after_first_tick(&input_backend, &config_with_input());
        input_backend.take_calls();
        let (_events_tx, events) = mpsc::channel(16);
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
            serve(engine(), output, input, |_| {}, events, stopped(stop)),
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
        let output = output_after_first_tick(&backend);
        let input_backend = input_fake::FakeBackend::openable(INPUT_PORT);
        let input = input_after_first_tick(&input_backend, &config_with_input());
        let (_events_tx, events) = mpsc::channel(16);
        let (stop_tx, stop) = oneshot::channel();
        let applied = RefCell::new(Vec::new());

        let script = async {
            input_backend.receive(&[0xb1, 100, 0]);
            sleep(MOMENT).await;
            stop_tx
                .send(())
                .expect("serve が動いている必要があります。");
        };
        tokio::join!(
            serve(
                engine(),
                output,
                input,
                |haptics| applied.borrow_mut().push(haptics),
                events,
                stopped(stop)
            ),
            script
        );

        assert_eq!(
            applied.into_inner(),
            [knob_off()],
            "出力ポートの待機中も、受信した Control Change でハプティクス設定を変える必要があります。"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn input_is_disabled_without_midi_input() {
        let backend = FakeBackend::openable(PORT);
        let output = output_after_first_tick(&backend);
        let input_backend = input_fake::FakeBackend::openable(INPUT_PORT);
        let input = input_after_first_tick(&input_backend, &config());
        let (_events_tx, events) = mpsc::channel(16);
        let (stop_tx, stop) = oneshot::channel();

        let script = async {
            sleep(RETRY_PERIOD * 2 + MOMENT).await;
            stop_tx
                .send(())
                .expect("serve が動いている必要があります。");
        };
        tokio::join!(
            serve(engine(), output, input, |_| {}, events, stopped(stop)),
            script
        );

        assert_eq!(
            input_backend.take_calls(),
            [],
            "midi.input がなければ入力ポートを開かない必要があります。"
        );
    }
}
