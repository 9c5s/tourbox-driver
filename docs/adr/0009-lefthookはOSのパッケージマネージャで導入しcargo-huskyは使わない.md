# ADR-0009: lefthook は OS のパッケージマネージャで導入し、cargo-husky は使わない

- 状態: 承認 (2026-09-23、Discord で承認)
- 日付: 2026-09-23

## 文脈

ユーザーは、CI と同じチェック (fmt、clippy、test) をコミット時にローカルでも実行することと、Conventional Commits の形式検査を求めた。
ツールは lefthook を希望し、cargo の dev-dependency のように入れられればそうしたいとの意向だった。
確認したところ、lefthook は Go 製で crates.io には公開されておらず、配布は npm、Homebrew、winget、GitHub Releases である。
cargo の dev-dependency はライブラリをテストにリンクする仕組みで、ツールを書く場所ではない。

## 決定

lefthook を使い、Windows は winget、macOS は Homebrew で導入する。
README に手順とバージョンを書き、clone 後に `lefthook install` を 1 回実行する。
pre-commit で `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` を実行し、commit-msg で Conventional Commits の形式を正規表現で検査する。
Node などの追加依存は入れない。

## 検討した選択肢

- cargo-husky を dev-dependency に入れる。`cargo test` の初回実行でフックが自動で入り追加のインストールが不要だが、最終リリースが古く保守が止まっており、フックの記述も素朴である。
- cargo-run-bin で `Cargo.toml` にツールのバージョンを固定する。lefthook が crates.io にないため対象にできない。

## 結果

- ツール自体の導入は cargo で管理できないが、設定は `lefthook.yml` 1 ファイルで分かりやすい。
- test の実行時間が問題になったら、test だけを pre-push に移す。
