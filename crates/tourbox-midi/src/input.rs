//! MIDI 入力ポートの再試行状態。

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::midi::{self, MidiIn, PortMode};

/// 入力ポートの開き方と一覧の取得。
pub trait InputBackend {
    type Port: InputPort;

    /// 入力ポートの名前を一覧の順に返す。
    fn list_names(&self) -> anyhow::Result<Vec<String>>;

    /// `name` の入力ポートを `mode` で開き、受信したバイト列を 1 メッセージずつ `tx` へ渡す。
    fn open(
        &self,
        name: &str,
        mode: PortMode,
        tx: mpsc::Sender<Vec<u8>>,
    ) -> anyhow::Result<Self::Port>;
}

/// 開いている入力ポート。
pub trait InputPort {
    /// 開いているポートの名前。`Existing` では一致した既存のポートの名前である。
    fn port_name(&self) -> &str;

    fn close(self);
}

/// midir の入力ポート。
pub struct MidiInBackend;

impl InputBackend for MidiInBackend {
    type Port = MidiIn;

    fn list_names(&self) -> anyhow::Result<Vec<String>> {
        midi::list_input_names()
    }

    fn open(
        &self,
        name: &str,
        mode: PortMode,
        tx: mpsc::Sender<Vec<u8>>,
    ) -> anyhow::Result<MidiIn> {
        MidiIn::open(name, mode, tx)
    }
}

impl InputPort for MidiIn {
    fn port_name(&self) -> &str {
        MidiIn::port_name(self)
    }

    fn close(self) {
        MidiIn::close(self)
    }
}

/// 入力ポートの状態 (未接続か接続済み)。
///
/// 時間は持たない。呼び出し側が再試行の周期ごとに [`InputState::tick`] を呼ぶ。
/// 受信がないことはポートの消失を意味しないので、受信の有無では接続を閉じない。
/// 消失は `Existing` の一覧の再評価で判断する。
pub struct InputState<B: InputBackend> {
    backend: B,
    name: String,
    mode: PortMode,
    /// 開いたポートが受信したバイト列の送り先。開くたびに複製を渡す。
    tx: mpsc::Sender<Vec<u8>>,
    /// None は未接続。
    port: Option<B::Port>,
}

impl<B: InputBackend> InputState<B> {
    /// 未接続の状態で作る。最初の [`InputState::tick`] で開く。
    pub fn new(backend: B, name: String, mode: PortMode, tx: mpsc::Sender<Vec<u8>>) -> Self {
        Self {
            backend,
            name,
            mode,
            tx,
            port: None,
        }
    }

    /// 未接続なら開き、`Existing` の接続中は一覧から選んだポートが消えていないかを調べる。
    pub fn tick(&mut self) {
        let Some(port) = &self.port else {
            self.connect();
            return;
        };
        if self.mode == PortMode::Virtual {
            return;
        }
        match self.backend.list_names() {
            Ok(names) if names.iter().any(|name| name == port.port_name()) => {}
            Ok(_) => {
                warn!(
                    port = port.port_name(),
                    "MIDI 入力ポートが一覧から消えました。ポートを閉じて再試行します。"
                );
                self.disconnect();
            }
            Err(error) => {
                warn!("MIDI 入力ポートの一覧を取得できませんでした。接続は維持します: {error:#}");
            }
        }
    }

    /// ポートを閉じ、すぐに開き直す。開けなければ次の [`InputState::tick`] で再試行する。
    pub fn reopen(&mut self) {
        info!(port = %self.name, "MIDI 入力ポートを開き直します。");
        self.disconnect();
        self.connect();
    }

    /// ポートを閉じる。
    pub fn shutdown(mut self) {
        self.disconnect();
    }

    fn connect(&mut self) {
        match self.backend.open(&self.name, self.mode, self.tx.clone()) {
            Ok(port) => self.port = Some(port),
            Err(error) => warn!("MIDI 入力ポートを開けませんでした。再試行します: {error:#}"),
        }
    }

    fn disconnect(&mut self) {
        if let Some(port) = self.port.take() {
            port.close();
        }
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use std::cell::RefCell;
    use std::rc::Rc;

    use tokio::sync::mpsc;

    use super::{InputBackend, InputPort};
    use crate::midi::PortMode;

    /// フェイクへの呼び出しの記録。
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum Call {
        /// 開く要求。成否を問わず記録する。
        Open(String, PortMode),
        List,
        Close(String),
    }

    struct State {
        /// 開けたときのポート名。None なら開けない。
        openable: Option<String>,
        /// 一覧の結果。None なら取得に失敗する。
        listed: Option<Vec<String>>,
        /// 開いているポートが受信したバイト列の送り先。None は開いているポートがない。
        tx: Option<mpsc::Sender<Vec<u8>>>,
        calls: Vec<Call>,
    }

    /// 開く結果と一覧をテストから操作でき、開いたポートの受信を起こせる入力。複製は状態を共有する。
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
                openable: port_name.map(str::to_owned),
                listed: Some(port_name.into_iter().map(str::to_owned).collect()),
                tx: None,
                calls: Vec::new(),
            })))
        }

        /// 開けたときのポート名を設定する。None なら開けなくする。
        pub(crate) fn set_openable(&self, port_name: Option<&str>) {
            self.0.borrow_mut().openable = port_name.map(str::to_owned);
        }

        /// 一覧の結果を設定する。None なら取得に失敗させる。
        pub(crate) fn set_listed(&self, names: Option<&[&str]>) {
            self.0.borrow_mut().listed =
                names.map(|names| names.iter().map(|&name| name.to_owned()).collect());
        }

        /// 開いているポートが `bytes` を受信したことにする。
        pub(crate) fn receive(&self, bytes: &[u8]) {
            let state = self.0.borrow();
            let tx = state
                .tx
                .as_ref()
                .expect("受信させるには入力ポートが開いている必要があります。");
            tx.try_send(bytes.to_vec())
                .expect("受信チャネルに空きがある必要があります。");
        }

        /// これまでの呼び出しを取り出し、記録を空にする。
        pub(crate) fn take_calls(&self) -> Vec<Call> {
            std::mem::take(&mut self.0.borrow_mut().calls)
        }
    }

    impl InputBackend for FakeBackend {
        type Port = FakePort;

        fn list_names(&self) -> anyhow::Result<Vec<String>> {
            let mut state = self.0.borrow_mut();
            state.calls.push(Call::List);
            state
                .listed
                .clone()
                .ok_or_else(|| anyhow::anyhow!("フェイクの一覧の取得エラーです。"))
        }

        fn open(
            &self,
            name: &str,
            mode: PortMode,
            tx: mpsc::Sender<Vec<u8>>,
        ) -> anyhow::Result<FakePort> {
            let mut state = self.0.borrow_mut();
            state.calls.push(Call::Open(name.to_owned(), mode));
            let port_name = state.openable.clone().ok_or_else(|| {
                anyhow::anyhow!("フェイクの入力ポート {name:?} が見つかりません。")
            })?;
            state.tx = Some(tx);
            Ok(FakePort {
                name: port_name,
                state: Rc::clone(&self.0),
            })
        }
    }

    pub(crate) struct FakePort {
        name: String,
        state: Rc<RefCell<State>>,
    }

    impl InputPort for FakePort {
        fn port_name(&self) -> &str {
            &self.name
        }

        fn close(self) {
            let mut state = self.state.borrow_mut();
            state.tx = None;
            state.calls.push(Call::Close(self.name));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Call, FakeBackend};
    use super::*;

    const PORT: &str = "loopMIDI TourBox In";

    fn open(name: &str, mode: PortMode) -> Call {
        Call::Open(name.to_owned(), mode)
    }

    fn closed(port: &str) -> Call {
        Call::Close(port.to_owned())
    }

    /// `PORT` を `Existing` で開いた状態と受信チャネルを作り、開くまでの呼び出しの記録を捨てる。
    fn connected(backend: &FakeBackend) -> (InputState<FakeBackend>, mpsc::Receiver<Vec<u8>>) {
        let (tx, rx) = mpsc::channel(16);
        let mut input = InputState::new(backend.clone(), PORT.to_owned(), PortMode::Existing, tx);
        input.tick();
        assert_eq!(
            backend.take_calls(),
            [open(PORT, PortMode::Existing)],
            "最初の tick でポートを開く必要があります。"
        );
        (input, rx)
    }

    #[test]
    fn tick_retries_opening_while_port_is_missing() {
        let backend = FakeBackend::unavailable();
        let (tx, _rx) = mpsc::channel(16);
        let mut input = InputState::new(
            backend.clone(),
            "TourBox In".to_owned(),
            PortMode::Existing,
            tx,
        );

        input.tick();
        input.tick();
        backend.set_openable(Some(PORT));
        backend.set_listed(Some(&[PORT]));
        input.tick();
        input.tick();

        assert_eq!(
            backend.take_calls(),
            [
                open("TourBox In", PortMode::Existing),
                open("TourBox In", PortMode::Existing),
                open("TourBox In", PortMode::Existing),
                Call::List,
            ],
            "開けない間は tick ごとに開き直し、開けた後は一覧の再評価に移る必要があります。"
        );
    }

    #[test]
    fn opened_port_forwards_received_bytes_to_channel() {
        let backend = FakeBackend::openable(PORT);
        let (_input, mut rx) = connected(&backend);

        backend.receive(&[0xb0, 100, 64]);

        assert_eq!(
            rx.try_recv(),
            Ok(vec![0xb0, 100, 64]),
            "開いたポートが受信したバイト列は、new に渡したチャネルへ届く必要があります。"
        );
    }

    #[test]
    fn connected_port_stays_open_without_received_messages() {
        let backend = FakeBackend::openable(PORT);
        let (mut input, mut rx) = connected(&backend);

        for _ in 0..3 {
            input.tick();
        }

        assert_eq!(
            backend.take_calls(),
            [Call::List, Call::List, Call::List],
            "受信がなくても、一覧にポートがある間は接続を維持する必要があります。"
        );
        backend.receive(&[0xb0, 1, 2]);
        assert_eq!(
            rx.try_recv(),
            Ok(vec![0xb0, 1, 2]),
            "無通信の後も、開いたポートの受信が届く必要があります。"
        );
    }

    #[test]
    fn existing_mode_closes_port_when_its_name_disappears_and_reopens_when_it_returns() {
        let backend = FakeBackend::openable(PORT);
        backend.set_listed(Some(&["Microsoft GS Wavetable Synth", PORT]));
        let (tx, mut rx) = mpsc::channel(16);
        let mut input = InputState::new(
            backend.clone(),
            "TourBox In".to_owned(),
            PortMode::Existing,
            tx,
        );
        input.tick();

        // 設定名 TourBox In は一覧にないが、選んだポートの名前はある
        input.tick();
        backend.set_listed(Some(&["Microsoft GS Wavetable Synth"]));
        backend.set_openable(None);
        input.tick();
        input.tick();
        backend.set_listed(Some(&["Microsoft GS Wavetable Synth", PORT]));
        backend.set_openable(Some(PORT));
        input.tick();
        input.tick();

        assert_eq!(
            backend.take_calls(),
            [
                open("TourBox In", PortMode::Existing),
                Call::List,
                Call::List,
                closed(PORT),
                open("TourBox In", PortMode::Existing),
                open("TourBox In", PortMode::Existing),
                Call::List,
            ],
            "選んだポートの名前が一覧から消えたら閉じて再試行し、再び現れたら開き直す必要があります。"
        );
        backend.receive(&[0xb0, 1, 2]);
        assert_eq!(
            rx.try_recv(),
            Ok(vec![0xb0, 1, 2]),
            "開き直したポートの受信も同じチャネルへ届く必要があります。"
        );
    }

    #[test]
    fn existing_mode_keeps_port_when_listing_fails() {
        let backend = FakeBackend::openable(PORT);
        let (mut input, _rx) = connected(&backend);

        backend.set_listed(None);
        input.tick();
        input.tick();

        assert_eq!(
            backend.take_calls(),
            [Call::List, Call::List],
            "一覧を取得できないときは消失とみなさず、接続を維持する必要があります。"
        );
    }

    #[test]
    fn virtual_mode_keeps_port_without_listing_and_retries_only_failed_creation() {
        let backend = FakeBackend::unavailable();
        let (tx, _rx) = mpsc::channel(16);
        let mut input = InputState::new(
            backend.clone(),
            "TourBox MIDI In".to_owned(),
            PortMode::Virtual,
            tx,
        );

        input.tick();
        backend.set_openable(Some("TourBox MIDI In"));
        input.tick();
        // 自作の仮想ポートは一覧に現れない
        input.tick();
        input.tick();

        assert_eq!(
            backend.take_calls(),
            [
                open("TourBox MIDI In", PortMode::Virtual),
                open("TourBox MIDI In", PortMode::Virtual),
            ],
            "Virtual では作成に失敗したときだけ再試行し、作成後は一覧を見ずにポートを維持する必要があります。"
        );
    }

    #[test]
    fn reopen_closes_existing_port_and_opens_it_again_immediately() {
        let backend = FakeBackend::openable(PORT);
        let (mut input, mut rx) = connected(&backend);

        input.reopen();

        assert_eq!(
            backend.take_calls(),
            [closed(PORT), open(PORT, PortMode::Existing)],
            "reopen はポートを閉じ、次の tick を待たずに開き直す必要があります。"
        );
        backend.receive(&[0xb0, 1, 2]);
        assert_eq!(
            rx.try_recv(),
            Ok(vec![0xb0, 1, 2]),
            "開き直したポートの受信も同じチャネルへ届く必要があります。"
        );
    }

    #[test]
    fn reopen_retries_on_next_tick_when_reopening_fails() {
        let backend = FakeBackend::openable(PORT);
        let (mut input, _rx) = connected(&backend);

        backend.set_openable(None);
        input.reopen();
        backend.set_openable(Some(PORT));
        input.tick();

        assert_eq!(
            backend.take_calls(),
            [
                closed(PORT),
                open(PORT, PortMode::Existing),
                open(PORT, PortMode::Existing),
            ],
            "reopen で開けなかったときは未接続になり、次の tick で開き直す必要があります。"
        );
    }

    #[test]
    fn shutdown_closes_port() {
        let backend = FakeBackend::openable(PORT);
        let (input, _rx) = connected(&backend);

        input.shutdown();

        assert_eq!(
            backend.take_calls(),
            [closed(PORT)],
            "shutdown はポートを閉じる必要があります。"
        );
    }
}
