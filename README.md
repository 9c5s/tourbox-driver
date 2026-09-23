# tourbox-driver

TourBox Elite を公式コンソールアプリなしで MIDI コントローラーとして使うためのドライバである。
デバイスとの通信 (USB と BLE) を担うライブラリ `tourbox` と、操作を MIDI メッセージに変換する常駐アプリ `tourbox-midi` からなる。
対象 OS は Windows と macOS である。

## 開発環境の準備

Rust の stable ツールチェーンでビルドする。

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

以後はコミットのたびに、CI と同じチェック (`cargo fmt`、`cargo clippy`、`cargo test`、`cargo lint-adr`) と、コミットメッセージが Conventional Commits の形式 (`type: 要約`) であるかの検査を実行する。

## Windows の仮想 MIDI ポート

Windows は OS 標準の機能では仮想 MIDI ポートを作成できない。
[loopMIDI](https://www.tobias-erichsen.de/software/loopmidi.html) で仮想 MIDI ポートを作成しておき、本アプリはその既存ポートを選んで使う。
この方式にした理由は [ADR-0004](docs/adr/0004-Windowsの仮想MIDIポートはloopMIDIを併用しteVirtualMIDISDKは組み込まない.md) に記録している。

## macOS の Bluetooth 権限

macOS 11 以降で BLE 接続を使うには、本アプリを起動するターミナルアプリに Bluetooth の使用を許可する必要がある。
「システム設定 > プライバシーとセキュリティ > Bluetooth」で、使用するターミナルアプリ (ターミナル、iTerm2 など) を許可する。

## 設定ファイル

未記載である。
設定ファイルの読込を実装した後に書く。

## 受け入れ確認

未記載である。
全機能を実装した後に、手動で確認する手順を書く。

## 設計資料

- 設計上の決定とその理由: [docs/adr/](docs/adr/README.md)
- 設計書: [docs/spec/2026-09-23-tourbox-midi-driver-design.md](docs/spec/2026-09-23-tourbox-midi-driver-design.md)
