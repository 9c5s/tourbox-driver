//! 設定ファイルの変更の監視。

use std::path::{self, Path};
use std::time::Duration;

use anyhow::Context;
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{new_debouncer, DebounceEventResult, DebouncedEventKind, Debouncer};
use tokio::sync::mpsc;
use tracing::warn;

/// 連続したファイルのイベントを 1 回にまとめる時間。
const DEBOUNCE: Duration = Duration::from_millis(300);

/// 設定ファイルの監視。破棄すると監視が止まる。
pub struct ConfigWatcher {
    _debouncer: Debouncer<RecommendedWatcher>,
}

/// `path` の親ディレクトリを監視し、`path` と同じ名前のファイルが変わったら通知する。
///
/// 置換して保存するエディタに追従するため、ファイルではなく親ディレクトリを監視する。
/// 連続した変更は 300 ms のデバウンスで 1 回にまとめ、受け取られていない通知が
/// あるうちの変更も同じ通知にまとめる。親ディレクトリがなければエラーを返す。
pub fn watch(path: &Path) -> anyhow::Result<(ConfigWatcher, mpsc::Receiver<()>)> {
    let path = path::absolute(path).with_context(|| {
        format!(
            "設定ファイル {} の絶対パスを求められませんでした。",
            path.display()
        )
    })?;
    let (Some(directory), Some(name)) = (path.parent(), path.file_name()) else {
        anyhow::bail!(
            "設定ファイル {} の親ディレクトリを特定できません。",
            path.display()
        );
    };
    let name = name.to_owned();
    // 未処理の通知は 1 つあれば足りる
    let (tx, changes) = mpsc::channel(1);
    let mut debouncer = new_debouncer(DEBOUNCE, move |result: DebounceEventResult| {
        match result {
            Ok(events) => {
                // AnyContinuous は変更が続いている途中の知らせなので、止んだ後の Any だけを通知にする
                let changed = events.iter().any(|event| {
                    event.kind == DebouncedEventKind::Any
                        && event.path.file_name() == Some(name.as_os_str())
                });
                if changed {
                    // 満杯なら受け取られていない通知があるので、この変更はそれにまとめる
                    let _ = tx.try_send(());
                }
            }
            Err(error) => warn!("設定ファイルの監視でエラーが発生しました: {error}"),
        }
    })
    .context("設定ファイルの監視を開始できませんでした。")?;
    debouncer
        .watcher()
        .watch(directory, RecursiveMode::NonRecursive)
        .with_context(|| {
            format!(
                "設定ファイルのディレクトリ {} を監視できませんでした。",
                directory.display()
            )
        })?;
    Ok((
        ConfigWatcher {
            _debouncer: debouncer,
        },
        changes,
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tokio::time::{sleep, timeout};

    use super::*;

    const NAME: &str = "config.toml";
    /// 連続した書き込みの間隔。デバウンスの時間より十分短くする。
    const BURST_INTERVAL: Duration = Duration::from_millis(50);
    /// 通知が届くまで待つ上限。デバウンスの時間と OS の通知の遅れに余裕を持たせる。
    const NOTIFY_LIMIT: Duration = Duration::from_secs(2);
    /// 通知が来ないことを確かめる時間。デバウンスの時間の 2 倍 (まとめ送りの最大の遅れ) より長くする。
    const QUIET: Duration = Duration::from_secs(1);

    async fn notified(changes: &mut mpsc::Receiver<()>) -> bool {
        matches!(timeout(NOTIFY_LIMIT, changes.recv()).await, Ok(Some(())))
    }

    async fn stays_quiet(changes: &mut mpsc::Receiver<()>) -> bool {
        timeout(QUIET, changes.recv()).await.is_err()
    }

    #[tokio::test]
    async fn consecutive_writes_to_target_are_notified_once() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れる必要があります。");
        let path = dir.path().join(NAME);
        let (_watcher, mut changes) = watch(&path).expect("監視を開始できる必要があります。");

        for content in [
            "[midi]\n",
            "[midi]\noutput = \"A\"\n",
            "[midi]\noutput = \"AB\"\n",
        ] {
            fs::write(&path, content).expect("書き込める必要があります。");
            sleep(BURST_INTERVAL).await;
        }

        assert!(
            notified(&mut changes).await,
            "対象のファイルへの書き込みを通知する必要があります。"
        );
        assert!(
            stays_quiet(&mut changes).await,
            "連続した書き込みは 1 回の通知にまとめる必要があります。"
        );
    }

    #[tokio::test]
    async fn changes_to_other_files_in_same_directory_are_not_notified() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れる必要があります。");
        let path = dir.path().join(NAME);
        let (_watcher, mut changes) = watch(&path).expect("監視を開始できる必要があります。");

        let other = dir.path().join("other.toml");
        for content in ["a", "ab"] {
            fs::write(&other, content).expect("書き込める必要があります。");
            sleep(BURST_INTERVAL).await;
        }
        fs::rename(&other, dir.path().join("config.toml.bak"))
            .expect("名前を変えられる必要があります。");

        assert!(
            stays_quiet(&mut changes).await,
            "同じディレクトリの別のファイルの変更は通知しない必要があります。"
        );
        // 監視が動いていることを、対象のファイルの変更が通知されることで確かめる
        fs::write(&path, "[midi]\n").expect("書き込める必要があります。");
        assert!(
            notified(&mut changes).await,
            "別のファイルの変更の後も、対象のファイルの変更は通知する必要があります。"
        );
    }

    #[tokio::test]
    async fn replacing_target_by_renaming_another_file_is_notified() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れる必要があります。");
        let path = dir.path().join(NAME);
        fs::write(&path, "[midi]\n").expect("書き込める必要があります。");
        let (_watcher, mut changes) = watch(&path).expect("監視を開始できる必要があります。");
        // 監視の開始前に作ったファイルのイベントが遅れて届いても、置換の通知と取り違えないよう捨てる
        sleep(QUIET).await;
        while changes.try_recv().is_ok() {}

        let temporary = dir.path().join("config.toml.tmp");
        fs::write(&temporary, "[midi]\noutput = \"A\"\n").expect("書き込める必要があります。");
        fs::rename(&temporary, &path).expect("置換できる必要があります。");

        assert!(
            notified(&mut changes).await,
            "別名で書いたファイルを対象の名前に置き換えたら通知する必要があります。"
        );
    }

    #[test]
    fn watch_fails_when_parent_directory_does_not_exist() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れる必要があります。");
        let path = dir.path().join("missing").join(NAME);

        assert!(
            watch(&path).is_err(),
            "親ディレクトリがなければ監視を開始できない必要があります。"
        );
    }
}
