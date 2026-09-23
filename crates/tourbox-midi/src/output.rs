//! MIDI 出力ポートの再試行状態と、Off を送れていない On の台帳。

use tracing::{debug, info, warn};

use crate::engine::{Origin, Outgoing};
use crate::midi::{self, MidiOut, PortInfo, PortMode};
use crate::midi_msg::MidiMessage;

/// 出力ポートの開き方と一覧の取得。
pub trait OutputBackend {
    type Port: OutputPort;

    /// 出力ポートの名前と ID を一覧の順に返す。
    fn list_ports(&self) -> anyhow::Result<Vec<PortInfo>>;

    /// `name` の出力ポートを `mode` で開く。
    fn open(&self, name: &str, mode: PortMode) -> anyhow::Result<Self::Port>;
}

/// 開いている出力ポート。
pub trait OutputPort {
    /// 開いているポートの名前。`Existing` では一致した既存のポートの名前である。
    fn port_name(&self) -> &str;

    /// 開いているポートの ID。`Existing` で選んだ既存のポートの ID で、仮想ポートは None である。
    fn port_id(&self) -> Option<&str>;

    fn send(&mut self, message: MidiMessage) -> anyhow::Result<()>;

    fn close(self);
}

/// midir の出力ポート。
pub struct MidiOutBackend;

impl OutputBackend for MidiOutBackend {
    type Port = MidiOut;

    fn list_ports(&self) -> anyhow::Result<Vec<PortInfo>> {
        midi::list_output_ports()
    }

    fn open(&self, name: &str, mode: PortMode) -> anyhow::Result<MidiOut> {
        MidiOut::open(name, mode)
    }
}

impl OutputPort for MidiOut {
    fn port_name(&self) -> &str {
        MidiOut::port_name(self)
    }

    fn port_id(&self) -> Option<&str> {
        MidiOut::port_id(self)
    }

    fn send(&mut self, message: MidiMessage) -> anyhow::Result<()> {
        MidiOut::send(self, message)
    }

    fn close(self) {
        MidiOut::close(self)
    }
}

/// 出力ポートの状態 (未接続か接続済み) と台帳。
///
/// 時間は持たない。呼び出し側が再試行の周期ごとに [`OutputState::tick`] を呼ぶ。
pub struct OutputState<B: OutputBackend> {
    backend: B,
    name: String,
    mode: PortMode,
    /// None は未接続。
    port: Option<B::Port>,
    /// On の送信に成功し、対応する Off をまだ送れていないボタン由来のメッセージ。On を送った順に並べる。
    ledger: Vec<Held>,
}

impl<B: OutputBackend> OutputState<B> {
    /// 未接続の状態で作る。最初の [`OutputState::tick`] で開く。
    pub fn new(backend: B, name: String, mode: PortMode) -> Self {
        Self {
            backend,
            name,
            mode,
            port: None,
            ledger: Vec::new(),
        }
    }

    /// 未接続なら開き、`Existing` の接続中は選んだポートの ID が一覧から消えていないかを調べる。
    pub fn tick(&mut self) {
        let Some(port) = &self.port else {
            self.connect();
            return;
        };
        if self.mode == PortMode::Virtual {
            return;
        }
        match self.backend.list_ports() {
            Ok(ports)
                if ports
                    .iter()
                    .any(|listed| port.port_id() == Some(&listed.id)) => {}
            Ok(_) => {
                warn!(
                    port = port.port_name(),
                    id = port.port_id().map(display),
                    "MIDI 出力ポートが一覧から消えました。ポートを閉じて再試行します。"
                );
                self.disconnect();
            }
            Err(error) => {
                warn!("MIDI 出力ポートの一覧を取得できませんでした。接続は維持します: {error:#}");
            }
        }
    }

    /// 接続中なら送り、未接続なら捨てる。送信エラーではポートを閉じて未接続に戻る。
    pub fn send(&mut self, outgoing: Outgoing) {
        let message = outgoing.message;
        let Some(port) = &mut self.port else {
            debug!(
                ?message,
                "MIDI 出力ポートが未接続のため、メッセージを捨てました。"
            );
            return;
        };
        match port.send(message) {
            Ok(()) => {
                debug!(
                    ?message,
                    bytes = format_args!("{:02x?}", message.to_bytes()),
                    "MIDI メッセージを送信しました。"
                );
                self.record(outgoing);
            }
            Err(error) => {
                warn!(
                    "MIDI メッセージを送信できませんでした。ポートを閉じて再試行します: {error:#}"
                );
                self.disconnect();
            }
        }
    }

    /// 台帳の Off を今のポートへ送って台帳を空にし、ポートを閉じて、すぐに `name` を開く。
    /// 開けなければ次の [`OutputState::tick`] で再試行する。
    pub fn reopen(&mut self, name: String) {
        info!(from = %self.name, to = %name, "MIDI 出力ポートを開き直します。");
        self.send_ledger_offs();
        self.ledger.clear();
        self.disconnect();
        self.name = name;
        self.connect();
    }

    /// `offs` と台帳に残った Off を送り、ポートを閉じる。
    pub fn shutdown(mut self, offs: Vec<Outgoing>) {
        for off in offs {
            self.send(off);
        }
        self.send_ledger_offs();
        self.disconnect();
    }

    /// ポートを開き、開けたら台帳の Off を送る。
    fn connect(&mut self) {
        match self.backend.open(&self.name, self.mode) {
            Ok(port) => {
                self.port = Some(port);
                if !self.ledger.is_empty() {
                    info!(
                        count = self.ledger.len(),
                        "未接続の間に送れなかったボタンの Off を送ります。"
                    );
                    self.send_ledger_offs();
                }
            }
            Err(error) => {
                warn!("MIDI 出力ポートを開けませんでした。再試行します: {error:#}");
            }
        }
    }

    fn disconnect(&mut self) {
        if let Some(port) = self.port.take() {
            port.close();
        }
    }

    /// 台帳の各項目の Off を送る。送れた項目は台帳から消える。
    fn send_ledger_offs(&mut self) {
        for held in self.ledger.clone() {
            self.send(Outgoing {
                message: held.off(),
                origin: Origin::ButtonOff,
            });
        }
    }

    /// 送信に成功したメッセージで台帳を更新する。
    fn record(&mut self, outgoing: Outgoing) {
        let held = Held::of(outgoing.message);
        match outgoing.origin {
            Origin::ButtonOn => {
                if !self.ledger.contains(&held) {
                    self.ledger.push(held);
                }
            }
            Origin::ButtonOff => self.ledger.retain(|entry| *entry != held),
            Origin::Rotation => {}
        }
    }
}

/// 台帳の項目。On を送ったメッセージのチャンネル、種類、番号。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Held {
    Note { channel: u8, note: u8 },
    Cc { channel: u8, cc: u8 },
}

impl Held {
    fn of(message: MidiMessage) -> Self {
        match message {
            MidiMessage::NoteOn { channel, note, .. } | MidiMessage::NoteOff { channel, note } => {
                Self::Note { channel, note }
            }
            MidiMessage::ControlChange { channel, cc, .. } => Self::Cc { channel, cc },
        }
    }

    /// 対応する Off (Note Off か CC の 0)。
    fn off(self) -> MidiMessage {
        match self {
            Self::Note { channel, note } => MidiMessage::NoteOff { channel, note },
            Self::Cc { channel, cc } => MidiMessage::ControlChange {
                channel,
                cc,
                value: 0,
            },
        }
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::{OutputBackend, OutputPort};
    use crate::midi::{PortInfo, PortMode};
    use crate::midi_msg::MidiMessage;

    /// フェイクへの呼び出しの記録。
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum Call {
        /// 開く要求。成否を問わず記録する。
        Open(String, PortMode),
        List,
        /// 送信先のポート名とメッセージ。成否を問わず記録する。
        Send(String, MidiMessage),
        Close(String),
    }

    struct State {
        /// 開けたときのポート。None なら開けない。
        openable: Option<PortInfo>,
        /// 一覧の結果。None なら取得に失敗する。
        listed: Option<Vec<PortInfo>>,
        send_fails: bool,
        calls: Vec<Call>,
    }

    /// 名前だけを指定したポート。ID は名前と取り違えても一致しないよう、名前と異なる文字列にする。
    fn named(name: &str) -> PortInfo {
        PortInfo {
            name: name.to_owned(),
            id: format!("{name} の ID"),
        }
    }

    /// 開く結果と一覧と送信の成否をテストから操作できる出力。複製は状態を共有する。
    #[derive(Clone)]
    pub(crate) struct FakeBackend(Rc<RefCell<State>>);

    impl FakeBackend {
        /// `port_name` で開けて、一覧にもそのポートがある状態で作る。
        pub(crate) fn openable(port_name: &str) -> Self {
            Self::with(Some(port_name))
        }

        /// 開けず、一覧が空の状態で作る。
        pub(crate) fn unavailable() -> Self {
            Self::with(None)
        }

        fn with(port_name: Option<&str>) -> Self {
            Self(Rc::new(RefCell::new(State {
                openable: port_name.map(named),
                listed: Some(port_name.into_iter().map(named).collect()),
                send_fails: false,
                calls: Vec::new(),
            })))
        }

        /// 開けたときのポート名を設定する。None なら開けなくする。
        pub(crate) fn set_openable(&self, port_name: Option<&str>) {
            self.0.borrow_mut().openable = port_name.map(named);
        }

        /// 開けたときのポートを名前と ID で設定する。
        pub(crate) fn set_openable_port(&self, name: &str, id: &str) {
            self.0.borrow_mut().openable = Some(PortInfo {
                name: name.to_owned(),
                id: id.to_owned(),
            });
        }

        /// 一覧の結果を設定する。None なら取得に失敗させる。
        pub(crate) fn set_listed(&self, names: Option<&[&str]>) {
            self.0.borrow_mut().listed =
                names.map(|names| names.iter().map(|&name| named(name)).collect());
        }

        /// 一覧の結果を名前と ID の組で設定する。
        pub(crate) fn set_listed_ports(&self, ports: &[(&str, &str)]) {
            self.0.borrow_mut().listed = Some(
                ports
                    .iter()
                    .map(|&(name, id)| PortInfo {
                        name: name.to_owned(),
                        id: id.to_owned(),
                    })
                    .collect(),
            );
        }

        pub(crate) fn set_send_fails(&self, fails: bool) {
            self.0.borrow_mut().send_fails = fails;
        }

        /// これまでの呼び出しを取り出し、記録を空にする。
        pub(crate) fn take_calls(&self) -> Vec<Call> {
            std::mem::take(&mut self.0.borrow_mut().calls)
        }
    }

    impl OutputBackend for FakeBackend {
        type Port = FakePort;

        fn list_ports(&self) -> anyhow::Result<Vec<PortInfo>> {
            let mut state = self.0.borrow_mut();
            state.calls.push(Call::List);
            state
                .listed
                .clone()
                .ok_or_else(|| anyhow::anyhow!("フェイクの一覧の取得エラーです。"))
        }

        fn open(&self, name: &str, mode: PortMode) -> anyhow::Result<FakePort> {
            let mut state = self.0.borrow_mut();
            state.calls.push(Call::Open(name.to_owned(), mode));
            let port = state.openable.clone().ok_or_else(|| {
                anyhow::anyhow!("フェイクの出力ポート {name:?} が見つかりません。")
            })?;
            Ok(FakePort {
                name: port.name,
                id: port.id,
                state: Rc::clone(&self.0),
            })
        }
    }

    pub(crate) struct FakePort {
        name: String,
        id: String,
        state: Rc<RefCell<State>>,
    }

    impl OutputPort for FakePort {
        fn port_name(&self) -> &str {
            &self.name
        }

        fn port_id(&self) -> Option<&str> {
            Some(&self.id)
        }

        fn send(&mut self, message: MidiMessage) -> anyhow::Result<()> {
            let mut state = self.state.borrow_mut();
            state.calls.push(Call::Send(self.name.clone(), message));
            if state.send_fails {
                anyhow::bail!("フェイクの送信エラーです。");
            }
            Ok(())
        }

        fn close(self) {
            self.state.borrow_mut().calls.push(Call::Close(self.name));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Call, FakeBackend};
    use super::*;

    const PORT: &str = "loopMIDI Port";

    fn button_on(message: MidiMessage) -> Outgoing {
        Outgoing {
            message,
            origin: Origin::ButtonOn,
        }
    }

    fn button_off(message: MidiMessage) -> Outgoing {
        Outgoing {
            message,
            origin: Origin::ButtonOff,
        }
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

    fn open(name: &str, mode: PortMode) -> Call {
        Call::Open(name.to_owned(), mode)
    }

    fn sent(port: &str, message: MidiMessage) -> Call {
        Call::Send(port.to_owned(), message)
    }

    fn closed(port: &str) -> Call {
        Call::Close(port.to_owned())
    }

    /// `PORT` を `Existing` で開いた状態を作り、開くまでの呼び出しの記録を捨てる。
    fn connected(backend: &FakeBackend) -> OutputState<FakeBackend> {
        let mut output = OutputState::new(backend.clone(), PORT.to_owned(), PortMode::Existing);
        output.tick();
        assert_eq!(
            backend.take_calls(),
            [open(PORT, PortMode::Existing)],
            "最初の tick でポートを開く必要があります。"
        );
        output
    }

    #[test]
    fn tick_retries_opening_while_port_is_missing_and_drops_messages() {
        let backend = FakeBackend::unavailable();
        let mut output = OutputState::new(
            backend.clone(),
            "TourBox MIDI".to_owned(),
            PortMode::Existing,
        );

        output.tick();
        output.send(button_on(note_on(60)));
        output.tick();
        assert_eq!(
            backend.take_calls(),
            [
                open("TourBox MIDI", PortMode::Existing),
                open("TourBox MIDI", PortMode::Existing),
            ],
            "開けない間は tick ごとに開き直し、送信するメッセージは捨てる必要があります。"
        );

        backend.set_openable(Some("TourBox MIDI Port"));
        output.tick();
        output.send(button_on(note_on(61)));
        assert_eq!(
            backend.take_calls(),
            [
                open("TourBox MIDI", PortMode::Existing),
                sent("TourBox MIDI Port", note_on(61)),
            ],
            "開けたら送信し、捨てた On の Off は送らない必要があります。"
        );
    }

    #[test]
    fn existing_mode_closes_port_when_it_disappears_from_list() {
        let backend = FakeBackend::openable(PORT);
        backend.set_listed(Some(&["Microsoft GS Wavetable Synth", PORT]));
        let mut output =
            OutputState::new(backend.clone(), "loopMIDI".to_owned(), PortMode::Existing);
        output.tick();

        // 設定名 loopMIDI は一覧にないが、選んだポートはある
        output.tick();
        output.send(button_on(note_on(60)));
        backend.set_listed(Some(&["Microsoft GS Wavetable Synth"]));
        backend.set_openable(None);
        output.tick();
        output.send(button_on(note_on(61)));
        output.tick();

        assert_eq!(
            backend.take_calls(),
            [
                open("loopMIDI", PortMode::Existing),
                Call::List,
                sent(PORT, note_on(60)),
                Call::List,
                closed(PORT),
                open("loopMIDI", PortMode::Existing),
            ],
            "選んだポートが一覧にある間は維持し、消えたら閉じて次の tick で開き直す必要があります。"
        );
    }

    #[test]
    fn existing_mode_judges_disappearance_by_id_when_ports_share_a_name() {
        // WinRT では loopMIDI の出力ポートの名前がどれも MIDI になる。
        // SELECTED は TourBox MIDI Out、OTHER は TourBox MIDI In の出力側である
        const SELECTED: &str =
            r"\\?\SWD#MMDEVAPI#MIDII_F20B738C.P_0000#{6dc23320-ab33-4ce4-80d4-bbb3ebbf2814}";
        const OTHER: &str =
            r"\\?\SWD#MMDEVAPI#MIDII_F20B738D.P_0000#{6dc23320-ab33-4ce4-80d4-bbb3ebbf2814}";
        let backend = FakeBackend::unavailable();
        backend.set_openable_port("MIDI", SELECTED);
        backend.set_listed_ports(&[("MIDI", SELECTED), ("MIDI", OTHER)]);
        let mut output = OutputState::new(
            backend.clone(),
            "TourBox MIDI Out".to_owned(),
            PortMode::Existing,
        );
        output.tick();

        output.tick();
        backend.set_listed_ports(&[("MIDI", OTHER)]);
        backend.set_openable(None);
        output.tick();

        assert_eq!(
            backend.take_calls(),
            [
                open("TourBox MIDI Out", PortMode::Existing),
                Call::List,
                Call::List,
                closed("MIDI"),
            ],
            "同じ名前の別のポートが一覧に残っていても、選んだポートの ID が消えたら閉じる必要があります。"
        );
    }

    #[test]
    fn existing_mode_keeps_port_when_listing_fails() {
        let backend = FakeBackend::openable(PORT);
        let mut output = connected(&backend);

        backend.set_listed(None);
        output.tick();
        output.send(button_on(note_on(60)));

        assert_eq!(
            backend.take_calls(),
            [Call::List, sent(PORT, note_on(60))],
            "一覧を取得できないときは消失とみなさず、接続を維持する必要があります。"
        );
    }

    #[test]
    fn virtual_mode_keeps_port_without_listing_and_retries_only_failed_creation() {
        let backend = FakeBackend::unavailable();
        let mut output = OutputState::new(
            backend.clone(),
            "TourBox MIDI".to_owned(),
            PortMode::Virtual,
        );

        output.tick();
        backend.set_openable(Some("TourBox MIDI"));
        output.tick();
        // 自作の仮想ポートは一覧に現れない
        output.tick();
        output.tick();
        output.send(button_on(note_on(60)));

        assert_eq!(
            backend.take_calls(),
            [
                open("TourBox MIDI", PortMode::Virtual),
                open("TourBox MIDI", PortMode::Virtual),
                sent("TourBox MIDI", note_on(60)),
            ],
            "Virtual では作成に失敗したときだけ再試行し、作成後は一覧を見ずにポートを維持する必要があります。"
        );
    }

    #[test]
    fn send_error_closes_port_and_drops_messages_until_reopened() {
        let backend = FakeBackend::openable(PORT);
        let mut output = connected(&backend);

        backend.set_send_fails(true);
        output.send(button_on(note_on(60)));
        output.send(button_on(note_on(61)));
        backend.set_send_fails(false);
        output.tick();
        output.send(button_on(note_on(62)));

        assert_eq!(
            backend.take_calls(),
            [
                sent(PORT, note_on(60)),
                closed(PORT),
                open(PORT, PortMode::Existing),
                sent(PORT, note_on(62)),
            ],
            "送信エラーでポートを閉じ、次の tick で開くまでのメッセージを捨て、失敗した On の Off は送らない必要があります。"
        );
    }

    #[test]
    fn button_on_is_recorded_until_its_off_is_sent() {
        let backend = FakeBackend::openable(PORT);
        let mut output = connected(&backend);
        output.send(button_on(note_on(60)));
        output.send(button_on(cc(20, 127)));
        output.send(button_off(note_off(60)));
        backend.take_calls();

        output.shutdown(Vec::new());

        assert_eq!(
            backend.take_calls(),
            [sent(PORT, cc(20, 0)), closed(PORT)],
            "Off を送った Note は台帳から消え、Off を送っていない CC は台帳に残る必要があります。"
        );
    }

    #[test]
    fn rotation_does_not_change_ledger() {
        let backend = FakeBackend::openable(PORT);
        let mut output = connected(&backend);
        let rotation = |message| Outgoing {
            message,
            origin: Origin::Rotation,
        };

        output.send(button_on(cc(20, 127)));
        // 絶対値 127、相対値 -1 を表す 127、ボタンと同じ CC 番号の 0
        output.send(rotation(cc(1, 127)));
        output.send(rotation(cc(2, 127)));
        output.send(rotation(cc(20, 0)));
        backend.take_calls();
        output.shutdown(Vec::new());

        assert_eq!(
            backend.take_calls(),
            [sent(PORT, cc(20, 0)), closed(PORT)],
            "回転のメッセージは台帳に入らず、台帳の項目も消さない必要があります。"
        );
    }

    #[test]
    fn failed_button_off_stays_in_ledger_and_is_sent_to_new_port_on_recovery() {
        let backend = FakeBackend::openable("Port A");
        let mut output = OutputState::new(backend.clone(), "Port".to_owned(), PortMode::Existing);
        output.tick();
        output.send(button_on(note_on(60)));

        backend.set_send_fails(true);
        output.send(button_off(note_off(60)));
        backend.set_send_fails(false);
        backend.set_openable(Some("Port B"));
        output.tick();
        output.shutdown(Vec::new());

        assert_eq!(
            backend.take_calls(),
            [
                open("Port", PortMode::Existing),
                sent("Port A", note_on(60)),
                sent("Port A", note_off(60)),
                closed("Port A"),
                open("Port", PortMode::Existing),
                sent("Port B", note_off(60)),
                closed("Port B"),
            ],
            "送れなかった Off は台帳に残り、復帰時に新しいポートへ送って台帳から消す必要があります。"
        );
    }

    #[test]
    fn button_off_dropped_while_port_is_missing_is_sent_after_recovery() {
        let backend = FakeBackend::openable(PORT);
        let mut output = connected(&backend);
        output.send(button_on(cc(20, 127)));

        backend.set_listed(Some(&[]));
        backend.set_openable(None);
        output.tick();
        output.send(button_off(cc(20, 0)));
        backend.set_listed(Some(&[PORT]));
        backend.set_openable(Some(PORT));
        output.tick();

        assert_eq!(
            backend.take_calls(),
            [
                sent(PORT, cc(20, 127)),
                Call::List,
                closed(PORT),
                open(PORT, PortMode::Existing),
                sent(PORT, cc(20, 0)),
            ],
            "ポートの消失中に離したボタンの Off は、復帰したポートへ送る必要があります。"
        );
    }

    #[test]
    fn recovery_keeps_offs_that_fail_to_send() {
        let backend = FakeBackend::openable(PORT);
        let mut output = connected(&backend);
        output.send(button_on(note_on(60)));
        output.send(button_on(note_on(61)));
        backend.set_send_fails(true);
        output.send(button_on(note_on(62)));
        backend.take_calls();

        output.tick();
        backend.set_send_fails(false);
        output.tick();

        assert_eq!(
            backend.take_calls(),
            [
                open(PORT, PortMode::Existing),
                sent(PORT, note_off(60)),
                closed(PORT),
                open(PORT, PortMode::Existing),
                sent(PORT, note_off(60)),
                sent(PORT, note_off(61)),
            ],
            "復帰時に送れなかった Off は台帳に残し、次の復帰で送る必要があります。"
        );
    }

    #[test]
    fn reopen_sends_ledger_offs_to_old_port_and_empties_ledger() {
        let backend = FakeBackend::openable("Old Port");
        let mut output =
            OutputState::new(backend.clone(), "Old Port".to_owned(), PortMode::Existing);
        output.tick();
        output.send(button_on(note_on(60)));
        output.send(button_on(cc(20, 127)));
        backend.set_openable(Some("New Port"));

        output.reopen("New Port".to_owned());
        output.shutdown(Vec::new());

        assert_eq!(
            backend.take_calls(),
            [
                open("Old Port", PortMode::Existing),
                sent("Old Port", note_on(60)),
                sent("Old Port", cc(20, 127)),
                sent("Old Port", note_off(60)),
                sent("Old Port", cc(20, 0)),
                closed("Old Port"),
                open("New Port", PortMode::Existing),
                closed("New Port"),
            ],
            "開き直す前に台帳の Off を古いポートへ送り、台帳を空にしてから、次の tick を待たずに新しい名前で開く必要があります。"
        );
    }

    #[test]
    fn reopen_empties_ledger_even_when_offs_to_old_port_fail() {
        let backend = FakeBackend::openable("Old Port");
        let mut output =
            OutputState::new(backend.clone(), "Old Port".to_owned(), PortMode::Existing);
        output.tick();
        output.send(button_on(note_on(60)));
        backend.set_openable(Some("New Port"));
        backend.set_send_fails(true);

        output.reopen("New Port".to_owned());
        backend.set_send_fails(false);
        output.shutdown(Vec::new());

        assert_eq!(
            backend.take_calls(),
            [
                open("Old Port", PortMode::Existing),
                sent("Old Port", note_on(60)),
                sent("Old Port", note_off(60)),
                closed("Old Port"),
                open("New Port", PortMode::Existing),
                closed("New Port"),
            ],
            "古いポートへの Off が失敗しても台帳を空にし、新しいポートへは送らない必要があります。"
        );
    }

    #[test]
    fn reopen_retries_on_next_tick_when_new_port_cannot_be_opened() {
        let backend = FakeBackend::openable(PORT);
        let mut output = connected(&backend);

        backend.set_openable(None);
        output.reopen("New Port".to_owned());
        backend.set_openable(Some("New Port"));
        output.tick();

        assert_eq!(
            backend.take_calls(),
            [
                closed(PORT),
                open("New Port", PortMode::Existing),
                open("New Port", PortMode::Existing),
            ],
            "reopen で開けなかったときは未接続になり、次の tick で新しい名前を開き直す必要があります。"
        );
    }

    #[test]
    fn shutdown_sends_given_offs_then_remaining_ledger_offs_and_closes() {
        let backend = FakeBackend::openable(PORT);
        let mut output = connected(&backend);
        output.send(button_on(note_on(60)));
        output.send(button_on(cc(20, 127)));
        backend.take_calls();

        output.shutdown(vec![button_off(note_off(60)), button_off(note_off(64))]);

        assert_eq!(
            backend.take_calls(),
            [
                sent(PORT, note_off(60)),
                sent(PORT, note_off(64)),
                sent(PORT, cc(20, 0)),
                closed(PORT),
            ],
            "渡された Off を送り、台帳に残った On の Off を送ってから閉じる必要があります。"
        );
    }
}
