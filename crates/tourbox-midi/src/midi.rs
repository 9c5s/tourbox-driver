//! MIDI ポートの列挙と選択、送受信。

use anyhow::Context;
use midir::{Ignore, MidiIO, MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
use tokio::sync::mpsc::{self, error::TrySendError};
use tracing::{debug, info, warn};

use crate::midi_msg::MidiMessage;

/// midir のクライアント名と接続名。
const CLIENT_NAME: &str = "tourbox-midi";
/// Control Change のステータスバイトの上位 4 ビット。
const CONTROL_CHANGE: u8 = 0xb0;
/// データバイトの最大値 (7 ビット)。
const DATA_MAX: u8 = 0x7f;

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

/// 開いている MIDI 出力ポート。
pub struct MidiOut {
    connection: MidiOutputConnection,
    port_name: String,
}

impl MidiOut {
    /// 出力ポートを開く。
    ///
    /// `Virtual` は `name` で仮想ポートを作成し、`Existing` は `name` に一致する既存のポートへ接続する。
    pub fn open(name: &str, mode: PortMode) -> anyhow::Result<Self> {
        let output = MidiOutput::new(CLIENT_NAME).context("MIDI 出力を初期化できませんでした。")?;
        let (connection, port_name) = match mode {
            PortMode::Virtual => (create_virtual_output(output, name)?, name.to_owned()),
            PortMode::Existing => {
                let (port, port_name) = choose_existing_port(&output, name, "出力")?;
                let connection = output.connect(&port, CLIENT_NAME).with_context(|| {
                    format!("出力ポート {port_name:?} に接続できませんでした。")
                })?;
                (connection, port_name)
            }
        };
        info!(port = %port_name, ?mode, "MIDI 出力ポートを開きました。");
        Ok(Self {
            connection,
            port_name,
        })
    }

    /// 開いているポートの名前を返す。`Existing` では一致した既存のポートの名前である。
    pub fn port_name(&self) -> &str {
        &self.port_name
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
}

impl MidiIn {
    /// 入力ポートを開き、受信したバイト列を 1 メッセージずつ `tx` へ渡す。
    ///
    /// ポートの選び方は [`MidiOut::open`] と同じである。SysEx、タイミング (MIDI クロックと MTC)、
    /// Active Sensing は受け取らない。`tx` が満杯のときは待たずに捨てる。
    pub fn open(name: &str, mode: PortMode, tx: mpsc::Sender<Vec<u8>>) -> anyhow::Result<Self> {
        let mut input =
            MidiInput::new(CLIENT_NAME).context("MIDI 入力を初期化できませんでした。")?;
        input.ignore(Ignore::All);
        let callback = forward_to(tx);
        let (connection, port_name) = match mode {
            PortMode::Virtual => (
                create_virtual_input(input, name, callback)?,
                name.to_owned(),
            ),
            PortMode::Existing => {
                let (port, port_name) = choose_existing_port(&input, name, "入力")?;
                let connection = input
                    .connect(&port, CLIENT_NAME, callback, ())
                    .with_context(|| {
                        format!("入力ポート {port_name:?} に接続できませんでした。")
                    })?;
                (connection, port_name)
            }
        };
        info!(port = %port_name, ?mode, "MIDI 入力ポートを開きました。");
        Ok(Self {
            connection,
            port_name,
        })
    }

    /// 開いているポートの名前を返す。`Existing` では一致した既存のポートの名前である。
    pub fn port_name(&self) -> &str {
        &self.port_name
    }

    /// ポートを閉じる。仮想ポートは削除され、以後は受信コールバックが呼ばれない。
    pub fn close(self) {
        self.connection.close();
        info!(port = %self.port_name, "MIDI 入力ポートを閉じました。");
    }
}

/// 出力ポートの名前を一覧の順に返す。
pub fn list_output_names() -> anyhow::Result<Vec<String>> {
    let output = MidiOutput::new(CLIENT_NAME).context("MIDI 出力を初期化できませんでした。")?;
    port_names(&output)
}

/// 入力ポートの名前を一覧の順に返す。
pub fn list_input_names() -> anyhow::Result<Vec<String>> {
    let input = MidiInput::new(CLIENT_NAME).context("MIDI 入力を初期化できませんでした。")?;
    port_names(&input)
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

/// ポートの名前を一覧の順に返す。
fn port_names<T: MidiIO>(io: &T) -> anyhow::Result<Vec<String>> {
    Ok(named_ports(io)?.into_iter().map(|(_, name)| name).collect())
}

/// ポートと名前の組を一覧の順に返す。
fn named_ports<T: MidiIO>(io: &T) -> anyhow::Result<Vec<(T::Port, String)>> {
    io.ports()
        .into_iter()
        .map(|port| {
            let name = io
                .port_name(&port)
                .context("MIDI ポートの名前を取得できませんでした。")?;
            Ok((port, name))
        })
        .collect()
}

/// `requested` に一致する既存のポートを選ぶ。一致が複数あれば候補をログに出して先頭を使う。
///
/// `direction` はメッセージに使う「出力」か「入力」である。
fn choose_existing_port<T: MidiIO>(
    io: &T,
    requested: &str,
    direction: &str,
) -> anyhow::Result<(T::Port, String)> {
    let mut ports = named_ports(io)?;
    let names: Vec<String> = ports.iter().map(|(_, name)| name.clone()).collect();
    let candidates = port_candidates(&names, requested);
    let Some(&index) = candidates.first() else {
        anyhow::bail!(
            "{direction}ポート {requested:?} が見つかりません。候補: {}",
            describe_names(names.iter())
        );
    };
    if candidates.len() > 1 {
        warn!(
            candidates = %describe_names(candidates.iter().map(|&candidate| &names[candidate])),
            "{direction}ポート {requested:?} に一致するポートが複数あります。先頭の {:?} を使います。",
            names[index]
        );
    }
    Ok(ports.swap_remove(index))
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
}
