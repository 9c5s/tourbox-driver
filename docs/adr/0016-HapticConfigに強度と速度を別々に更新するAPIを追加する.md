# ADR-0016: HapticConfig に強度と速度を別々に更新する API を追加する

- 状態: 承認 (2026-09-23、Discord で承認)
- 日付: 2026-09-23

## 文脈

設計書 4.1 節の `HapticConfig` は、組み合わせ単位の `set(axis, modifier, strength, speed)` と軸単位の `set_axis(axis, strength, speed)` だけを持ち、実装計画 Task 3 もこの 2 つで実装した。
どちらも強度と速度を同時に与える。読み出しの API はない。
一方、設計書 6.2 節の MIDI 入力によるハプティクス制御は、強度を `cc` で、速度を `speed_cc` で別々に受け取る。
`HapticsController` は基準の `HapticConfig` を受け取り、届いた CC に応じて一部の組み合わせの強度だけ、または速度だけを変えた `HapticConfig` を返す必要がある。
現在の API では、速度を保ったまま強度だけを変えることができない。
Task 3 のレビュー (2026-09-23) でこの不足が指摘された。

## 決定

`HapticConfig` に組み合わせ単位の部分更新 `set_strength(axis, modifier, strength)` と `set_speed(axis, modifier, speed)` を追加する。
読み出しの API は追加しない。
`HapticsController` は基準の `HapticConfig` と上書き (組み合わせごとの強度または速度) を別々に保持し、出力のたびに基準の複製へ上書きを部分更新で適用する。
軸単位の一括更新は `Modifier::ALL` の各要素について組み合わせ単位の更新を呼ぶ (除外する組み合わせがあるため、軸単位の API は設けない)。
実装は Task 12 の最初のコミットで行い、設計書 4.1 節に追記する。

## 検討した選択肢

- 読み出し `get(axis, modifier) -> (Strength, Speed)` を追加し、呼び出し側が読み書きする。内部表現がバイト列のため復号の分岐が要り、呼び出し側の手順も長くなる。
- `HapticsController` が全 45 組の強度と速度を自前で保持する。基準の `HapticConfig` から値を取り出せないため、設定ファイル側から別の表も渡す必要があり、API が増える。
- Task 3 で先に追加する。Task 3 の時点では利用者がなく、YAGNI に反する。

## 結果

- Task 12 の着手前に本 ADR を承認し、Task 12 のブリーフに追加を含める。
- 部分更新のテストは、該当オフセットの強度ビット (`0x0c`) または速度ビット (`0x03`) だけが変わり、他の 44 組と同じ組み合わせのもう一方の値が保たれることを確認する。
