# ADR-0019: GitHub Actions の定義は actionlint と zizmor で検査する

- 状態: 承認 (2026-09-24、ユーザー指示)
- 日付: 2026-09-24

## 文脈

CI の定義 `.github/workflows/ci.yml` は、ADR-0009 の lefthook と ADR-0012 の `cargo lint-adr` の検査対象に入っていない。
ワークフローの構文や式の誤りは、push して CI が失敗するまで分からない。
また、GitHub Actions には、権限の広すぎるトークンやピン留めされていないアクションなど、静的に検出できるセキュリティ上の問題がある。
ユーザーは 2026-09-24 に、lefthook に actionlint と zizmor を加えるよう指示した。
actionlint (rhysd/actionlint、Go 製) はワークフローの構文、式、シェルスクリプトを検査する。
zizmor (zizmorcore/zizmor、Rust 製) はワークフローのセキュリティ上の問題を検査する。
どちらも開発機には導入済みである (actionlint 1.7.12、zizmor 1.30.1)。

## 決定

lefthook の pre-commit に `actionlint` と `zizmor` のジョブを加え、`.github/workflows/` 配下のファイルがステージされたときに実行する。
CI にも同じ 2 つの検査を加え、ローカルと CI で検査内容を揃える (ADR-0015 の方針と同じ)。
導入は OS のパッケージマネージャで行い (ADR-0009 と同じ)、Windows は winget (`rhysd.actionlint`) と `cargo install zizmor`、macOS は Homebrew (`actionlint`、`zizmor`) を README に書く。
zizmor の指摘は既定の重大度で扱い、抑制が必要な場合はワークフロー内のコメントで理由を書く。

## 検討した選択肢

- CI だけで実行する。push するまで分からない点は解消されず、lefthook で CI と同じ検査を行う ADR-0009 の方針と合わない。
- どちらか一方だけを入れる。actionlint は構文と式、zizmor はセキュリティと、検出する問題が重ならない。
- `cargo xtask` から呼び出す。外部ツールの有無を xtask が吸収する利点はあるが、lefthook のジョブで足りる。

## 結果

- 検査の一覧は fmt、build、clippy、test、lint-adr、actionlint、zizmor の 7 つになる。ADR-0009 と ADR-0015 の一覧はこの ADR で置き換える。
- ツールが未導入の環境では pre-commit が失敗する。README の開発環境の準備に導入手順を書く。
- zizmor の検査で既存のワークフローに指摘が出た場合は、この ADR の実装で直す。
- 2026-09-24: ワークフローの検査ジョブは ubuntu-latest で実行する。YAML の静的検査は OS に依存せず、zizmor-action と actionlint の Docker イメージが Linux のランナーを前提とするためである。実装計画の Global Constraints の対象 OS は製品のビルドと検査の対象を定めたもので、この運用と矛盾しない。
