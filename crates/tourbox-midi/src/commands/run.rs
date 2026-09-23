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
use tourbox::protocol::Event;
use tracing::info;

use crate::config::{default_config_path, Config};
use crate::engine::Engine;
use crate::midi::PortMode;
use crate::output::{MidiOutBackend, OutputBackend, OutputState};

/// 出力ポートを開き直す周期。`Existing` では接続中の一覧の再評価もこの周期で行う。
const RETRY_PERIOD: Duration = Duration::from_secs(5);
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

/// Ctrl+C の受付、MIDI 出力の準備、デバイス接続の順に始めて常駐ループを回し、終わったらデバイスを止める。
async fn reside(config: Config) -> anyhow::Result<()> {
    let stop = listen_ctrl_c().context("Ctrl+C の受付を開始できませんでした。")?;
    let mut output = OutputState::new(
        MidiOutBackend,
        config.midi.output.clone(),
        PortMode::default_for_os(),
    );
    output.tick();
    let (events, device) = Device::run(config.to_connection_config(), config.to_haptic_config());
    info!("常駐を開始しました。Ctrl+C で終了します。");
    serve(Engine::new(config.resolve_mapping()), output, events, stop).await;
    device.shutdown().await;
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

/// 常駐ループ。`stop` が完了するまで、デバイスのイベントを engine で変換して出力し、
/// [`RETRY_PERIOD`] ごとに出力ポートを再試行する。
///
/// 終わるときは押下中のボタンと台帳の Off を送ってからポートを閉じる。
async fn serve<B: OutputBackend>(
    mut engine: Engine,
    mut output: OutputState<B>,
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
            _ = retry.tick() => output.tick(),
        }
    }
    output.shutdown(engine.release_all());
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
    use tokio::sync::oneshot;
    use tokio::time::sleep;
    use tourbox::protocol::Button;

    use super::*;
    use crate::midi_msg::MidiMessage;
    use crate::output::fake::{Call, FakeBackend};

    const PORT: &str = "loopMIDI Port";
    /// Side を修飾ボタンにする。Top は基本レイヤでは CC 20、Side の修飾中は Note 70 になる。
    const MAP: &str =
        "side = { note = 50 }\ntop = { cc = 20 }\n[map.with.side]\ntop = { note = 70 }\n";
    /// 時間を止めたテストで、送ったイベントを serve に処理させてから次へ進むための短い待ち。
    const MOMENT: Duration = Duration::from_millis(1);

    fn engine() -> Engine {
        let text = format!("[midi]\noutput = \"{PORT}\"\nchannel = 1\n[map]\n{MAP}");
        let config = Config::parse(&text, Path::new("config.toml"))
            .unwrap_or_else(|error| panic!("検証に通る必要があります: {error}"));
        Engine::new(config.resolve_mapping())
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
        let ((), (before_period, first_period, second_period)) =
            tokio::join!(serve(engine(), output, events, stopped(stop)), script);

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
        tokio::join!(serve(engine(), output, events, stopped(stop)), script);

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
        tokio::join!(serve(engine(), output, events, stopped(stop)), script);

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
        tokio::join!(serve(engine(), output, events, stopped(stop)), script);

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
}
