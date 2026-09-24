# ADR-0015: テスト用の fake feature は自己 dev-dependency で有効化し、CI では出荷構成のビルドも検査する

- 状態: 承認 (2026-09-23、Discord で承認)
- 日付: 2026-09-23

## 文脈

ライブラリ `tourbox` はテスト用のメモリ内トランスポートを `fake` feature で公開する (設計書 4.2 節)。
設計書 3.2 節は「`cargo test` はワークスペース既定でこの feature を含めるよう、ワークスペースの設定で明示する」と書くが、Cargo にはワークスペース全体でテスト時だけ feature を有効にする設定はない。
実装計画 Task 1 は、`tourbox` の dev-dependencies で自クレートを `features = ["fake"]` 付きで参照する方式を採った。
この方式では、resolver 2 の規則により、テストターゲットを含む呼び出し (`cargo test`、`cargo clippy --all-targets`) では同じ呼び出し内の `tourbox` に `fake` が統合され、`tourbox-midi` の実行ファイルも `fake` 付きの `tourbox` とリンクされる。
一方、`cargo build` では `fake` が無効になる。
CI と lefthook の検査 (`fmt`、`clippy --all-targets`、`test`、`lint-adr`) はいずれもテストターゲットを含むため、出荷構成 (`fake` なし) をコンパイルする検査がない。
`cfg(feature = "fake")` で囲み忘れた参照があっても CI は成功し、`cargo build` だけが失敗する。
macOS はローカルで確認できないため、macOS の出荷構成を検証できるのは CI だけである。

## 決定

`fake` feature の有効化は `tourbox` の自己 dev-dependency (`tourbox = { path = ".", features = ["fake"] }`) で行い、ワークスペース側の設定は追加しない。
設計書 3.2 節の文言はこの方式に合わせて修正する。
CI と lefthook の pre-commit に `cargo build` を加え、出荷構成 (`fake` なし) がコンパイルできることを検査する。
検査の一覧は `fmt`、`build`、`clippy --all-targets`、`test`、`lint-adr` の 5 つになる。

## 検討した選択肢

- `cargo clippy -- -D warnings` (`--all-targets` なし) を加える。出荷構成の検査になり lint も掛かるが、`--all-targets` 付きと 2 回 clippy が走り、目的が読み取りにくい。`cargo build` のほうが意図が明確である。
- `.cargo/config.toml` のエイリアスで `cargo test` を `cargo test --features fake` に置き換える。組み込みコマンドはエイリアスで上書きできないため実現できない。
- `fake` を既定 feature にし、出荷ビルドで `--no-default-features` を付ける。利用者の `cargo build` にフェイクが混入し、最小驚きに反する。
- 出荷構成の検査を加えない。Task 4 で `fake` を導入した後、囲み忘れを CI が検出できない。

## 結果

- 実装計画の Global Constraints と設計書 8.2 節の検査一覧に `cargo build` を加える。ADR-0009 の pre-commit の一覧 (3 つ) は、ADR-0012 の `lint-adr` と本 ADR の `build` を加えた 5 つになる。
- `cargo test -p tourbox-midi` を単独で実行した場合は `fake` が有効にならない。`tourbox-midi` のテストがフェイクを必要とする場合は、その時点で `tourbox-midi` の dev-dependencies に `tourbox = { path = "../tourbox", features = ["fake"] }` を加える。
- 検査が 1 つ増えるが、`cargo build` の成果物は `cargo test` と共有されないため、pre-commit の所要時間が増える。問題になったら `build` と `test` を pre-push に移す。
- 2026-09-24: 検査に actionlint と zizmor を加え、7 つになった (ADR-0019)。
