# ADR-0004: Windows の仮想 MIDI ポートは loopMIDI を併用し、teVirtualMIDI SDK は組み込まない

- 状態: 承認 (2026-09-23、Discord で承認)
- 日付: 2026-09-23

## 文脈

macOS は CoreMIDI が仮想ポートを標準で作れるため、アプリ単体で DAW から見える。
Windows は OS 標準では仮想 MIDI ポートを作れず、loopMIDI のような外部ツールを併用するのが一般的である。
teVirtualMIDI SDK (loopMIDI と同じ作者) を組み込めばアプリ起動時に仮想ポートを自動作成できるが、公式サイトによると同 SDK はフリーウェアでもシェアウェアでもなく、配布には作者の事前許可が必要で、料金は個別交渉である。
SDK 自体にドライバは含まれず、動作には loopMIDI か rtpMIDI の事前インストールが必要である。
Windows 11 の Windows MIDI Services は OS 標準のループバックポートを提供するが、ユーザーの環境は Windows 10 である。

## 決定

アプリは「OS の MIDI 出力ポートを選択して送る」方式に統一する。
macOS では midir で仮想ポートを作成し、Windows では利用者が loopMIDI でポートを作り、アプリは名前の一致で既存ポートを選ぶ。
teVirtualMIDI SDK は組み込まない。

## 検討した選択肢

- teVirtualMIDI SDK を組み込む。ポートを手動で作る手間は省けるが、loopMIDI のインストールは結局必要で、配布に作者の許可が要り、個人プロジェクトには手続きの負担が大きい。
- Windows MIDI Services を使う。外部ツールが不要になるが、Windows 11 限定で対応ライブラリも限られる。
- 自前で仮想 MIDI ドライバを書く。カーネルドライバの開発と署名が必要で、個人プロジェクトの範囲を超える。

## 結果

- ポート選択の抽象の裏側を差し替えるだけで、将来 Windows MIDI Services に切り替えられる。
- Windows では loopMIDI の後起動や再起動に備えて、ポートの再試行と一覧の再評価が設計に加わった (設計書 6.1 節)。
- 5 秒未満の短い再起動では自動復帰を保証しない (設計書 6.2 節)。
