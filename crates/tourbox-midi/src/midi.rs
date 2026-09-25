//! MIDI ポートの列挙と選択、送受信。

use std::any::Any;
use std::panic::{self, AssertUnwindSafe};

use anyhow::Context;
use midir::{
    Ignore, MidiIO, MidiInput, MidiInputConnection, MidiInputPort, MidiOutput,
    MidiOutputConnection, MidiOutputPort,
};
use tokio::sync::mpsc::{self, error::TrySendError};
use tracing::{debug, info, warn};

use crate::midi_msg::MidiMessage;

/// midir のクライアント名と接続名。
const CLIENT_NAME: &str = "tourbox-midi";
/// Control Change のステータスバイトの上位 4 ビット。
const CONTROL_CHANGE: u8 = 0xb0;
/// データバイトの最大値 (7 ビット)。
const DATA_MAX: u8 = 0x7f;
/// Windows の機器 ID で、機器の識別子の直前にある文字列。
const DEVICE_KEY_PREFIX: &str = "MIDII_";

/// ポートの開き方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortMode {
    /// 設定名で仮想ポートを作成する。
    Virtual,
    /// 設定名に一致する既存のポートを選ぶ。
    Existing,
}

impl PortMode {
    /// この OS の既定の開き方を返す。macOS は仮想ポートを作成し、それ以外は既存のポートを選ぶ。
    pub fn default_for_os() -> Self {
        if cfg!(target_os = "macos") {
            Self::Virtual
        } else {
            Self::Existing
        }
    }
}

/// 一覧にあるポートの名前と ID。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortInfo {
    pub name: String,
    /// midir が返すポートの ID。Windows では機器 ID (`\\?\SWD#MMDEVAPI#MIDII_...`) である。
    pub id: String,
}

/// 開いている MIDI 出力ポート。
pub struct MidiOut {
    connection: MidiOutputConnection,
    port_name: String,
    /// None は仮想ポート。
    port_id: Option<String>,
}

impl MidiOut {
    /// 出力ポートを開く。
    ///
    /// `Virtual` は `name` で仮想ポートを作成し、`Existing` は `name` に一致する既存のポートへ接続する。
    /// `Existing` で名前が一致する出力ポートがなければ、名前が一致する入力ポートと機器の識別子
    /// ([`device_key`]) が同じ出力ポートへ接続する。
    pub fn open(name: &str, mode: PortMode) -> anyhow::Result<Self> {
        let output = MidiOutput::new(CLIENT_NAME).context("MIDI 出力を初期化できませんでした。")?;
        let (connection, port_name, port_id) = match mode {
            PortMode::Virtual => (create_virtual_output(output, name)?, name.to_owned(), None),
            PortMode::Existing => {
                let (port, info) = choose_existing_output(&output, name)?;
                let connection = output.connect(&port, CLIENT_NAME).with_context(|| {
                    format!("出力ポート {:?} に接続できませんでした。", info.name)
                })?;
                (connection, info.name, Some(info.id))
            }
        };
        info!(port = %port_name, id = port_id.as_deref().map(display), ?mode, "MIDI 出力ポートを開きました。");
        Ok(Self {
            connection,
            port_name,
            port_id,
        })
    }

    /// 開いているポートの名前を返す。`Existing` では一致した既存のポートの名前である。
    pub fn port_name(&self) -> &str {
        &self.port_name
    }

    /// 開いているポートの ID を返す。`Existing` で選んだ既存のポートの ID で、仮想ポートは None である。
    pub fn port_id(&self) -> Option<&str> {
        self.port_id.as_deref()
    }

    /// メッセージを 3 バイトで送る。
    pub fn send(&mut self, message: MidiMessage) -> anyhow::Result<()> {
        self.connection
            .send(&message.to_bytes())
            .with_context(|| format!("出力ポート {:?} へ送信できませんでした。", self.port_name))
    }

    /// ポートを閉じる。仮想ポートは削除される。
    pub fn close(self) {
        self.connection.close();
        info!(port = %self.port_name, "MIDI 出力ポートを閉じました。");
    }
}

/// 開いている MIDI 入力ポート。
pub struct MidiIn {
    connection: MidiInputConnection<()>,
    port_name: String,
    /// None は仮想ポート。
    port_id: Option<String>,
}

impl MidiIn {
    /// 入力ポートを開き、受信したバイト列を 1 メッセージずつ `tx` へ渡す。
    ///
    /// `Virtual` は `name` で仮想ポートを作成し、`Existing` は `name` に名前が一致する既存のポートへ接続する。
    /// SysEx、タイミング (MIDI クロックと MTC)、Active Sensing は受け取らない。
    /// `tx` が満杯のときは待たずに捨てる。
    pub fn open(name: &str, mode: PortMode, tx: mpsc::Sender<Vec<u8>>) -> anyhow::Result<Self> {
        let mut input =
            MidiInput::new(CLIENT_NAME).context("MIDI 入力を初期化できませんでした。")?;
        input.ignore(Ignore::All);
        let callback = forward_to(tx);
        let (connection, port_name, port_id) = match mode {
            PortMode::Virtual => (
                create_virtual_input(input, name, callback)?,
                name.to_owned(),
                None,
            ),
            PortMode::Existing => {
                let (port, info) = choose_existing_input(&input, name)?;
                let connection = input
                    .connect(&port, CLIENT_NAME, callback, ())
                    .with_context(|| {
                        format!("入力ポート {:?} に接続できませんでした。", info.name)
                    })?;
                (connection, info.name, Some(info.id))
            }
        };
        info!(port = %port_name, id = port_id.as_deref().map(display), ?mode, "MIDI 入力ポートを開きました。");
        Ok(Self {
            connection,
            port_name,
            port_id,
        })
    }

    /// 開いているポートの名前を返す。`Existing` では一致した既存のポートの名前である。
    pub fn port_name(&self) -> &str {
        &self.port_name
    }

    /// 開いているポートの ID を返す。`Existing` で選んだ既存のポートの ID で、仮想ポートは None である。
    pub fn port_id(&self) -> Option<&str> {
        self.port_id.as_deref()
    }

    /// ポートを閉じる。仮想ポートは削除され、以後は受信コールバックが呼ばれない。
    pub fn close(self) {
        self.connection.close();
        info!(port = %self.port_name, "MIDI 入力ポートを閉じました。");
    }
}

/// 出力ポートの名前と ID を一覧の順に返す。
pub fn list_output_ports() -> anyhow::Result<Vec<PortInfo>> {
    let output = MidiOutput::new(CLIENT_NAME).context("MIDI 出力を初期化できませんでした。")?;
    Ok(listed_ports(&output)?.1)
}

/// 入力ポートの名前と ID を一覧の順に返す。
pub fn list_input_ports() -> anyhow::Result<Vec<PortInfo>> {
    let input = MidiInput::new(CLIENT_NAME).context("MIDI 入力を初期化できませんでした。")?;
    Ok(listed_ports(&input)?.1)
}

/// 設定名に一致するポートの添字を優先順に返す。
///
/// 完全一致があれば完全一致だけを、なければ部分一致を、一覧の順に返す。大文字と小文字は区別する。
/// 呼び出し側は先頭を使う。
pub fn port_candidates(names: &[String], requested: &str) -> Vec<usize> {
    let has_exact = names.iter().any(|name| name == requested);
    names
        .iter()
        .enumerate()
        .filter(|(_, name)| {
            if has_exact {
                *name == requested
            } else {
                name.contains(requested)
            }
        })
        .map(|(index, _)| index)
        .collect()
}

/// Windows の機器 ID から機器の識別子 (`MIDII_` の後から `.` か `#` の前まで) を取り出す。
///
/// loopMIDI のポートでは、同じポートの入力と出力で識別子が一致する。
/// `MIDII_` を含まない ID (macOS の ID など) には識別子がない。
pub fn device_key(id: &str) -> Option<&str> {
    let (_, rest) = id.split_once(DEVICE_KEY_PREFIX)?;
    let key = rest.split(['.', '#']).next()?;
    (!key.is_empty()).then_some(key)
}

/// 受信したバイト列が Control Change なら (0 起点のチャンネル、CC 番号、値) を返す。
///
/// ステータスバイトで始まる 3 バイトだけを受け付け、ランニングステータスは扱わない。
pub fn parse_control_change(bytes: &[u8]) -> Option<(u8, u8, u8)> {
    match *bytes {
        [status, cc, value]
            if status & 0xf0 == CONTROL_CHANGE && cc <= DATA_MAX && value <= DATA_MAX =>
        {
            Some((status & 0x0f, cc, value))
        }
        _ => None,
    }
}

/// midir の入力ポートと出力ポートに共通の ID の取得。[`MidiIO`] は ID を扱わないので補う。
trait PortId {
    fn port_id(&self) -> String;
}

impl PortId for MidiInputPort {
    fn port_id(&self) -> String {
        self.id()
    }
}

impl PortId for MidiOutputPort {
    fn port_id(&self) -> String {
        self.id()
    }
}

/// ポートと、その名前と ID を一覧の順に返す。2 つの Vec の同じ添字が同じポートを表す。
///
/// 列挙中のパニックはエラーとして返す ([`recover_from_panic`])。
fn listed_ports<T>(io: &T) -> anyhow::Result<(Vec<T::Port>, Vec<PortInfo>)>
where
    T: MidiIO,
    T::Port: PortId,
{
    recover_from_panic(|| {
        let ports = io.ports();
        let infos = ports
            .iter()
            .map(|port| {
                let name = io
                    .port_name(port)
                    .context("MIDI ポートの名前を取得できませんでした。")?;
                Ok(PortInfo {
                    name,
                    id: port.port_id(),
                })
            })
            .collect::<anyhow::Result<_>>()?;
        Ok((ports, infos))
    })
}

/// 列挙の処理 `enumerate` を呼び、パニックしたらその内容を添えたエラーに変える。
///
/// midir の WinRT 実装は、列挙の失敗でエラーを返さずにパニックする。
fn recover_from_panic<R>(enumerate: impl FnOnce() -> anyhow::Result<R>) -> anyhow::Result<R> {
    panic::catch_unwind(AssertUnwindSafe(enumerate)).unwrap_or_else(|payload| {
        Err(anyhow::anyhow!(
            "MIDI ポートの一覧の取得中にパニックが発生しました: {}",
            panic_message(payload.as_ref())
        ))
    })
}

/// パニックの内容を返す。`panic!` と `expect` の内容は `&str` か `String` である。
fn panic_message(payload: &(dyn Any + Send)) -> &str {
    if let Some(message) = payload.downcast_ref::<&str>() {
        message
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message
    } else {
        "(内容を取得できません)"
    }
}

/// `requested` に名前が一致する既存の入力ポートを選ぶ。
fn choose_existing_input(
    input: &MidiInput,
    requested: &str,
) -> anyhow::Result<(MidiInputPort, PortInfo)> {
    let (mut ports, mut infos) = listed_ports(input)?;
    let index = pick_by_name(&infos, requested, "入力")
        .ok_or_else(|| not_found(&infos, requested, "入力"))?;
    Ok((ports.swap_remove(index), infos.swap_remove(index)))
}

/// `requested` に一致する既存の出力ポートを [`pick_output`] で選ぶ。
fn choose_existing_output(
    output: &MidiOutput,
    requested: &str,
) -> anyhow::Result<(MidiOutputPort, PortInfo)> {
    let (mut ports, mut infos) = listed_ports(output)?;
    let index = pick_output(&infos, requested, list_input_ports)?;
    Ok((ports.swap_remove(index), infos.swap_remove(index)))
}

/// `requested` に一致する出力ポートの添字を返す。
///
/// 名前が一致する出力ポートがなければ、`list_inputs` で入力ポートの一覧を取得し、
/// [`output_matching_input`] で選ぶ。WinRT では loopMIDI の出力ポートの名前が「MIDI」になり、
/// 名前では選べないためである。
fn pick_output(
    outputs: &[PortInfo],
    requested: &str,
    list_inputs: impl FnOnce() -> anyhow::Result<Vec<PortInfo>>,
) -> anyhow::Result<usize> {
    if let Some(index) = pick_by_name(outputs, requested, "出力") {
        return Ok(index);
    }
    let inputs = list_inputs()?;
    let index = output_matching_input(outputs, &inputs, requested)
        .ok_or_else(|| not_found(outputs, requested, "出力"))?;
    info!(
        id = %outputs[index].id,
        "出力ポート {requested:?} に名前が一致するポートがないため、名前が一致する入力ポートと機器の識別子が同じ出力ポートを選びました。"
    );
    Ok(index)
}

/// 名前が `requested` に一致する入力ポートと、機器の識別子 ([`device_key`]) が同じ出力ポートの添字を返す。
///
/// 入力ポートは名前での選択と同じ規則で選ぶ。その入力ポートに識別子がない場合と、
/// 識別子が同じ出力ポートがない場合、複数あって 1 つに決められない場合は None を返す。
fn output_matching_input(
    outputs: &[PortInfo],
    inputs: &[PortInfo],
    requested: &str,
) -> Option<usize> {
    let input = pick_by_name(inputs, requested, "入力")?;
    let key = device_key(&inputs[input].id)?;
    let mut matching = outputs
        .iter()
        .enumerate()
        .filter(|(_, output)| device_key(&output.id) == Some(key))
        .map(|(index, _)| index);
    let first = matching.next()?;
    matching.next().is_none().then_some(first)
}

/// `requested` に名前が一致するポートの添字を返す。一致が複数あれば候補をログに出して先頭を使う。
///
/// `direction` はメッセージに使う「出力」か「入力」である。
fn pick_by_name(ports: &[PortInfo], requested: &str, direction: &str) -> Option<usize> {
    let names: Vec<String> = ports.iter().map(|port| port.name.clone()).collect();
    let candidates = port_candidates(&names, requested);
    let &index = candidates.first()?;
    if candidates.len() > 1 {
        warn!(
            candidates = %describe_names(candidates.iter().map(|&candidate| &names[candidate])),
            "{direction}ポート {requested:?} に一致するポートが複数あります。先頭の {:?} を使います。",
            names[index]
        );
    }
    Some(index)
}

/// `requested` に一致するポートがないことを、一覧の名前を候補として添えて表す。
fn not_found(ports: &[PortInfo], requested: &str, direction: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "{direction}ポート {requested:?} が見つかりません。候補: {}",
        describe_names(ports.iter().map(|port| &port.name))
    )
}

/// ポート名を表示用に並べる。
fn describe_names<'a>(names: impl ExactSizeIterator<Item = &'a String>) -> String {
    if names.len() == 0 {
        return "(なし)".to_owned();
    }
    names
        .map(|name| format!("{name:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(unix)]
fn create_virtual_output(output: MidiOutput, name: &str) -> anyhow::Result<MidiOutputConnection> {
    use midir::os::unix::VirtualOutput;

    output
        .create_virtual(name)
        .with_context(|| format!("仮想出力ポート {name:?} を作成できませんでした。"))
}

#[cfg(not(unix))]
fn create_virtual_output(_output: MidiOutput, _name: &str) -> anyhow::Result<MidiOutputConnection> {
    anyhow::bail!("この OS では仮想ポートを作成できません。")
}

#[cfg(unix)]
fn create_virtual_input<F>(
    input: MidiInput,
    name: &str,
    callback: F,
) -> anyhow::Result<MidiInputConnection<()>>
where
    F: FnMut(u64, &[u8], &mut ()) + Send + 'static,
{
    use midir::os::unix::VirtualInput;

    input
        .create_virtual(name, callback, ())
        .with_context(|| format!("仮想入力ポート {name:?} を作成できませんでした。"))
}

#[cfg(not(unix))]
fn create_virtual_input<F>(
    _input: MidiInput,
    _name: &str,
    _callback: F,
) -> anyhow::Result<MidiInputConnection<()>>
where
    F: FnMut(u64, &[u8], &mut ()) + Send + 'static,
{
    anyhow::bail!("この OS では仮想ポートを作成できません。")
}

/// 受信コールバックで受け取ったバイト列をチャネルへ渡す関数を作る。
///
/// コールバックは midir の受信スレッドで動くので、待たずに渡し、満杯なら捨てる。
fn forward_to(tx: mpsc::Sender<Vec<u8>>) -> impl FnMut(u64, &[u8], &mut ()) + Send + 'static {
    let mut dropping = false;
    move |_timestamp, bytes, _| match tx.try_send(bytes.to_vec()) {
        Ok(()) => dropping = false,
        Err(TrySendError::Full(_)) => {
            if !dropping {
                warn!("受信チャネルが満杯のため、空きができるまで受信した MIDI メッセージを捨てます。");
                dropping = true;
            }
        }
        Err(TrySendError::Closed(_)) => {
            debug!("受信チャネルが閉じているため、受信した MIDI メッセージを捨てます。");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use tokio::sync::mpsc::error::TryRecvError;

    use super::*;

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn exact_match_takes_priority_over_partial_matches() {
        let ports = names(&["TourBox MIDI In", "TourBox MIDI", "TourBox MIDI 2"]);

        assert_eq!(
            port_candidates(&ports, "TourBox MIDI"),
            vec![1],
            "完全一致があれば部分一致より優先し、完全一致だけを候補にする必要があります。"
        );
    }

    #[test]
    fn partial_match_is_used_without_exact_match() {
        let ports = names(&["Microsoft GS Wavetable Synth", "loopMIDI TourBox"]);

        assert_eq!(
            port_candidates(&ports, "TourBox"),
            vec![1],
            "完全一致がなければ名前の一部に含むポートを候補にする必要があります。"
        );
    }

    #[test]
    fn multiple_partial_matches_are_listed_in_port_order() {
        let ports = names(&["TourBox A", "Microsoft GS Wavetable Synth", "TourBox B"]);

        assert_eq!(
            port_candidates(&ports, "TourBox"),
            vec![0, 2],
            "複数の部分一致は一覧の順に並べ、先頭を一覧で最初のポートにする必要があります。"
        );
    }

    #[test]
    fn duplicate_exact_matches_are_listed_in_port_order() {
        let ports = names(&["TourBox", "TourBox Out", "TourBox"]);

        assert_eq!(
            port_candidates(&ports, "TourBox"),
            vec![0, 2],
            "同名のポートが複数あれば、完全一致だけを一覧の順に並べる必要があります。"
        );
    }

    #[test]
    fn no_match_yields_no_candidates() {
        let ports = names(&["loopMIDI Port", "Microsoft GS Wavetable Synth"]);

        assert!(
            port_candidates(&ports, "TourBox MIDI").is_empty(),
            "一致するポートがなければ候補を空にする必要があります。"
        );
        assert!(
            port_candidates(&[], "TourBox MIDI").is_empty(),
            "ポートが 1 つもなければ候補を空にする必要があります。"
        );
    }

    #[test]
    fn matching_is_case_sensitive() {
        let ports = names(&["tourbox midi", "TOURBOX MIDI OUT"]);

        assert!(
            port_candidates(&ports, "TourBox MIDI").is_empty(),
            "大文字と小文字が異なる名前は一致として扱わない必要があります。"
        );
    }

    #[test]
    fn control_change_yields_zero_based_channel_cc_and_value() {
        for (bytes, expected) in [
            ([0xb0, 100, 64], (0, 100, 64)),
            ([0xbf, 0, 127], (15, 0, 127)),
            ([0xb9, 127, 0], (9, 127, 0)),
        ] {
            assert_eq!(
                parse_control_change(&bytes),
                Some(expected),
                "{bytes:02x?} は Control Change として解釈する必要があります。"
            );
        }
    }

    #[test]
    fn non_control_change_messages_are_ignored() {
        for bytes in [
            // Note On、Note Off、Program Change、Pitch Bend
            &[0x90, 60, 127][..],
            &[0x80, 60, 0],
            &[0xc0, 5],
            &[0xe0, 0, 64],
            // Song Position Pointer、Timing Clock、SysEx
            &[0xf2, 0, 0],
            &[0xf8],
            &[0xf0, 0x7e, 0x7f, 0x06, 0x01, 0xf7],
        ] {
            assert_eq!(
                parse_control_change(bytes),
                None,
                "{bytes:02x?} は Control Change ではないので捨てる必要があります。"
            );
        }
    }

    #[test]
    fn control_change_with_wrong_length_is_ignored() {
        for bytes in [&[][..], &[0xb0], &[0xb0, 100], &[0xb0, 100, 64, 0]] {
            assert_eq!(
                parse_control_change(bytes),
                None,
                "長さが 3 バイトでない {bytes:02x?} は捨てる必要があります。"
            );
        }
    }

    #[test]
    fn control_change_with_data_byte_out_of_seven_bits_is_ignored() {
        for bytes in [[0xb0, 128, 64], [0xb0, 100, 128], [0xb0, 0xff, 0xff]] {
            assert_eq!(
                parse_control_change(&bytes),
                None,
                "データバイトが 128 以上の {bytes:02x?} は捨てる必要があります。"
            );
        }
    }

    #[test]
    fn forwarder_passes_received_bytes_to_channel() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut forward = forward_to(tx);

        forward(0, &[0xb0, 100, 64], &mut ());
        forward(1, &[0x90, 60, 127], &mut ());

        assert_eq!(
            rx.try_recv(),
            Ok(vec![0xb0, 100, 64]),
            "受信したバイト列をそのまま渡す必要があります。"
        );
        assert_eq!(
            rx.try_recv(),
            Ok(vec![0x90, 60, 127]),
            "受信したバイト列を受信順に渡す必要があります。"
        );
    }

    #[test]
    fn open_ports_can_be_moved_to_another_thread() {
        fn assert_send<T: Send>() {}

        assert_send::<MidiOut>();
        assert_send::<MidiIn>();
    }

    #[test]
    fn forwarder_drops_bytes_while_channel_is_full_and_resumes_after_space_frees() {
        let (tx, mut rx) = mpsc::channel(1);
        let mut forward = forward_to(tx);

        forward(0, &[0xb0, 1, 1], &mut ());
        // 満杯のチャネルへの受け渡しは待たずに捨てる。待つとこのテストは終わらない
        forward(0, &[0xb0, 2, 2], &mut ());
        forward(0, &[0xb0, 3, 3], &mut ());

        assert_eq!(rx.try_recv(), Ok(vec![0xb0, 1, 1]));
        assert_eq!(
            rx.try_recv(),
            Err(TryRecvError::Empty),
            "満杯の間に受信したバイト列は捨てる必要があります。"
        );

        forward(0, &[0xb0, 4, 4], &mut ());

        assert_eq!(
            rx.try_recv(),
            Ok(vec![0xb0, 4, 4]),
            "空きができたら再び渡す必要があります。"
        );
    }

    #[test]
    fn forwarder_ignores_closed_channel() {
        let (tx, rx) = mpsc::channel(1);
        let mut forward = forward_to(tx);
        drop(rx);

        // 受信側が先に終了しても、コールバックはパニックせずに戻る必要がある
        forward(0, &[0xb0, 1, 1], &mut ());
    }

    #[test]
    fn running_status_without_status_byte_is_ignored() {
        assert_eq!(
            parse_control_change(&[100, 64, 0]),
            None,
            "ステータスバイトで始まらないバイト列は捨てる必要があります。"
        );
    }

    fn infos(ports: &[(&str, &str)]) -> Vec<PortInfo> {
        ports
            .iter()
            .map(|&(name, id)| PortInfo {
                name: name.to_owned(),
                id: id.to_owned(),
            })
            .collect()
    }

    /// Windows 10 と loopMIDI 1.0.16 で、WinRT の列挙が返した出力ポート。
    /// loopMIDI のポートは TourBox MIDI Out (F20B738C) と TourBox MIDI In (F20B738D) である。
    fn winrt_outputs() -> Vec<PortInfo> {
        infos(&[
            (
                "Microsoft GS Wavetable Synth",
                r"\\?\SWD#MMDEVAPI#MicrosoftGSWavetableSynth#{6dc23320-ab33-4ce4-80d4-bbb3ebbf2814}",
            ),
            (
                "TouchOSC Bridge",
                r"\\?\SWD#MMDEVAPI#MIDII_78289FD4.P_0000#{6dc23320-ab33-4ce4-80d4-bbb3ebbf2814}",
            ),
            (
                "3 - AG06/AG03",
                r"\\?\SWD#MMDEVAPI#MIDII_6D01412D.P_0000#{6dc23320-ab33-4ce4-80d4-bbb3ebbf2814}",
            ),
            (
                "MIDI",
                r"\\?\SWD#MMDEVAPI#MIDII_F20B738C.P_0000#{6dc23320-ab33-4ce4-80d4-bbb3ebbf2814}",
            ),
            (
                "MIDI",
                r"\\?\SWD#MMDEVAPI#MIDII_F20B738D.P_0000#{6dc23320-ab33-4ce4-80d4-bbb3ebbf2814}",
            ),
        ])
    }

    /// [`winrt_outputs`] と同じ環境で、WinRT の列挙が返した入力ポート。
    fn winrt_inputs() -> Vec<PortInfo> {
        infos(&[
            (
                "3 - AG06/AG03",
                r"\\?\SWD#MMDEVAPI#MIDII_6D01412D.P_0001#{504be32c-ccf6-4d2c-b73f-6f8b3747e22b}",
            ),
            (
                "TouchOSC Bridge",
                r"\\?\SWD#MMDEVAPI#MIDII_96317EFF.P_0001#{504be32c-ccf6-4d2c-b73f-6f8b3747e22b}",
            ),
            (
                "TourBox MIDI Out [1]",
                r"\\?\SWD#MMDEVAPI#MIDII_F20B738C.P_0004#{504be32c-ccf6-4d2c-b73f-6f8b3747e22b}",
            ),
            (
                "TourBox MIDI In [1]",
                r"\\?\SWD#MMDEVAPI#MIDII_F20B738D.P_0004#{504be32c-ccf6-4d2c-b73f-6f8b3747e22b}",
            ),
        ])
    }

    #[test]
    fn device_key_is_text_between_midii_prefix_and_pin_suffix() {
        let outputs = winrt_outputs();
        let inputs = winrt_inputs();

        assert_eq!(
            device_key(&outputs[3].id),
            Some("F20B738C"),
            "出力の機器 ID から MIDII_ の後、.P_ の前までを取り出す必要があります。"
        );
        assert_eq!(
            device_key(&inputs[2].id),
            Some("F20B738C"),
            "同じ loopMIDI のポートの入力からは、出力と同じ識別子を取り出す必要があります。"
        );
        assert_eq!(
            device_key(&inputs[0].id),
            Some("6D01412D"),
            "ハードウェアの機器 ID からも識別子を取り出す必要があります。"
        );
    }

    #[test]
    fn device_key_is_absent_without_midii_prefix_or_identifier() {
        for id in [
            winrt_outputs()[0].id.as_str(),
            // CoreMIDI の ID は数値である
            "1234567",
            r"\\?\SWD#MMDEVAPI#MIDII_.P_0000#{6dc23320-ab33-4ce4-80d4-bbb3ebbf2814}",
            "",
        ] {
            assert_eq!(
                device_key(id),
                None,
                "{id:?} には MIDII_ に続く識別子がないので、識別子なしとする必要があります。"
            );
        }
    }

    #[test]
    fn output_is_matched_through_input_of_same_loopmidi_port() {
        let outputs = winrt_outputs();
        let inputs = winrt_inputs();

        assert_eq!(
            output_matching_input(&outputs, &inputs, "TourBox MIDI Out"),
            Some(3),
            "名前が一致する入力 TourBox MIDI Out [1] と識別子が同じ出力を選ぶ必要があります。"
        );
        assert_eq!(
            output_matching_input(&outputs, &inputs, "TourBox MIDI In"),
            Some(4),
            "名前が一致する入力 TourBox MIDI In [1] と識別子が同じ出力を選ぶ必要があります。"
        );
    }

    #[test]
    fn output_matching_requires_input_name_match_and_output_with_same_key() {
        let outputs = winrt_outputs();
        let inputs = winrt_inputs();

        assert_eq!(
            output_matching_input(&outputs, &inputs, "loopMIDI Port"),
            None,
            "名前が一致する入力がなければ選ばない必要があります。"
        );
        // TouchOSC Bridge は入力と出力で識別子が異なる
        assert_eq!(
            output_matching_input(&outputs, &inputs, "TouchOSC"),
            None,
            "入力と識別子が同じ出力がなければ選ばない必要があります。"
        );
        assert_eq!(
            output_matching_input(&[], &inputs, "TourBox MIDI Out"),
            None,
            "出力が 1 つもなければ選ばない必要があります。"
        );
    }

    #[test]
    fn output_matching_skips_input_without_device_key() {
        let outputs = infos(&[("MIDI", "1001")]);
        let inputs = infos(&[("TourBox MIDI Out", "1001")]);

        assert_eq!(
            output_matching_input(&outputs, &inputs, "TourBox MIDI Out"),
            None,
            "識別子のない ID は、文字列が同じでも対応付けない必要があります。"
        );
    }

    #[test]
    fn output_matching_rejects_key_shared_by_several_outputs() {
        let outputs = infos(&[
            (
                "MIDI",
                r"\\?\SWD#MMDEVAPI#MIDII_0A0B0C0D.P_0000#{6dc23320-ab33-4ce4-80d4-bbb3ebbf2814}",
            ),
            (
                "MIDI",
                r"\\?\SWD#MMDEVAPI#MIDII_0A0B0C0D.P_0001#{6dc23320-ab33-4ce4-80d4-bbb3ebbf2814}",
            ),
        ]);
        let inputs = infos(&[(
            "Interface Port 1",
            r"\\?\SWD#MMDEVAPI#MIDII_0A0B0C0D.P_0004#{504be32c-ccf6-4d2c-b73f-6f8b3747e22b}",
        )]);

        assert_eq!(
            output_matching_input(&outputs, &inputs, "Interface Port 1"),
            None,
            "識別子が同じ出力が複数あるときは、どれか決められないので選ばない必要があります。"
        );
    }

    #[test]
    fn output_matching_uses_first_input_candidate_like_name_selection() {
        let outputs = winrt_outputs();
        let inputs = winrt_inputs();

        assert_eq!(
            output_matching_input(&outputs, &inputs, "TourBox MIDI"),
            Some(3),
            "入力の名前の候補が複数あれば、名前での選択と同じく一覧で最初の入力を使う必要があります。"
        );
    }

    #[test]
    fn output_found_by_name_is_chosen_without_listing_inputs() {
        let listed_inputs = Cell::new(false);

        let index = pick_output(&winrt_outputs(), "AG06", || {
            listed_inputs.set(true);
            Ok(winrt_inputs())
        });

        assert_eq!(
            index.ok(),
            Some(2),
            "名前が一致する出力ポートを選ぶ必要があります。"
        );
        assert!(
            !listed_inputs.get(),
            "名前で見つかったときは、入力ポートの一覧を取得しない必要があります。"
        );
    }

    #[test]
    fn output_not_found_by_name_is_chosen_through_input_of_same_device() {
        assert_eq!(
            pick_output(&winrt_outputs(), "TourBox MIDI Out", || Ok(winrt_inputs())).ok(),
            Some(3),
            "名前で見つからなければ、名前が一致する入力と識別子が同じ出力を選ぶ必要があります。"
        );
    }

    #[test]
    fn output_found_neither_by_name_nor_device_is_reported_with_candidates() {
        let error = pick_output(&winrt_outputs(), "Missing Port", || Ok(winrt_inputs()))
            .expect_err("どの方法でも見つからなければエラーにする必要があります。");

        assert!(
            error
                .to_string()
                .starts_with("出力ポート \"Missing Port\" が見つかりません。候補: "),
            "エラーには設定名と候補を示す必要があります: {error:#}"
        );
    }

    #[test]
    fn panic_during_enumeration_becomes_error() {
        // midir の WinRT 実装は列挙の失敗を expect で処理し、expect は String の内容でパニックする
        let from_expect: anyhow::Result<Vec<PortInfo>> =
            recover_from_panic(|| panic!("FindAllAsyncAqsFilter failed: {:?}", "E_FAIL"));
        let from_literal: anyhow::Result<Vec<PortInfo>> =
            recover_from_panic(|| panic!("固定の文言"));

        let error = from_expect.expect_err("列挙中のパニックはエラーとして返す必要があります。");
        assert!(
            error.to_string().contains("FindAllAsyncAqsFilter failed"),
            "エラーにはパニックの内容を含める必要があります: {error:#}"
        );
        let error = from_literal.expect_err("列挙中のパニックはエラーとして返す必要があります。");
        assert!(
            error.to_string().contains("固定の文言"),
            "文字列リテラルのパニックでも内容を含める必要があります: {error:#}"
        );
    }

    #[test]
    fn enumeration_without_panic_returns_its_result() {
        let ports = infos(&[("MIDI", "1001")]);

        assert_eq!(
            recover_from_panic(|| Ok(ports.clone())).ok(),
            Some(ports),
            "パニックしなければ、列挙の結果をそのまま返す必要があります。"
        );
    }
}
