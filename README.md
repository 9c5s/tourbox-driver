# tourbox-driver

TourBox Elite を公式コンソールアプリなしで MIDI コントローラーとして使うためのドライバである。
デバイスとの通信 (USB と BLE) を担うライブラリ `tourbox` と、操作を MIDI メッセージに変換する常駐アプリ `tourbox-midi` からなる。
対象 OS は Windows と macOS である。

`tourbox-midi` は、TourBox のボタンと回転の操作を、設定ファイルの割り当てに従って Note と Control Change (CC) に変換し、MIDI の出力ポートへ送る。
DAW などから MIDI の入力ポートへ CC を送ると、回転操作のハプティクス (手応え) を切り替えられる。
キーボードやマウスの操作への変換と、GUI の設定画面は対象外である。

## インストール

### ビルド

Rust の stable ツールチェーンでビルドする。
リポジトリの直下で次のコマンドを実行する。

```sh
cargo build --release
```

実行ファイルは、Windows では `target\release\tourbox-midi.exe`、macOS では `target/release/tourbox-midi` に作られる。
以降の例では、実行ファイルを `tourbox-midi` と書く。
PATH の通ったディレクトリに置くか、実行ファイルのパスを指定して起動する。

### Windows の仮想 MIDI ポート

Windows 10 には、仮想 MIDI ポートを作成する OS 標準の機能がない。
Windows 11 では Windows MIDI Services が標準のループバックポートを提供するが、本アプリは Windows の版によらず loopMIDI を前提にする。
[loopMIDI](https://www.tobias-erichsen.de/software/loopmidi.html) で仮想 MIDI ポートを作成しておき、本アプリはその既存ポートを選んで使う。
この方式にした理由は [ADR-0004](docs/adr/0004-Windowsの仮想MIDIポートはloopMIDIを併用しteVirtualMIDISDKは組み込まない.md) に記録している。

loopMIDI では、出力用と入力用に別々のポートを作る (理由は「注意点と制限」の節にある)。
`config.example.toml` の名前に合わせる場合は、`TourBox MIDI Out` と `TourBox MIDI In` の 2 つを作る。
入力用のポートは、DAW からハプティクスを切り替える場合にだけ必要である。
入力を使わない場合は、設定ファイルから `input` の行を削除する。
`input` を残したまま入力用のポートがないと、「MIDI 入力ポートを開けませんでした。」の warn ログが 5 秒ごとに出続ける。
ポート名は 31 文字以内にする。
作ったポートの名前は `tourbox-midi list-ports` で確認できる。

macOS では loopMIDI に当たるソフトウェアは不要である。
本アプリが設定の名前で仮想ポートを作成し、DAW からはその名前のポートとして見える。
仮想ポートは、本アプリが動いている間だけ存在する。

### macOS の Bluetooth 権限

macOS 11 以降で BLE 接続を使うには、本アプリを起動するターミナルアプリに Bluetooth の使用を許可する必要がある。
使用するターミナルアプリ (ターミナル、iTerm2 など) を、次の画面で許可する。

- macOS 13 以降: 「システム設定 > プライバシーとセキュリティ > Bluetooth」
- macOS 11 と 12: 「システム環境設定 > セキュリティとプライバシー > プライバシー > Bluetooth」

BLE 接続を初めて試したときに許可を求めるダイアログが出た場合は、許可する。
権限がない状態で BLE 接続を試すと、本アプリはこの手順への案内をログに出し、接続の再試行を続ける。

許可しなかった場合は、次の手順で復旧する。

1. 上の画面で、使用するターミナルアプリの Bluetooth の使用を許可する。
2. 本アプリを Ctrl+C で終了し、起動し直す。
3. それでも接続できない場合は、ターミナルアプリも終了してから起動し直す。

## 設定ファイル

### 置き場所

設定は TOML の 1 ファイルである。
常駐するときに `--config` でパスを指定する。
省略すると、次の場所の設定ファイルを読む。

- Windows: `%APPDATA%\tourbox-midi\config.toml`
- macOS: `~/Library/Application Support/tourbox-midi/config.toml`

設定ファイルを読めないと、エラーを表示して終了する。
リポジトリ直下の `config.example.toml` を上の場所にコピーし、編集して使う。

```powershell
# Windows (PowerShell)
New-Item -ItemType Directory -Force "$env:APPDATA\tourbox-midi"
Copy-Item config.example.toml "$env:APPDATA\tourbox-midi\config.toml"
```

```sh
# macOS
mkdir -p ~/Library/Application\ Support/tourbox-midi
cp config.example.toml ~/Library/Application\ Support/tourbox-midi/config.toml
```

### セクション

書き方の例は `config.example.toml` にある。
セクションの役割は次のとおり。

| セクション | 省略 | 内容 |
|---|---|---|
| `[device]` | 可 | TourBox との接続方式 |
| `[midi]` | 不可 | MIDI の出力ポート、入力ポート、既定のチャンネル |
| `[haptics]` | 可 | 回転操作のハプティクスの強度と速度 |
| `[haptics.control]` | 可 | 受信した CC によるハプティクスの切り替え |
| `[map]` | 不可 | 操作と MIDI メッセージの対応 |

`[device]` のキーは次のとおり。

- `transport`: `auto` (既定)、`usb`、`ble` のいずれか。`auto` は USB のポートを先に探し、見つからなければ BLE を 10 秒スキャンする。
- `usb_port`: USB のポート名 (`COM3`、`/dev/cu.usbmodem1101` など)。`auto` と `usb` で使い、省略すると VID と PID で自動検出する。大文字と小文字は区別する。

`[midi]` のキーは次のとおり。

- `output`: 出力ポートの名前。必須。
- `input`: ハプティクスの切り替えを受信する入力ポートの名前。省略すると、MIDI 入力によるハプティクスの切り替えは無効になる。
- `channel`: 既定のチャンネル (1〜16)。必須。

ポートの名前の扱いは OS で異なる。
Windows では、既存のポートから名前が完全に一致するものを選び、なければ名前を含むもの (部分一致) を選ぶ。
大文字と小文字は区別する。
一致するポートが複数あれば、候補をログに出して一覧の先頭を使う。
ポートが見つからなければ、候補をログに出して 5 秒ごとに探し直す。
macOS では、`output` と `input` の名前で仮想ポートを作成する。

`output`、`input`、`[device]` の `usb_port` に空の文字列を書くと、検証エラーになる。
空の `output` と `input` は部分一致ですべてのポートに一致し、空の `usb_port` に一致するポートはないためである。

### コントロール名

設定ファイルでは、次のコントロール名を使う。

| 種類 | コントロール名 |
|---|---|
| ボタン | `tall`、`side`、`top`、`short`、`c1`、`c2`、`tour` |
| 十字キー | `up`、`down`、`left`、`right` |
| 押込 | `scroll_press`、`knob_press`、`dial_press` |
| 回転 | `knob`、`scroll`、`dial` |

十字キーと押込はボタンと同じ扱いなので、以下ではまとめてボタンと呼ぶ。

### `[map]` の割り当て

ボタンの項目は `note` か `cc` のどちらか一方を持つ。

- `note`: Note 番号 (0〜127)。押下で Note On、解放で Note Off を送る。
- `cc`: CC 番号 (0〜127)。押下で値 127、解放で値 0 を送る。
- `velocity`: Note On のベロシティ (1〜127)。既定 127。`note` の項目にだけ書ける。
- `channel`: チャンネル (1〜16)。省略すると `[midi]` の `channel` を使う。

回転の項目は次のキーを持つ。

- `cc`: CC 番号 (0〜127)。必須。
- `mode`: `absolute` (既定) か `relative`。
- `step`: 1 ノッチあたりの増減量。`absolute` は 1〜127、`relative` は 1〜63 で、既定は 1。
- `initial`: 内部値の初期値 (0〜127)。既定 0。`absolute` の項目にだけ書ける。
- `encoding`: 増減量の符号化。`twos_complement` (既定。+1 は 1、-1 は 127) か `binary_offset` (+1 は 65、-1 は 63)。`relative` の項目にだけ書ける。
- `invert`: `true` にすると回転方向を反転する。既定 `false`。
- `channel`: チャンネル (1〜16)。省略すると `[midi]` の `channel` を使う。

`absolute` は、0〜127 の範囲で増減する内部値を CC の値として送る。
`relative` は、1 ノッチごとの増減量を `encoding` で符号化して送る。

`[map.with.<ボタン>]` に割り当てを書くと、そのボタンは修飾ボタンになる。
修飾ボタンを押している間は、そのレイヤに書いた操作はレイヤの割り当てを使い、書いていない操作は `[map]` の割り当てを使う。
同時に有効な修飾ボタンは、先に押した 1 つだけである。
修飾ボタン自身の `[map]` の割り当ても通常どおり送る。
修飾ボタン自身を、そのレイヤに書くことはできない。

### `[haptics]` の強度と速度

`knob`、`scroll`、`dial` の各軸に、`strength` と `speed` を書く。

- `strength`: `off`、`weak`、`strong` のいずれか。既定 `strong`。
- `speed`: `fast`、`medium`、`slow` のいずれか。既定 `medium`。公式コンソールの「回転速度」に当たる。

`[haptics.with.<ボタン>]` には、そのボタンを押している間の軸の値を書く。
省略したキーは、軸の値を継承する。

### `[haptics.control]` による切り替え

MIDI の入力ポートで受信した CC で、ハプティクスを切り替える。
`[midi]` に `input` がなければ切り替えは無効になり、起動時と再読込のたびに warn ログでそのことを知らせる。
受信した変更は設定ファイルに書き戻さない。
設定ファイルを再読込すると、ハプティクスは設定ファイルの値に戻る。

- `channel`: 受信するチャンネル (1〜16)。省略すると全チャンネルを受信する。
- `master = { cc = N }`: 値 0〜63 で全軸のハプティクスをなしにし、64〜127 で設定値に戻す。なしの間に受信した軸の変更は、戻したときに反映する。
- `<軸> = { cc = N, speed_cc = M }`: `cc` は値 0 でなし、1〜63 で弱、64〜127 で強にする。`speed_cc` は値 0〜42 で速い、43〜85 で中速、86〜127 で遅いにする。
- `[haptics.control.with.<ボタン>]`: 修飾と軸の組み合わせを個別に切り替える。

`<軸>` の切り替えは、その軸の全組み合わせ (修飾なしと修飾付き) に適用する。
ただし、`[haptics.control.with.<ボタン>]` に同じ軸の項目がある組み合わせは除く。
`[haptics.control]` の中で、同じ CC 番号を複数の項目に割り当てることはできない。

### エラーと自動再読込

起動時の設定ファイルに誤りがあると、行番号付きのエラーを表示して終了コード 1 で終了する。
次は、`--config config.toml` で起動し、7 行目の CC 番号が範囲外だった場合の表示である。

```text
config.toml の 7 行目: `map.knob.cc` は 0〜127 の範囲で指定してください (指定値: 128)。
```

常駐中は設定ファイルを監視し、保存すると再起動なしで反映する。
反映すると、ログに「設定ファイルを再読込して反映しました。」と、変わったセクション (`changed=map` など) が出る。
保存した内容に誤りがあれば、前の設定を維持し、行番号付きの理由を warn ログに出す。
このとき本アプリは終了しない。

再読込では、押下中のボタンの Off を送ってから、新しい割り当てに切り替える。
`[midi]` のポート名を変えるとそのポートを開き直し、`[device]` を変えると TourBox との接続をやり直す。

## 使い方

```text
tourbox-midi [--config <PATH>] [--verbose]
tourbox-midi list-ports
tourbox-midi [--verbose] dump [--transport <auto|usb|ble>] [--usb-port <NAME>]
```

- サブコマンドなし: 設定ファイルを読み込んで常駐し、TourBox の操作を MIDI メッセージに変換する。Ctrl+C を押すと、押下中のボタンの Off を送ってから終了する。
- `list-ports`: MIDI の出力ポートと入力ポートの名前を一覧表示する。Windows で設定ファイルに書く名前を確かめるときに使う。
- `dump`: 設定ファイルを読まずに TourBox に接続し、受け取ったイベントを表示する。MIDI は送らない。接続と操作の確認に使う。

`dump` の `--transport` は接続方式を指定し、既定は `auto` である。
`--usb-port` は USB のポート名を指定し、省略すると自動で検出する。
`--config` と `--verbose` はサブコマンドより前に書く (`tourbox-midi --verbose dump` の順)。
`tourbox-midi --help` でヘルプを表示する。

ログは標準エラー出力に出る。
`--verbose` を付けると、受信したバイト列や送信した MIDI メッセージを debug ログに出す。
ログの絞り込みは環境変数 `RUST_LOG` でも指定でき、`RUST_LOG` があれば `--verbose` より優先する。
`RUST_LOG` がない場合、常駐は `info`、`dump` は `tourbox=debug,info` で出す。
`dump` の既定にライブラリの debug ログを含めるのは、初期化中の受信バイトを確認できるようにするためである。

```powershell
# Windows (PowerShell)
$env:RUST_LOG = "debug"
tourbox-midi
```

```sh
# macOS
RUST_LOG=debug tourbox-midi
```

## 注意点と制限

### TourBox Console の終了

公式の TourBox Console が起動していると、USB のポートが使用中になる。
本アプリはポートが空くまで接続を待ち続けるので、TourBox Console は終了しておく。
BLE でも、他のアプリが TourBox に接続している間は、TourBox を見つけられないことがある。

### BLE での接続

OS の Bluetooth 設定で TourBox をペアリングする必要はない。
ファームウェアによっては、初回の発見に TourBox 側のペアリングモードが必要とされる。
BLE で見つからない場合は、電源スイッチの上のボタンを 2〜3 秒長押ししてペアリングモードにする。

### Windows のポート名の長さ

本アプリが Windows で使う MIDI API (WinMM) は、ポート名を 31 文字までしか返さない。
loopMIDI で 32 文字以上のポート名を付けると、一覧の名前が途中で切れ、設定の名前と完全一致も部分一致もしなくなる。
loopMIDI のポート名は 31 文字以内にする。

### 出力と入力のポートの分離

loopMIDI のポートでは、出力として送ったメッセージが、同じ名前の入力ポートにそのまま現れる。
Windows で `output` と `input` に同じ loopMIDI のポートを指定すると、本アプリは自分が送った CC を受信する。
その CC 番号が `[haptics.control]` の CC 番号と重なると、操作のたびにハプティクスが切り替わる。
出力と入力には、別々の loopMIDI のポートを指定する。

また、出力と入力の一方の名前が、他方の名前に含まれないようにする (接頭辞になる場合を含む)。
loopMIDI のポートは出力の一覧と入力の一覧の両方に現れ、ポートの選択は部分一致を許す。
たとえば出力の名前を `TourBox MIDI`、入力の名前を `TourBox MIDI In` にすると、出力用のポートがなく入力用のポートだけがある間は、出力が `TourBox MIDI In` を開く。
`config.example.toml` の出力の名前を `TourBox MIDI Out` にしているのは、このためである。

### loopMIDI を再起動したときの復帰

本アプリは Windows で、ポートの一覧を 5 秒ごとに確認する。
開いているポートが一覧から消えていれば閉じ、再び現れたら開き直す。
このため、loopMIDI を終了して 10 秒以上待ってから起動し直せば、送信と受信は自動で復帰する。
DAW によっては、ポートが作り直された後に DAW 側でポートを選び直す必要がある。

loopMIDI を終了して 5 秒未満で起動し直すと、一覧を確認する間にポートが消えて再び現れるので、消失を検知できないことがある。
この場合は、無効になった旧接続が残る可能性がある。
とくに入力ポートは、受信がないことと消失を区別できないため、旧接続が残ると DAW から送った CC でハプティクスが切り替わらなくなる。
次のどちらかで復旧する。

- 設定ファイルを正しい内容のまま保存し直し、再読込を起こす。Windows では再読込のたびに入力ポートを開き直す。エディタが変更のないファイルを保存しない場合は、空行を足して保存する。
- 本アプリを Ctrl+C で終了し、起動し直す。

設定ファイルの保存し直しで開き直すのは、入力ポートだけである。
出力が復帰しない場合は、本アプリを起動し直す。

### `--config` のファイル名の大文字と小文字

設定ファイルの変更は、親ディレクトリの変更通知のうち、ファイル名が一致するものだけで検知する。
この比較は大文字と小文字を区別する。
一方、Windows (NTFS) と macOS (既定の APFS) のファイルシステムは、ファイル名の大文字と小文字を区別しない。
そのため、`--config CONFIG.TOML` でも `config.toml` を読み込めるが、Windows では保存しても再読込が起きない。
macOS での挙動は確認していないが、変更通知が実際のファイル名で届けば同じことが起きる。
どちらの OS でも、`--config` には実際のファイル名と同じ大文字と小文字で書く。

### USB での複数起動

同じ TourBox に USB で接続する本アプリを複数起動すると、後から起動した側はポート使用中として接続を待ち続ける。
待っている間は、`auto` でも BLE に切り替えない。
先に起動した側を終了すると、後から起動した側が接続する。
BLE での複数起動の挙動は保証しない。

### 出力ポートがない間の操作

出力ポートが見つからない間の操作は、MIDI メッセージとして送らず、溜めることもしない。
ただし、回転による `absolute` の内部値は変化する。
このため、出力ポートが復帰した後は、DAW 側の値と内部値が一致しないことがある。

## 受け入れ確認

リリース前に、次の手順で動作を確認する。
各項目の見出しの括弧内は、適用する OS と接続方式である。
実機や該当 OS がなくて実施できなかった項目は、結果に「未実施」と理由を記録し、実施済みと区別する。

### 準備

- TourBox Console を終了しておく。
- `config.example.toml` を設定ファイルの場所にコピーする (「設定ファイル」の節を参照)。
- Windows では、loopMIDI で `TourBox MIDI Out` と `TourBox MIDI In` の 2 つのポートを作る。
- DAW か MIDI モニタで、出力ポート `TourBox MIDI Out` から届くメッセージを表示できるようにする。項目 5、5b、7 では、DAW から入力ポート `TourBox MIDI In` へ CC を送れるようにもする。

以下の「表示」は、標準出力とログ (標準エラー出力) の両方を指す。
ログの行には時刻とレベルが付く。

### 1. USB 接続での dump (両 OS)

1. TourBox を USB ケーブルで接続し、`tourbox-midi dump --transport usb` を実行する。
2. 「アンロックを送信しました。」と「初期化が完了しました。」の後に、「接続しました。」が表示されることを確認する。
3. アンロック応答 (26 バイト程度) が、接続直後の「受信しました。」の行のうち、すべて `state="Unlocking"` の行に出ていることを確認する。最後のかたまりの `since_unlock` (アンロックの送信完了からの経過時間) を記録する。`state="Configuring"` や `state="Running"` の行に応答の続きが出た場合や、操作していないのに `入力: Press(Tall)` が表示された場合は、静穏期間 (設計書 4.3 節) の初期値を見直す。
4. 17 のコントロールをすべて操作し、`入力: Press(Tall)`、`入力: Release(Tall)`、`入力: Rotate(Knob, Clockwise)` のような表示が出ることを確認する。ボタン 14 種は押下と解放、回転 3 軸は両方向が対象である。`入力: Unknown(...)` が出ないことも確認する。
5. ケーブルを抜くと「切断しました。」が表示され、挿し直すと再び「接続しました。」が表示されることを確認する。
6. Ctrl+C で終了する。

### 2. BLE 接続での dump (両 OS)

1. USB ケーブルを抜き、TourBox の電源を入れる。
2. macOS では、Bluetooth 権限の案内を確認する。「macOS の Bluetooth 権限」の節の画面でターミナルアプリの許可をオフにしてから、`tourbox-midi dump --transport ble` を実行する。ログに「README の「macOS の Bluetooth 権限」の手順を参照してください。」で終わる案内が出ることを確認する。Ctrl+C で終了し、同じ画面で許可をオンにする。
3. `tourbox-midi dump --transport ble` を実行する。
4. 「BLE のスキャンを開始しました。」「TourBox を見つけました。」「TourBox に BLE で接続しました。」の後に、「接続しました。」が表示されることを確認する。見つからない場合は、TourBox をペアリングモードにする (「BLE での接続」の節を参照)。
5. 項目 1 の 3 と 4 (アンロック応答と操作の表示) を同じように確認する。
6. TourBox の電源を切ると「切断しました。」が表示され、電源を入れると再び「接続しました。」が表示されることを確認する。
7. Ctrl+C で終了する。

### 3. DAW での Note と CC の受信 (両 OS)

1. TourBox を接続し、`tourbox-midi --verbose` を実行する。
2. 「MIDI 出力ポートを開きました。」「MIDI 入力ポートを開きました。」「TourBox と接続しました。」が表示されることを確認する。
3. DAW か MIDI モニタで、`TourBox MIDI Out` を入力に選ぶ。
4. 次の表の操作をして、それぞれのメッセージが届くことを確認する。送ったメッセージは、ログの「MIDI メッセージを送信しました。」の行でも確認できる。

| 操作 | 届くメッセージ |
|---|---|
| Tall の押下と解放 | チャンネル 1 の Note 60 の On (ベロシティ 127) と Off |
| Top の押下と解放 | チャンネル 1 の CC 20 の値 127 と 0 |
| C1 の押下と解放 | チャンネル 2 の Note 62 の On と Off |
| Knob の回転 | チャンネル 1 の CC 1。時計回りで 1 ずつ増え、反時計回りで 1 ずつ減る (絶対 CC) |
| Scroll の回転 | チャンネル 1 の CC 2。上に回すと値 1、下に回すと値 127 (相対 CC) |
| Dial の回転 | チャンネル 1 の CC 3。反時計回りで 2 ずつ増え、時計回りで 2 ずつ減る (絶対 CC、方向の反転) |

Knob と Dial の内部値は 0 から始まる。
値を減らす方向から回すと値 0 が続けて届くので、増やす方向 (Knob は時計回り、Dial は反時計回り) から回す。

### 4. 修飾レイヤ (両 OS)

項目 3 の状態で、次を確認する。

1. Side を押したまま Knob を回すと、CC 1 ではなく CC 11 が届く。Side を離すと CC 1 に戻る。
2. Side を押し、Top を押し、Side を離し、Top を離す。Top の押下で Note 70 の On が、Top の解放で Note 70 の Off が届き、CC 20 は届かない。DAW に Note 70 の On が残らない。

### 5. loopMIDI の再起動からの自動復帰 (Windows)

1. 項目 3 の状態で、TourBox の操作が DAW に届くこと (送信) と、DAW から CC 100 の値 0 と 127 を交互に送ると Knob の手応えが変わること (受信、項目 7 を参照) を確認する。
2. loopMIDI を終了する。`tourbox-midi list-ports` で 2 つのポートが消えたことを確認する。
3. 10 秒以上待ち、ログに「MIDI 入力ポートが一覧から消えました。」と、出力側の消失のログが出ることを確認する。出力側のログは、通常は「MIDI 出力ポートが一覧から消えました。」である。消失を検知する前に TourBox を操作した場合は、送信エラーの「MIDI メッセージを送信できませんでした。」になることがある。消失を検知した後の操作は送らずに捨て、`--verbose` のログに「MIDI 出力ポートが未接続のため、メッセージを捨てました。」が出る。
4. loopMIDI を起動し、同じ名前の 2 つのポートがあることを確認する (なければ作り直す)。
5. 5 秒以内に「MIDI 出力ポートを開きました。」と「MIDI 入力ポートを開きました。」が出ることを確認する。
6. 1 と同じ送信と受信ができることを確認する。DAW 側でポートを選び直す必要があれば、選び直す。

### 5b. loopMIDI の短時間の再起動 (Windows)

1. 項目 5 の 1 と同じ状態にする。
2. loopMIDI を終了し、5 秒未満で起動し直す。
3. ポートの消失と開き直しのログが出たかと、送信と受信が自動で復帰したかを記録する。
4. 受信が復帰しない場合は、設定ファイルを正しい内容のまま保存し直す (「loopMIDI を再起動したときの復帰」の節を参照)。「MIDI 入力ポートを開き直します。」と「設定ファイルを再読込して反映しました。」が出て、DAW から送った CC で手応えが変わるようになることを確認する。
5. 送信が復帰しない場合は、そのことを記録し、本アプリを起動し直すと復帰することを確認する。

### 6. 設定ファイルの自動再読込 (両 OS)

1. 項目 3 の状態で、設定ファイルの `tall = { note = 60 }` を `tall = { note = 61 }` に変えて保存する。
2. ログに「設定ファイルを再読込して反映しました。」(`changed=map`) が出て、Tall で Note 61 が届くことを確認する。
3. `tall = { note = 128 }` に変えて保存する。
4. ログに「設定ファイルの再読込に失敗したため、現在の設定を維持します」と、行番号付きの理由が出ることを確認する。本アプリが終了せず、Tall で Note 61 が届き続けることを確認する。
5. 設定ファイルを元に戻して保存する。

### 7. MIDI 入力によるハプティクスの切り替え (両 OS)

項目 3 の状態で、DAW から入力ポート `TourBox MIDI In` のチャンネル 16 に CC を送り、次を確認する。
ハプティクス設定が変わる CC を受信すると、ログに「受信した Control Change でハプティクス設定を変更します。」が出る。
設定が変わらない値 (現在と同じ強度や速度に当たる値) の CC では、このログは出ず、手応えも変わらない。
`--verbose` では、設定が変わらない CC も「MIDI 入力で Control Change を受信しました。」の debug ログで確認できる。

1. CC 100 の値 0 を送ると Knob の手応えがなくなり、値 127 を送ると強い手応えに戻る。
2. CC 110 の値 0 を送ると、Side を押している間の Knob の手応えがなくなる。Side を押している間の Knob の手応えは、CC 100 では変わらない。
3. CC 102 の値を 127、64、0 の順に送ると、Scroll の手応えの速度が遅い、中速、速いの順に変わる。
4. CC 99 の値 0 を送ると全軸の手応えがなくなり、値 127 を送ると元に戻る。

### 8. USB での複数起動 (両 OS、USB 接続のみ)

1. TourBox を USB ケーブルで接続し、1 つ目の端末で `tourbox-midi dump` を実行する。「接続しました。」が表示されることを確認する。
2. 2 つ目の端末で `tourbox-midi dump` を実行する。
3. 2 つ目の端末に「TourBox に接続できませんでした: TourBox のポートは他のプロセスが使用中です。」と「N 秒後に再接続を試みます。」が繰り返し表示され、「BLE のスキャンを開始しました。」が表示されないことを確認する。再試行の間隔は 1 秒から 2 倍ずつ延び、最大 30 秒になる。
4. 1 つ目を Ctrl+C で終了すると、次の再試行で 2 つ目に「接続しました。」が表示されることを確認する。

BLE での複数起動は保証しないので、確認の対象外である。

### 9. スリープからの復帰 (両 OS)

1. TourBox を接続した状態で `tourbox-midi dump` を実行し、「接続しました。」が表示されることを確認する。使用している接続方式 (USB か BLE) を記録する。
2. PC をスリープさせ、しばらくしてから復帰させる。
3. TourBox のケーブルや電源に触れずに待つ。切断を検知した場合は、「切断しました。」の後に「接続しました。」が表示される。再接続の間隔は最大 30 秒である。
4. TourBox を操作し、操作が表示されることを確認する。
5. 「切断しました。」が表示されないまま操作も表示されない場合は、OS と接続方式を記録する。切断の通知が届かない環境に当たる。

## 開発

### 開発環境の準備

コミット時のチェックには lefthook を使う。
lefthook は crates.io で配布されていないため、OS のパッケージマネージャで導入する。
Windows で動作を確認した lefthook のバージョンは 2.1.14 である。

```sh
# Windows
winget install evilmartians.lefthook

# macOS
brew install lefthook
```

clone した後に、リポジトリの直下で次のコマンドを 1 回実行してフックを登録する。

```sh
lefthook install
```

lefthook はコミット時に、ステージしたファイルに応じて CI と同じチェックを実行し、コミットメッセージが Conventional Commits の形式 (`type: 要約`) であるかを検査する。

### 検査

CI (GitHub Actions) は、windows-latest と macos-latest の両方で次の 5 つを実行する。

```sh
cargo fmt --all -- --check
cargo build
cargo clippy --all-targets -- -D warnings
cargo test
cargo lint-adr
```

`cargo build` は、テスト用の `fake` feature を含まない出荷構成のコンパイル検査である ([ADR-0015](docs/adr/0015-テスト用のfakefeatureは自己dev-dependencyで有効化しCIでは出荷構成のビルドも検査する.md))。
`cargo lint-adr` は ADR の形式検査である ([ADR-0012](docs/adr/0012-ADRの形式検査はxtaskで実装する.md))。

### ワークスペースの構成

| パス | 内容 |
|---|---|
| `crates/tourbox` | TourBox との通信ライブラリ。プロトコルの変換、USB と BLE の接続、初期化と再接続を担い、MIDI には依存しない |
| `crates/tourbox-midi` | 設定ファイルに従って、操作を MIDI メッセージに変換する実行ファイル |
| `xtask` | ADR の形式検査 (`cargo lint-adr`) |

### 設計資料

- 設計上の決定とその理由: [docs/adr/](docs/adr/README.md)
- 設計書: [docs/spec/2026-09-23-tourbox-midi-driver-design.md](docs/spec/2026-09-23-tourbox-midi-driver-design.md)
- 実装計画: [docs/plan/2026-09-23-tourbox-midi-driver.md](docs/plan/2026-09-23-tourbox-midi-driver.md)
- ハプティクス設定の実機キャプチャ: [docs/protocol/haptic-captures.md](docs/protocol/haptic-captures.md)
