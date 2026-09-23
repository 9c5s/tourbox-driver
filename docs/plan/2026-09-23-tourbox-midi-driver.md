# TourBox Elite MIDI ドライバ 実装計画

> レビュー状態: codex-review-loop round 6 PASS (2026-09-23)。累積 41 件 (round 1: 24、round 2: 11、round 3: 6) をすべて反映し、却下なし。実機で確認する残項目は 9 章の受け入れ確認 1 (アンロック応答の到着遅延) と 5b (5 秒未満のポート再起動)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** TourBox Elite を公式コンソールなしで MIDI コントローラーとして使う常駐アプリと、その土台になる通信ライブラリを Rust で作る。

**Architecture:** Cargo ワークスペースに、デバイス通信だけを担うライブラリ `tourbox` (protocol、transport、device) と、設定ファイルに従って操作を MIDI に変換する実行ファイル `tourbox-midi` (lib + 薄い main。config、engine、midi、haptics、commands) を置く。ライブラリは MIDI を知らず、実行ファイルはプロトコルのバイト列を知らない。

**Tech Stack:** Rust (stable)、tokio、serialport、btleplug、midir、notify + notify-debouncer-mini、toml + serde、clap、thiserror、anyhow、tracing、lefthook、GitHub Actions。

**Spec:** `docs/spec/2026-09-23-tourbox-midi-driver-design.md`

本計画はユーザーの指示により、具体的なコードを含めない。
各タスクは「何を、どのファイルに、どう検証するか」を示し、仕様の詳細は設計書を参照する。

## Global Constraints

- 対象 OS は Windows と macOS。CI は windows-latest と macos-latest の両方で実行する
- `cargo fmt --check`、`cargo build` (出荷構成の検査、ADR-0015)、`cargo clippy --all-targets -- -D warnings`、`cargo test`、`cargo lint-adr` (ADR の形式検査、ADR-0012) を CI と lefthook の pre-commit で実行する
- コミットは Conventional Commits (`type: 日本語の要約` の 1 行、body なし)
- 文書 (README、docs 配下、コード内コメント) はすべて日本語の常体で書く。プログラムの出力 (ログ、エラーメッセージ、CLI のヘルプ) は敬体 (ADR-0013)
- ライブラリ `tourbox` は midir に依存しない。実行ファイル `tourbox-midi` は protocol のバイト値を直接扱わない
- ライセンス未指定のリポジトリ (jasonrohrer/tourBoxEliteLinuxDriver) のコードは流用しない
- テストは要件を満たすことを目的にし、`docs/protocol/haptic-captures.md` の実機キャプチャを 94 バイトの期待値に使う (設計書 2.5 節、8.1 節)
- 各タスクのコミットには、そのタスクで必要になるモジュール登録 (`lib.rs`、親 `mod.rs`)、`Cargo.toml` の依存追加、`cli.rs` のサブコマンドや引数の更新をすべて含める。以下の Files に書かれていなくても、このルールで当該タスクに含まれる
- 実機確認の手順は、実機や該当 OS がなくて実施できない場合に「未実施」と理由を記録し、実施済みと区別する
- 各タスクの実装中に ADR に当たる判断 (依存クレートの選定、公開インターフェースの変更、データ形式の解釈、手順の変更、既存 ADR との矛盾。基準は `docs/adr/README.md`) が生じたら、実装を止めて統括側へ報告し、「提案」の ADR を書いて承認を得てから続ける (ADR-0014)

## Review Focus

仕様が暗黙に含むが、どのタスクのテストも直接は扱わない入力と、期待される振る舞い。各行のテストは括弧内のタスクに追加する。

1. アンロック応答が複数のかたまりに分かれて届き、末尾が遅れて届く。初期化の状態機械は受信のたびに静穏タイマー (200 ms) を再設定し、`Configuring` の待機 (200 ms) でも受信を捨てる。これは時間による区切りで保証ではないため、`dump` が各状態の受信時刻をログに出し、受け入れ確認で実機の遅延が収まることを確認する。`Running` 中に 20 バイト以上のイベントがまとまって届いても全件復号される (Task 5、6)
2. 修飾ボタンを押したまま切断される。engine は `Disconnected` で `release_all()` を返し、押下中だったボタンの Off を送って修飾状態を解除する (Task 9)
3. MIDI 出力ポートが消えている間にボタンが解放される (Off の送信が失敗する)。出力層の台帳が送信に成功したボタン由来の On を保持し、復帰時に台帳の Off を新しいポートに送って DAW 側に Note On を残さない。待機中も engine は DeviceEvent で状態を更新し続けるので、待機中に離した修飾は復帰後に残らない (Task 9、11)
4. 設定ファイルが保存途中の状態で再読込が走る。読んだ内容が構文エラーか検証エラー (空ファイルの必須セクション欠落を含む) なら前の設定を維持してログに出す。途中まででも検証に通る内容は反映され、次の保存で再度読み直される (Task 13)
5. 相対 CC の `step` が符号化の範囲 (1〜63) を超える。設定の検証で範囲外をエラーにする (Task 8)

---

### Task 1: リポジトリとワークスペースの骨組み

**Files:**
- Modify: `Cargo.toml` (workspace。`xtask` だけが members にあるので 2 クレートを追加する)、`lefthook.yml` (fmt、clippy、test、lint-adr、commit-msg は作成済み。glob の見直しと build ジョブの追加を行う)
- Create: `crates/tourbox/Cargo.toml`、`crates/tourbox/src/lib.rs`、`crates/tourbox-midi/Cargo.toml`、`crates/tourbox-midi/src/lib.rs`、`crates/tourbox-midi/src/main.rs`
- Create: `.github/workflows/ci.yml`、`README.md` (骨組み)
- 既存: `.gitignore`、`.cargo/config.toml` (`cargo lint-adr` のエイリアス)、`xtask/` (ADR の形式検査)、`docs/adr/`、`docs/protocol/haptic-captures.md`、`docs/spec/`、`docs/plan/` (コミット済み)

**Interfaces:**
- Produces: 2 クレートの空の骨組み。`tourbox` は `fake` feature を宣言し、dev-dependencies で自クレートを `features = ["fake"]` で参照する。`tourbox-midi` は lib と main の 2 ターゲットを持ち、main は lib の `run_cli()` を呼ぶだけにする

- [ ] ワークスペースに 2 クレートを追加し、`cargo build` と `cargo test` が通ることを確認する (新クレートのテストは 0 件でよい。xtask のテストは既にある)
- [ ] `lefthook install` が済んでいることを確認し、lefthook.yml の glob が新クレートを含むことを確認する
- [ ] CI ワークフローに windows-latest と macos-latest のマトリクスで fmt、build、clippy、test、lint-adr を書く
- [ ] README に目的、対象 OS、lefthook の導入手順 (winget / brew)、loopMIDI の案内、macOS の Bluetooth 権限 (設計書 4.3 節) の案内を書く
- [ ] コミット (`chore: ワークスペースと CI の骨組みを追加`)

### Task 2: protocol のボタン、軸、イベント

**Files:**
- Create: `crates/tourbox/src/protocol/mod.rs`、`crates/tourbox/src/protocol/event.rs`
- Test: 同ファイル内の `#[cfg(test)]`

**Interfaces:**
- Produces: `Button` (14 種)、`Axis` (Knob、Scroll、Dial)、`Direction`、`Event` (Press、Release、Rotate、Unknown)、`decode(u8) -> Event`

- [ ] 設計書 2.3 節の 34 値 (ボタン 14 種の押下と解放、回転 3 軸の両方向) と未知値 (`84`、`ff` など) を表駆動で検証するテストを書き、失敗を確認する
- [ ] ビット規則で復号する `decode` を実装し、テストを通す
- [ ] コミット (`feat: イベントの復号を追加`)

### Task 3: protocol のハプティクス設定、アンロック、応答文字列

**Files:**
- Create: `crates/tourbox/src/protocol/haptics.rs`、`crates/tourbox/src/protocol/frame.rs`
- Test: 同ファイル内の `#[cfg(test)]`。期待値は `docs/protocol/haptic-captures.md`

**Interfaces:**
- Produces: `Strength` (Off、Weak、Strong)、`Speed` (Fast、Medium、Slow)、`Modifier` (None + Button)、`HapticConfig` (`set(axis, modifier, strength, speed)`、`set_axis(axis, strength, speed)`、`encode() -> [u8; 94]`、`Default` は全組み合わせ「強、中速」)、`UNLOCK: [u8; 8]`、`NOT_ALLOW_CONFIG: &[u8]`、`NotAllowConfigDetector` (`feed(&[u8]) -> bool`、`reset()`)

- [ ] `docs/protocol/haptic-captures.md` の固定テンプレートと 6 件のキャプチャを期待値にした `encode` のテストを書き、失敗を確認する。キャプチャの再現には、同資料の備考に書かれた組み合わせごとの値を `set` で与える (`set_axis` だけでは再現できない)
- [ ] 組み合わせ単位の `set` で 1 か所だけ変えた設定を組み立て、設計書 2.4 節のオフセット表の位置だけが変わることを全 45 組で確認するテストと、`set_axis` が対象軸の全組み合わせを書き換えるテストを書く
- [ ] `UNLOCK` のバイト列と、`NotAllowConfigDetector` (一括、全 19 通りの分割位置、1 バイトずつ、前後に無関係なバイト、`reset` 後は一致状態が消える) のテストを書く
- [ ] 実装してテストを通す
- [ ] コミット (`feat: ハプティクス設定メッセージの組み立てを追加`)

### Task 4: Transport トレイトとメモリ内フェイク

**Files:**
- Create: `crates/tourbox/src/transport/mod.rs`、`crates/tourbox/src/transport/fake.rs` (`cfg(feature = "fake")`)、`crates/tourbox/src/error.rs`

**Interfaces:**
- Produces: `Transport` トレイト (`send(&[u8])` は送信完了まで待つ、受信チャネル、切断通知、`close()` は終了を待つ)、`TransportError` (Io、Ble、NotFound、Busy)、`FakeTransport` (送信内容の記録、送信にかかる時間の指定、受信データの注入、切断の発生、`close` の呼び出し記録をテストから操作できる)、`TransportKind` (Auto、Usb、Ble)、`ConnectionConfig { transport: TransportKind, usb_port: Option<String> }`

- [ ] フェイク自体の振る舞い (注入した受信が届く、送信が記録される、切断を発生させられる、`close` が記録される) のテストを `crates/tourbox/tests/fake_transport.rs` に書き、失敗を確認する
- [ ] トレイトとフェイクを実装し、テストを通す。`cargo test` (feature 指定なし) で fake が有効になることを確認する
- [ ] コミット (`feat: Transport トレイトとテスト用フェイクを追加`)

### Task 5: device の初期化、イベント送出、ハプティクス更新、再接続

**Files:**
- Create: `crates/tourbox/src/device.rs`
- Test: `crates/tourbox/tests/device.rs` (フェイクと tokio の時間停止を使う)

**Interfaces:**
- Consumes: Task 2〜4 の型
- Produces: `DeviceEvent` (Input(Event)、Connected、Disconnected)、`Device::run(ConnectionConfig, HapticConfig) -> (DeviceEvent の受信チャネル, DeviceHandle)`、`DeviceHandle::set_haptics(HapticConfig)`、`DeviceHandle::shutdown()`。transport の生成関数を差し込めるようにし、テストではフェイクを注入する

- [ ] 次の要件のテストを先に書き、失敗を確認する: 初期化の送信順序 (UNLOCK、94 バイト)、`Unlocking` で受信を捨て、静穏タイマーは最初の受信で開始して以後は受信のたびに 200 ms に再設定される、無受信なら送信から 1 秒で次へ進む (200 ms では進まない)、最初の受信が 200 ms より遅い場合はその受信から 200 ms 後に進む、送信から 1 秒で打ち切る (1 秒の直前と直後の受信で挙動が分かれる)、`Configuring` の 200 ms 待機中の受信を捨てる (Review Focus 1 の遅れた末尾)、状態遷移の前にチャネルへ入っていたかたまりは取り出した時点の状態で扱われる、`Running` に入ったら `Connected` を送出する、`Running` で 20 バイト以上の受信も 1 バイトずつ全件復号する (Review Focus 1)、`NOT_ALLOW_CONFIG` が `Configuring` と `Running` をまたいで分割到着しても検出して `Disconnected` を送出し再初期化する、`set_haptics` は送信完了から 50 ms あけて最新値だけ送る (送信に 40 ms かかる fake で、送信中の更新が完了後 50 ms に送られる)、同じ値は送らない、初期化中と切断中の更新は保持されて初期化完了後または `Configuring` で送られる、切断で `Disconnected` を送出して 1 秒から 2 倍ずつ最大 30 秒で再接続する、`shutdown().await` が再接続待機中、初期化中、送信中のいずれからでも `close` を呼んでタスクを終える、`Drop` は停止要求だけを出す
- [ ] device を実装し、テストを通す
- [ ] コミット (`feat: デバイスの初期化と再接続を追加`)

### Task 6: USB トランスポートと dump サブコマンド

**Files:**
- Create: `crates/tourbox/src/transport/usb.rs`
- Create: `crates/tourbox-midi/src/cli.rs`、`crates/tourbox-midi/src/commands/mod.rs`、`crates/tourbox-midi/src/commands/dump.rs`
- Modify: `crates/tourbox-midi/src/lib.rs` (`run_cli()`)

**Interfaces:**
- Produces: `UsbTransport::open(port: Option<&str>)` (VID/PID による自動検出、明示指定、115200、DTR、読み取りタイムアウト 100 ms)、`select_usb_port(候補一覧, 指定名) -> Option<候補>`、`port_builder(path) -> SerialPortBuilder` (115200、読み取りタイムアウト 100 ms、`dtr_on_open(true)`)、CLI の `dump [--transport <auto|usb|ble>] [--usb-port <name>]`

- [ ] `select_usb_port` を、serialport の列挙結果の型を入力にして単体テストする (VID/PID 一致、指定名優先、該当なし)
- [ ] `port_builder(path)` を単体テストする。`SerialPortBuilder` には getter がないので、テスト側で `serialport::new(path, 115200)` に `timeout(Duration::from_millis(100))` と `dtr_on_open(true)` を付けて組み立てた期待値と `PartialEq` で比較する (期待値の組み立てに `port_builder` は使わない)。`UsbTransport::open` がポートを開くときに `port_builder` を使うことを実装で保証する
- [ ] 読み取りループの判定関数 (`TimedOut` と `Interrupted` は継続、それ以外のエラーは切断) を切り出して単体テストする
- [ ] 専用スレッドで読み取り、mpsc へ渡す実装と、`close` でスレッド終了を待つ実装を書く。ポートはあるが開けない (使用中) 場合は `TransportError::Busy` にする
- [ ] clap でコマンドラインを定義し、`dump` で引数の接続設定と `HapticConfig::default()` を使って device に接続し、DeviceEvent を表示する。device は各状態での受信バイトと時刻を debug ログに出し、`dump` はそれを表示する。不正な引数は clap の既定 (終了コード 2) に任せる
- [ ] 実機 (USB) で `dump` を実行し、全コントロールのイベントと Connected が表示され、無操作で 10 秒待っても再接続が起きず、ケーブルを抜くと Disconnected と再接続が表示されることを確認する。ログでアンロック応答が `Unlocking` の静穏期間内に収まっていることも確認し、収まらなければ設計書 4.3 節の初期値を見直す
- [ ] 実機 (USB) で `dump` を 2 つ起動し、後から起動した側が `Busy` として待機し、先の側を終了すると接続することを確認する
- [ ] コミット (`feat: USB 接続と dump サブコマンドを追加`)

### Task 7: BLE トランスポートと auto 選択

**Files:**
- Create: `crates/tourbox/src/transport/ble.rs`
- Modify: `crates/tourbox/src/device.rs` (auto の順序)

**Interfaces:**
- Produces: `BleTransport::connect()` (名前の前方一致スキャン、fff1 購読、fff2 書き込み、20 バイト分割と 10 ms 間隔)、`TransportKind::Auto` の実装 (USB 検出、`NotFound` なら BLE を 10 秒スキャン、`Busy` なら BLE へ進まず再試行)

- [ ] 20 バイト分割 (94 バイトが 20、20、20、20、14 になる) と送信間隔 (10 ms) を純粋関数と tokio の時間停止で単体テストする
- [ ] btleplug でスキャン、接続、購読、書き込み、切断通知、`close` を実装する。権限がなくてアダプタを取れない場合は `TransportError::Ble` にし、ログに README の権限手順を案内する
- [ ] auto の順序を device に実装し、フェイクの生成関数で「USB なし、BLE あり」「両方なし」「USB が Busy (BLE へ進まない)」の分岐をテストする
- [ ] 実機 (USB ケーブルを抜いた状態) で `dump --transport ble` を実行し、イベントが届くことを確認する。macOS ではターミナルに Bluetooth 権限を付けずに起動して案内が出ること、付けてから接続できることを確認する
- [ ] コミット (`feat: BLE 接続と自動選択を追加`)

### Task 8: 設定ファイルの読込、検証、解決

**Files:**
- Create: `crates/tourbox-midi/src/config/mod.rs`、`crates/tourbox-midi/src/config/schema.rs`、`crates/tourbox-midi/src/config/validate.rs`、`crates/tourbox-midi/src/config/resolve.rs`
- Create: `config.example.toml`
- Test: 各ファイル内の `#[cfg(test)]` (lib ターゲットなので内部 API に届く)

**Interfaces:**
- Produces: `Config` (device、midi、haptics、haptics.control、map)、`Config::load(path) -> Result<Config, ConfigError>` (行番号付き)、`default_config_path()`、`Config::to_haptic_config() -> HapticConfig`、`Config::to_connection_config() -> ConnectionConfig`、`Config::resolve_mapping() -> MappingSet` (レイヤ × 操作ごとの割り当て。チャンネル解決済み)、`HapticsControlConfig` (6.2 節の割り当て)

- [ ] `config.example.toml` を設計書 5.2 節の内容で書く
- [ ] 次のテストを先に書き、失敗を確認する: example が読み込める、空ファイルが必須セクション欠落のエラーになる、範囲外の CC 番号、未知のキー、存在しないコントロール名、`note` と `cc` の両方指定、相対 CC の `step` が 64 (Review Focus 5) と絶対 CC の `step` が 128、`[haptics.control]` の (チャンネル、CC) 重複がそれぞれ行番号付きのエラーになる、`[haptics]` の既定値 (strong、medium) と修飾ごとの上書きが HapticConfig に反映される、`midi.channel` と項目の `channel` が MappingSet に解決される、`default_config_path()` が OS ごとの値を返す、`transport = "auto"` と `usb_port` の併用が ConnectionConfig に両方入る
- [ ] serde と toml (span 付きエラー) で実装し、テストを通す
- [ ] コミット (`feat: 設定ファイルの読込と検証を追加`)

### Task 9: engine (修飾レイヤと MIDI 変換)

**Files:**
- Create: `crates/tourbox-midi/src/engine.rs`、`crates/tourbox-midi/src/midi_msg.rs` (Note On、Note Off、CC の 3 バイト表現)
- Test: 同ファイル内の `#[cfg(test)]`

**Interfaces:**
- Consumes: Task 8 の `MappingSet`、Task 5 の `DeviceEvent`
- Produces: `MidiMessage` (Note On、Note Off、CC の 3 バイト表現)、`Outgoing { message: MidiMessage, origin: ButtonOn | ButtonOff | Rotation }`、`Engine::new(MappingSet)`、`Engine::handle(DeviceEvent) -> Vec<Outgoing>` (`Disconnected` では `release_all` と同じ結果を返す)、`Engine::release_all() -> Vec<Outgoing>`、`Engine::replace_mapping(MappingSet)` (絶対値の引き継ぎ規則を含む)

- [ ] 次のテストを表駆動で先に書き、失敗を確認する: ボタンの Note On/Off と CC 127/0、チャンネルと velocity の解決済み値、絶対 CC の 0〜127 への丸めと `step` と `initial` と `invert`、相対 CC の 2 方式の符号化と両方向と `step`、修飾レイヤの切替とフォールバック (フォールバックした絶対 CC は基本レイヤの内部値を使う)、同時に有効な修飾は先に押した 1 つだけで後のボタンは昇格しない、修飾ボタン自身のメッセージ送出、押下時の割り当てを記憶して解放時に対応する Off を送る (Side 押下、Top 押下、Side 解放、Top 解放の操作列で Note 70 の Off が出る)、Outgoing の由来がボタンの Note と CC では `ButtonOn` と `ButtonOff`、回転では `Rotation` になる (絶対 CC の値 127 と相対 CC の -1 も `Rotation`)、`release_all` の内容と状態の初期化、`Disconnected` で `release_all` と同じ結果が出る (Review Focus 2)、`replace_mapping` で同じ `cc`、`mode`、`encoding` の割り当てが残った絶対値だけ引き継がれる
- [ ] engine を実装し、テストを通す
- [ ] コミット (`feat: 修飾レイヤと MIDI 変換の engine を追加`)

### Task 10: MIDI ポートの選択と送受信

**Files:**
- Create: `crates/tourbox-midi/src/midi.rs`、`crates/tourbox-midi/src/commands/list_ports.rs`
- Test: 同ファイル内の `#[cfg(test)]` (名前選択と受信解析のみ)

**Interfaces:**
- Produces: `select_port(候補名一覧, 設定名) -> Option<usize>` (完全一致、部分一致、複数なら先頭)、`list_output_names()` と `list_input_names()`、`PortMode` (`Virtual` は macOS の自作ポート、`Existing` は Windows の既存ポート。`cfg` で既定を決める)、`MidiOut::open(name, PortMode)`、`MidiOut::send(MidiMessage)`、`MidiIn::open(name, PortMode, 送信先チャネル)`、`parse_control_change(&[u8]) -> Option<(channel, cc, value)>`、CLI の `list-ports`

- [ ] `select_port` (完全一致の優先、部分一致、複数候補で先頭、該当なし) と `parse_control_change` (CC は返す、Note やシステムメッセージは None) のテストを書き、失敗を確認する
- [ ] midir で出力と入力を実装し、OS ごとの分岐を `cfg` で書く。`list-ports` を実装する
- [ ] Windows で loopMIDI のポート、macOS で仮想ポートが `list-ports` と DAW から見えることを確認する
- [ ] コミット (`feat: MIDI ポートの選択と送受信を追加`)

### Task 11: 常駐ループの結線

**Files:**
- Create: `crates/tourbox-midi/src/commands/run.rs`、`crates/tourbox-midi/src/output.rs` (出力ポートの再試行状態)
- Modify: `crates/tourbox-midi/src/lib.rs`

**Interfaces:**
- Consumes: Task 5、8、9、10
- Produces: `run(config_path, verbose)`、`OutputState` (未接続、接続済み。5 秒ごとの再試行、`Existing` では 5 秒ごとの一覧再評価、`Virtual` では一覧監視なし、送信エラーで未接続に戻る、送信に成功したボタン由来の On の台帳、復帰時に台帳の Off を送る)。このタスク時点の起動順は、設定読込、ログ、Ctrl+C、MIDI 出力 (再試行状態)、デバイス接続。設定監視は Task 13、MIDI 入力は Task 12 で加わり、最終形の起動順 (設計書 6.3 節) は Task 13 で検証する

- [ ] `OutputState` を、ポートの開閉と送信と一覧取得を差し替えられる形にして単体テストする: 未検出時は 5 秒ごとに再試行する、`Existing` では 5 秒ごとの一覧再評価で選んだポート名が消えたら未接続に戻る、`Virtual` では一覧に自分のポートがなくても閉じず作成失敗時だけ再試行する、送信エラーで未接続に戻り待機中の Outgoing は捨てる、`ButtonOn` の送信成功で台帳に入り `ButtonOff` の送信成功で消える、`ButtonOff` の送信失敗で台帳に残る、`Rotation` (絶対値 127 と相対値 127 を含む) は台帳を変えない、復帰時に台帳の Off を新しいポートへ送る (Review Focus 3)、ポート名の変更時は古いポートへ Off を送ってから台帳を空にする
- [ ] run の結線を、出力と device を差し替えられる形にしてテストする: 出力待機中も DeviceEvent が engine に渡り、待機中に離した修飾が復帰後に残らない、待機中の切断で engine の状態が初期化される
- [ ] tracing の初期化 (`RUST_LOG`、`--verbose` で受信バイトと送信 MIDI を表示)、Ctrl+C の受付、状態変化のログ (接続、切断、ポート消失、復帰) を実装する。Ctrl+C では `release_all` の結果と台帳の Off を送ってからポートを閉じ、`shutdown().await` を待つ。設定ファイルのエラーは終了コード 1 で行番号付きの原因を表示する
- [ ] 実機と DAW で確認する: 設定どおりの Note と CC が届く、修飾レイヤが切り替わり押下中の切替でも Note On が残らない、loopMIDI を終了して再起動すると送信が復帰する (Windows)、Ctrl+C で押下中の Note が解放される
- [ ] コミット (`feat: 常駐ループを追加`)

### Task 12: MIDI 入力によるハプティクス制御

**Files:**
- Create: `crates/tourbox-midi/src/haptics.rs`
- Modify: `crates/tourbox-midi/src/commands/run.rs`

**Interfaces:**
- Consumes: Task 8 の `HapticsControlConfig`、Task 5 の `DeviceHandle`、Task 10 の `MidiIn` と `parse_control_change`
- Produces: `HapticsController::new(HapticsControlConfig, 基準の HapticConfig)`、`HapticsController::on_cc(channel, cc, value) -> Option<HapticConfig>`、`HapticsController::reset(HapticsControlConfig, 基準の HapticConfig) -> HapticConfig`

- [ ] 次のテストを先に書き、失敗を確認する: チャンネルの絞り込み、軸ごとの強度と速度の値域、軸への変更が全組み合わせに及ぶが `with` に個別項目のある組み合わせは除外される、修飾ごとの個別制御、master がなしの間は強度も速度も 0 の設定を返しその間の軸変更は保持して master 復帰時に反映する、`reset` で新しい割り当てと基準値に戻り上書きが捨てられる
- [ ] 入力ポートの状態 (`InputState`) を、開閉と一覧取得を差し替えられる形にして単体テストする: 未検出時は 5 秒ごとに再試行する、無通信では閉じない、`Existing` では 5 秒ごとの一覧再評価で名前が消えたら閉じて再試行状態に戻り再び現れたら開き直す、`Virtual` では一覧に自分のポートがなくても閉じない、再読込の要求で `Existing` の接続を閉じて開き直す
- [ ] 実装してテストを通し、run に結線する。入力は出力とは独立した状態として扱う
- [ ] DAW から CC を送って回転操作の手応えが変わることを実機で確認する
- [ ] コミット (`feat: MIDI 入力によるハプティクス制御を追加`)

### Task 13: 設定ファイルの自動再読込

**Files:**
- Create: `crates/tourbox-midi/src/config/watch.rs`、`crates/tourbox-midi/src/config/diff.rs`
- Modify: `crates/tourbox-midi/src/commands/run.rs`

**Interfaces:**
- Consumes: Task 5 の `DeviceHandle::shutdown()`、Task 9 の `replace_mapping`、Task 12 の `reset`
- Produces: `watch(path) -> 変更通知チャネル` (親ディレクトリを監視し、対象ファイル名だけ通す。notify-debouncer-mini で 300 ms)、`diff(old, new) -> 変更セクションの集合` (map、haptics、haptics.control、midi.output、midi.input、midi.channel、device)、run 内の反映手順 (設計書 5.4 節の順)

- [ ] 次のテストを先に書き、失敗を確認する: デバウンスで連続イベントが 1 回にまとまる、親ディレクトリの他ファイルの変更は通さない、空ファイルと構文エラーのあるファイルはエラーとして前の設定を維持する (Review Focus 4)、途中まででも検証に通る内容は反映される、`diff` が変更セクションを正しく返す (`midi.channel` だけの変更も検出する)、反映の順序 (release_all と台帳の Off、MappingSet の差し替え、ハプティクスの作り直しと `set_haptics`、ポートの開き直し、device の `shutdown().await` 後の再起動) が守られる、`midi` に差分がなくても `Existing` の入力ポートは開き直される
- [ ] 実装してテストを通し、run に結線する。最終形の起動順 (設計書 6.3 節) になっていること、設定監視と Ctrl+C の受付が出力ポートの待機中も動くことをテストで確認する
- [ ] 実機で設定ファイルを編集して反映されること、誤った編集で前の設定が維持されてログに理由が出ること、誤ったポート名を設定ファイルで直すと待機から抜けることを確認する
- [ ] コミット (`feat: 設定ファイルの自動再読込を追加`)

### Task 14: README と受け入れ確認

**Files:**
- Modify: `README.md`

- [ ] README に、インストール (cargo build、loopMIDI、lefthook)、macOS の Bluetooth 権限の付与と拒否時の復旧、設定ファイルの書き方 (example への参照)、サブコマンド、loopMIDI を短時間で再起動した場合の復旧手順 (設定ファイルの保存し直し、またはアプリ再起動)、設計書 9 章の受け入れ確認手順 (適用 OS 付き) を書く
- [ ] コミット (`docs: README に導入手順と受け入れ確認を追加`)
- [ ] リモートに push し、最終コミットに対する CI が両 OS で成功していることを確認する
- [ ] Windows と macOS で設計書 9 章の全項目 (適用 OS に従う。5b の短時間再起動を含む) を実施し、結果 (実施済みか未実施と理由) を PR の説明に書く
