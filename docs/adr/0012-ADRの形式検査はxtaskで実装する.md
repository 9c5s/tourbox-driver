# ADR-0012: ADR の形式検査は xtask で実装する

- 状態: 承認 (2026-09-23、Discord で承認)
- 日付: 2026-09-23

## 文脈

ユーザーは、参考にした cgss-emusrv と同じく ADR の形式検査 (`lint:adr`) をコミット時と CI で実行するよう指示した。
cgss-emusrv の検査は Bun で動く TypeScript のスクリプトである。
本プロジェクトは Rust で書き (ADR-0002)、Node などの追加依存は入れない方針である (ADR-0009)。
検査の規則は、ファイル名と題名の対応、状態と日付の形式、4 つの節の順序と非空、番号の連番、README の一覧との一致である。

## 決定

ワークスペースに `xtask` クレートを置き、`cargo lint-adr` (`.cargo/config.toml` のエイリアスで `cargo run --package xtask -- lint-adr`) で ADR の形式を検査する。
規則は cgss-emusrv の `scripts/check-adr.ts` と同じにし、外部クレートに依存しない。
検査の本体は純粋関数 (ファイル名と内容の対応表を受け取り、問題の文の一覧を返す) にして単体テストする。
lefthook の pre-commit と CI の両方で実行する。

## 検討した選択肢

- cgss-emusrv の TypeScript をそのまま使う。Bun または Node が必要になり、ADR-0009 の方針に反する。
- Python のスクリプトにする。開発機と CI には Python があるが、プロジェクトに 2 つ目の言語が入る。
- シェルスクリプトにする。Windows での互換性が保てない。

## 結果

- ワークスペースの `Cargo.toml` と `xtask` は実装計画 Task 1 より前に存在することになり、Task 1 はそこへクレートを追加する形になる。
- ADR-0000 の「形式の自動検査は行っていない」は本 ADR で置き換える。
- 将来、他の補助コマンド (生成やリリース手順) も `cargo xtask` に足せる。
