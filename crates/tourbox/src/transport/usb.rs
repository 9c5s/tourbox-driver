//! USB (CDC ACM の仮想シリアル) の接続。

use std::io::{self, Read, Write};
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use futures::future::BoxFuture;
use serialport::{SerialPort, SerialPortBuilder, SerialPortInfo, SerialPortType};
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::task::{JoinError, JoinHandle};
use tracing::{info, warn};

use super::{Incoming, Transport};
use crate::error::TransportError;

/// TourBox Elite の USB のベンダー ID。
const VID: u16 = 0xC251;
/// TourBox Elite の USB のプロダクト ID。
const PID: u16 = 0x2005;
/// ボーレート。CDC ACM では動作に影響しない。
const BAUD_RATE: u32 = 115_200;
/// 読み取りと書き込みの待ち時間。読み取りスレッドはこの間隔で停止要求を確認する。
const TIMEOUT: Duration = Duration::from_millis(100);
/// 1 回の読み取りで受け取る最大のバイト数。
const READ_BUFFER_SIZE: usize = 64;
/// 受信チャネルに溜められるかたまりの数。
const CHANNEL_CAPACITY: usize = 256;
/// 受信チャネルが満杯のときに、空きと停止要求を確認し直す間隔。
const FULL_RETRY_INTERVAL: Duration = Duration::from_millis(10);

/// 書き込みに使うポート。書き込みのたびに別スレッドへ渡す。
type SharedPort = Arc<Mutex<Box<dyn SerialPort>>>;

/// TourBox との USB の接続。
///
/// 開いたポートを書き込みに使い、読み取りスレッドにはその複製を渡す。
/// 複製を閉じるとポートの排他 (TIOCEXCL) が解除されるので、排他を保つため複製は接続ごとに 1 つにする。
pub struct UsbTransport {
    /// 書き込みに使うポート。`close` の後は `None`。
    port: Option<SharedPort>,
    receiver: Option<mpsc::Receiver<Incoming>>,
    /// 立てると、読み取りスレッドが次の読み取りの前に終わる。
    stop: Arc<AtomicBool>,
    /// 読み取りスレッド。`close` で終了を待った後は `None`。
    reader: Option<thread::JoinHandle<()>>,
    /// 完了を確かめていない書き込み。送信の future を破棄しても、次の送信と `close` はこの完了を待つ。
    writing: Option<JoinHandle<io::Result<()>>>,
}

impl UsbTransport {
    /// TourBox の USB のポートを開き、読み取りスレッドを起動する。
    ///
    /// `port` はポート名の明示指定で、`None` なら VID と PID で検出する。
    /// ポートが見つからなければ `TransportError::NotFound`、他のプロセスが使用中なら `TransportError::Busy` を返す。
    /// 呼び出しはポートを開き終えるまでブロックする。
    pub fn open(port: Option<&str>) -> Result<Self, TransportError> {
        let candidates = serialport::available_ports().map_err(io::Error::from)?;
        info!(ports = %describe_ports(&candidates), "シリアルポートを列挙しました。");
        let Some(selected) = select_usb_port(&candidates, port) else {
            return Err(TransportError::NotFound);
        };
        let path = selected.port_name.as_str();
        info!(port = path, "TourBox のポートを開きます。");
        let port = port_builder(path).open().map_err(|error| {
            warn!(port = path, "ポートを開けませんでした: {error}");
            open_error(error)
        })?;

        let reader_port = port.try_clone().map_err(io::Error::from)?;
        let (sender, receiver) = mpsc::channel(CHANNEL_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        let reader = thread::Builder::new()
            .name("tourbox-usb-reader".to_owned())
            .spawn({
                let stop = Arc::clone(&stop);
                move || read_loop(reader_port, &sender, &stop)
            })?;
        Ok(Self {
            port: Some(Arc::new(Mutex::new(port))),
            receiver: Some(receiver),
            stop,
            reader: Some(reader),
            writing: None,
        })
    }

    /// 書き込みに使うポートを返す。閉じた後や切断の後はエラーを返す。
    fn writable_port(&self) -> Result<SharedPort, TransportError> {
        let reading = self
            .reader
            .as_ref()
            .is_some_and(|reader| !reader.is_finished());
        match &self.port {
            Some(port) if reading => Ok(Arc::clone(port)),
            _ => Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "ポートは閉じているか、切断されています。",
            )
            .into()),
        }
    }

    /// 完了を確かめていない書き込みがあれば、終わるまで待って結果を返す。
    async fn finish_writing(&mut self) -> io::Result<()> {
        let Some(writing) = &mut self.writing else {
            return Ok(());
        };
        let result = writing.await;
        self.writing = None;
        resume_if_panicked(result)
    }
}

impl Transport for UsbTransport {
    fn send<'a>(&'a mut self, data: &'a [u8]) -> BoxFuture<'a, Result<(), TransportError>> {
        Box::pin(async move {
            // 破棄された送信の書き込みが残っていれば、バイト列が混ざらないように終わるまで待つ。その成否は問わない
            let _ = self.finish_writing().await;
            let port = self.writable_port()?;
            let data = data.to_vec();
            self.writing = Some(tokio::task::spawn_blocking(move || {
                let mut port = port.lock().unwrap_or_else(PoisonError::into_inner);
                port.write_all(&data)?;
                port.flush()
            }));
            Ok(self.finish_writing().await?)
        })
    }

    fn take_receiver(&mut self) -> Option<mpsc::Receiver<Incoming>> {
        self.receiver.take()
    }

    fn close(&mut self) -> BoxFuture<'_, Result<(), TransportError>> {
        Box::pin(async move {
            self.stop.store(true, Ordering::Relaxed);
            let _ = self.finish_writing().await;
            if let Some(reader) = self.reader.take() {
                let joined = tokio::task::spawn_blocking(move || reader.join()).await;
                if let Err(payload) = resume_if_panicked(joined) {
                    panic::resume_unwind(payload);
                }
            }
            self.port = None;
            Ok(())
        })
    }
}

impl Drop for UsbTransport {
    fn drop(&mut self) {
        // close を経ずに破棄された場合も、読み取りスレッドを終わらせてポートを解放する
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// 列挙したポートから TourBox のポートを選ぶ。
///
/// `requested` があれば名前が一致するポートを種類を問わずに返し、なければ VID と PID が一致する最初のポートを返す。
pub fn select_usb_port<'a>(
    candidates: &'a [SerialPortInfo],
    requested: Option<&str>,
) -> Option<&'a SerialPortInfo> {
    match requested {
        Some(name) => candidates.iter().find(|port| port.port_name == name),
        None => candidates.iter().find(|port| {
            matches!(&port.port_type, SerialPortType::UsbPort(usb) if usb.vid == VID && usb.pid == PID)
        }),
    }
}

/// TourBox のポートを開く設定 (115200、読み書きの待ち時間 100 ms、開くときに DTR を立てる) のビルダーを返す。
pub fn port_builder(path: &str) -> SerialPortBuilder {
    serialport::new(path, BAUD_RATE)
        .timeout(TIMEOUT)
        .dtr_on_open(true)
}

/// ログ用に、列挙したポートを名前と USB の VID:PID で表す。
fn describe_ports(ports: &[SerialPortInfo]) -> String {
    if ports.is_empty() {
        return "なし".to_owned();
    }
    ports
        .iter()
        .map(|port| match &port.port_type {
            SerialPortType::UsbPort(usb) => {
                format!("{} (USB {:04x}:{:04x})", port.port_name, usb.vid, usb.pid)
            }
            _ => port.port_name.clone(),
        })
        .collect::<Vec<_>>()
        .join("、")
}

/// 停止要求か切断まで、ポートから読み取ったかたまりを受信チャネルへ送る。
fn read_loop(mut port: Box<dyn SerialPort>, sender: &mpsc::Sender<Incoming>, stop: &AtomicBool) {
    let mut buffer = [0; READ_BUFFER_SIZE];
    while !stop.load(Ordering::Relaxed) {
        let result = port.read(&mut buffer);
        match classify_read(&result) {
            ReadStep::Data(len) => {
                if !deliver(sender, stop, Incoming::Data(buffer[..len].to_vec())) {
                    return;
                }
            }
            ReadStep::Retry => {}
            ReadStep::Disconnect => {
                // close による終了では切断を通知しない
                if !stop.load(Ordering::Relaxed) {
                    match result {
                        Err(error) => warn!("USB の読み取りでエラーが発生しました: {error}"),
                        Ok(_) => warn!("USB のポートの読み取りが終端に達しました。"),
                    }
                    deliver(sender, stop, Incoming::Disconnected);
                }
                return;
            }
        }
    }
}

/// `spawn_blocking` で実行した処理の結果を取り出す。処理がパニックしていたら、そのパニックを再開する。
fn resume_if_panicked<T>(result: Result<T, JoinError>) -> T {
    result.unwrap_or_else(|error| panic::resume_unwind(error.into_panic()))
}

/// 1 回の読み取りの結果に対する、読み取りスレッドの次の動作。
#[derive(Debug, PartialEq, Eq)]
enum ReadStep {
    /// 先頭から指定のバイト数を受信した。
    Data(usize),
    /// データがなかった。読み取りを続ける。
    Retry,
    /// 接続が切れた。
    Disconnect,
}

/// 読み取りの結果を、読み取りスレッドの次の動作に分ける。
///
/// serialport の `read` はデータがないと `TimedOut` を返すので、`TimedOut` と `Interrupted` は読み取りを続ける。
/// 0 バイト (終端) とそれ以外のエラーは、ポートの消失として切断にする。
fn classify_read(result: &io::Result<usize>) -> ReadStep {
    match result {
        Ok(0) => ReadStep::Disconnect,
        Ok(len) => ReadStep::Data(*len),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
            ) =>
        {
            ReadStep::Retry
        }
        Err(_) => ReadStep::Disconnect,
    }
}

/// ポートを開けなかったエラーを `TransportError` に変換する。
///
/// serialport は、Windows のアクセス拒否と、macOS の排他 (TIOCEXCL の EBUSY と flock の競合) を
/// `NoDevice` にするので、これを他のプロセスによる使用中とみなす。
/// Windows ではファイルとパスが見つからないエラーも `NoDevice` になるので、列挙の直後に抜かれたポートは 1 回分の再試行の間 `Busy` になる。
fn open_error(error: serialport::Error) -> TransportError {
    match error.kind() {
        serialport::ErrorKind::NoDevice => TransportError::Busy,
        _ => TransportError::Io(error.into()),
    }
}

/// `incoming` を受信チャネルへ送る。満杯なら空くまで待つが、停止要求があれば送らずに戻る。
///
/// 送れたら true、停止要求か受け取り側の破棄で送れなければ false を返す。
fn deliver(sender: &mpsc::Sender<Incoming>, stop: &AtomicBool, mut incoming: Incoming) -> bool {
    loop {
        match sender.try_send(incoming) {
            Ok(()) => return true,
            Err(TrySendError::Closed(_)) => return false,
            Err(TrySendError::Full(rejected)) => {
                if stop.load(Ordering::Relaxed) {
                    return false;
                }
                incoming = rejected;
                thread::sleep(FULL_RETRY_INTERVAL);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc as std_mpsc;

    use serialport::{ErrorKind as SerialErrorKind, UsbPortInfo};

    use super::*;

    fn usb_port(name: &str, vid: u16, pid: u16) -> SerialPortInfo {
        SerialPortInfo {
            port_name: name.to_owned(),
            port_type: SerialPortType::UsbPort(UsbPortInfo {
                vid,
                pid,
                serial_number: None,
                manufacturer: None,
                product: None,
            }),
        }
    }

    fn pci_port(name: &str) -> SerialPortInfo {
        SerialPortInfo {
            port_name: name.to_owned(),
            port_type: SerialPortType::PciPort,
        }
    }

    fn selected_name<'a>(
        candidates: &'a [SerialPortInfo],
        requested: Option<&str>,
    ) -> Option<&'a str> {
        select_usb_port(candidates, requested).map(|port| port.port_name.as_str())
    }

    #[test]
    fn selects_first_port_with_tourbox_vid_and_pid() {
        let candidates = [
            pci_port("COM1"),
            usb_port("COM2", 0x2341, 0x0043),
            usb_port("COM3", 0xC251, 0x2005),
            usb_port("COM4", 0xC251, 0x2005),
        ];

        assert_eq!(
            selected_name(&candidates, None),
            Some("COM3"),
            "VID と PID が一致する最初のポートを選ぶ必要があります。"
        );
    }

    #[test]
    fn ignores_ports_matching_only_vid_or_only_pid() {
        let candidates = [
            usb_port("COM2", 0xC251, 0x0001),
            usb_port("COM3", 0x0001, 0x2005),
        ];

        assert_eq!(
            selected_name(&candidates, None),
            None,
            "VID と PID の片方だけが一致するポートは選ばない必要があります。"
        );
    }

    #[test]
    fn selects_nothing_without_tourbox_port() {
        let candidates = [pci_port("COM1"), usb_port("COM2", 0x2341, 0x0043)];

        assert_eq!(
            selected_name(&candidates, None),
            None,
            "TourBox のポートがなければ何も選ばない必要があります。"
        );
    }

    #[test]
    fn requested_name_takes_precedence_over_vid_and_pid() {
        let candidates = [usb_port("COM3", 0xC251, 0x2005), pci_port("COM5")];

        assert_eq!(
            selected_name(&candidates, Some("COM5")),
            Some("COM5"),
            "明示指定があれば、VID と PID を問わずに名前が一致するポートを選ぶ必要があります。"
        );
    }

    #[test]
    fn requested_name_not_listed_selects_nothing_even_if_tourbox_exists() {
        let candidates = [usb_port("COM3", 0xC251, 0x2005)];

        assert_eq!(
            selected_name(&candidates, Some("COM9")),
            None,
            "明示指定のポートがなければ、自動検出に切り替えずに何も選ばない必要があります。"
        );
    }

    #[test]
    fn port_builder_sets_baud_rate_timeout_and_dtr() {
        let expected = serialport::new("COM3", 115_200)
            .timeout(Duration::from_millis(100))
            .dtr_on_open(true);

        assert_eq!(
            port_builder("COM3"),
            expected,
            "115200、待ち時間 100 ms、開くときに DTR を立てる設定にする必要があります。"
        );
    }

    #[test]
    fn read_with_bytes_yields_that_many_bytes() {
        assert_eq!(
            classify_read(&Ok(5)),
            ReadStep::Data(5),
            "読み取れたバイト数をそのまま受信として扱う必要があります。"
        );
    }

    #[test]
    fn timed_out_and_interrupted_reads_continue_reading() {
        for kind in [io::ErrorKind::TimedOut, io::ErrorKind::Interrupted] {
            assert_eq!(
                classify_read(&Err(kind.into())),
                ReadStep::Retry,
                "{kind:?} は切断ではなく、読み取りを続ける必要があります。"
            );
        }
    }

    #[test]
    fn other_read_errors_are_disconnects() {
        for kind in [
            io::ErrorKind::BrokenPipe,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::NotFound,
            io::ErrorKind::Other,
        ] {
            assert_eq!(
                classify_read(&Err(kind.into())),
                ReadStep::Disconnect,
                "{kind:?} は切断として扱う必要があります。"
            );
        }
    }

    #[test]
    fn end_of_file_is_disconnect() {
        assert_eq!(
            classify_read(&Ok(0)),
            ReadStep::Disconnect,
            "0 バイトの読み取り (EOF) は切断として扱う必要があります。"
        );
    }

    #[test]
    fn open_failure_with_no_device_is_busy() {
        let error = serialport::Error::new(SerialErrorKind::NoDevice, "アクセスが拒否されました。");

        let error = open_error(error);

        assert!(
            matches!(error, TransportError::Busy),
            "NoDevice は使用中として扱う必要があります: {error:?}"
        );
    }

    #[test]
    fn other_open_failures_are_io_errors_of_same_kind() {
        for (kind, expected) in [
            (
                SerialErrorKind::Io(io::ErrorKind::PermissionDenied),
                io::ErrorKind::PermissionDenied,
            ),
            (
                SerialErrorKind::Io(io::ErrorKind::NotFound),
                io::ErrorKind::NotFound,
            ),
            (SerialErrorKind::InvalidInput, io::ErrorKind::InvalidInput),
            (SerialErrorKind::Unknown, io::ErrorKind::Other),
        ] {
            let error = open_error(serialport::Error::new(kind, "原因"));

            assert!(
                matches!(&error, TransportError::Io(io) if io.kind() == expected),
                "{kind:?} は {expected:?} の入出力エラーにする必要があります: {error:?}"
            );
        }
    }

    /// `deliver` を別スレッドで実行し、戻り値を受け取るチャネルを返す。
    fn deliver_in_thread(
        sender: mpsc::Sender<Incoming>,
        stop: Arc<AtomicBool>,
    ) -> std_mpsc::Receiver<bool> {
        let (done_tx, done_rx) = std_mpsc::channel();
        thread::spawn(move || {
            let delivered = deliver(&sender, &stop, Incoming::Data(vec![2]));
            let _ = done_tx.send(delivered);
        });
        done_rx
    }

    #[test]
    fn deliver_waits_for_room_in_full_channel() {
        let (sender, mut receiver) = mpsc::channel(1);
        sender.try_send(Incoming::Data(vec![1])).unwrap();
        let done = deliver_in_thread(sender, Arc::new(AtomicBool::new(false)));

        assert!(
            done.recv_timeout(Duration::from_millis(50)).is_err(),
            "満杯の間は戻らずに空きを待つ必要があります。"
        );
        assert_eq!(receiver.try_recv().ok(), Some(Incoming::Data(vec![1])));
        assert_eq!(
            done.recv_timeout(Duration::from_secs(5)),
            Ok(true),
            "空きができたら送って true を返す必要があります。"
        );
        assert_eq!(
            receiver.try_recv().ok(),
            Some(Incoming::Data(vec![2])),
            "待っていたかたまりが届く必要があります。"
        );
    }

    #[test]
    fn deliver_stops_waiting_when_stop_is_requested() {
        let (sender, _receiver) = mpsc::channel(1);
        sender.try_send(Incoming::Data(vec![1])).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let done = deliver_in_thread(sender, Arc::clone(&stop));
        assert!(
            done.recv_timeout(Duration::from_millis(50)).is_err(),
            "停止要求の前は空きを待つ必要があります。"
        );

        stop.store(true, Ordering::Relaxed);

        assert_eq!(
            done.recv_timeout(Duration::from_secs(5)),
            Ok(false),
            "停止要求があれば、満杯のチャネルを待たずに false を返す必要があります。"
        );
    }

    #[test]
    fn deliver_fails_when_receiver_is_dropped() {
        let (sender, receiver) = mpsc::channel(1);
        drop(receiver);

        assert!(
            !deliver(&sender, &AtomicBool::new(false), Incoming::Disconnected),
            "受け取り側が破棄されていれば false を返す必要があります。"
        );
    }
}
