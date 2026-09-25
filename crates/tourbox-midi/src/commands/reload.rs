//! 設定ファイルの再読込の手順 (設計書 5.4 節)。

use std::path::Path;

use tourbox::protocol::HapticConfig;
use tourbox::transport::ConnectionConfig;
use tracing::{info, warn};

use crate::config::{diff, Config, HapticsControlConfig, MappingSet, Section};
use crate::midi::PortMode;

/// 再読込した設定の反映先。各操作は設計書 5.4 節の反映の手順 1〜5 に当たる。
pub(super) trait ReloadTarget {
    /// engine の `release_all` の Off を出力へ送り、修飾状態と押下状態を初期化する。
    fn release_all(&mut self);

    /// engine の割り当てを差し替える。
    fn replace_mapping(&mut self, mapping: MappingSet);

    /// ハプティクス制御を作り直し、返った基準の設定をデバイスへ送る。
    fn reset_haptics(&mut self, control: HapticsControlConfig, base: HapticConfig);

    /// 出力ポートを `name` で開き直す。
    fn reopen_output(&mut self, name: String);

    /// 入力ポートを閉じ、`name` があればその名前で開き直す。None なら入力機能を無効にする。
    fn reopen_input(&mut self, name: Option<String>);

    /// デバイスを止めて完了を待ってから、新しい設定で接続し直す。
    async fn restart_device(&mut self, connection: ConnectionConfig, haptics: HapticConfig);
}

/// `path` の設定ファイルを読み直し、検証に通れば `current` からの差分に従って `target` へ反映し、
/// `current` を置き換える。読めないか検証に通らなければ、何も反映せずに現在の設定を維持する。
///
/// `input_mode` は入力ポートの開き方である。`Existing` では差分がなくても入力ポートを開き直す。
pub(super) async fn reload(
    target: &mut impl ReloadTarget,
    path: &Path,
    current: &mut Config,
    input_mode: PortMode,
) {
    let new = match Config::load(path) {
        Ok(new) => new,
        Err(error) => {
            warn!("設定ファイルの再読込に失敗したため、現在の設定を維持します: {error}");
            return;
        }
    };
    let changed = diff(current, &new);
    target.release_all();
    target.replace_mapping(new.resolve_mapping());
    target.reset_haptics(new.haptics_control(), new.to_haptic_config());
    if changed.contains(Section::MidiOutput) {
        target.reopen_output(new.midi.output.clone());
    }
    // Existing の入力ポートは消失を見逃すことがあるので、再読込を復旧の手段として毎回開き直す
    let reopens_existing = input_mode == PortMode::Existing && new.midi.input.is_some();
    if changed.contains(Section::MidiInput) || reopens_existing {
        target.reopen_input(new.midi.input.clone());
    }
    if changed.contains(Section::Device) {
        target
            .restart_device(new.to_connection_config(), new.to_haptic_config())
            .await;
    }
    info!(changed = %changed, "設定ファイルを再読込して反映しました。");
    warn_if_haptics_control_is_disabled(&new);
    *current = new;
}

/// `[haptics.control]` があって `midi.input` がなければ、ハプティクス制御が無効であることを警告する。
pub(super) fn warn_if_haptics_control_is_disabled(config: &Config) {
    if config.midi.input.is_none() && config.haptics.control != HapticsControlConfig::default() {
        warn!(
            "[haptics.control] が設定されていますが、midi.input がないため、MIDI 入力によるハプティクス制御は無効です。"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::TempDir;
    use tracing::Level;

    use super::super::captured_logs::CapturedLogs;
    use super::*;

    /// 反映先に対して行われた操作。
    #[derive(Debug, PartialEq)]
    enum Step {
        ReleaseAll,
        ReplaceMapping(MappingSet),
        ResetHaptics(HapticsControlConfig, HapticConfig),
        ReopenOutput(String),
        ReopenInput(Option<String>),
        RestartDevice(ConnectionConfig, HapticConfig),
    }

    /// 行われた操作を順に記録する反映先。
    #[derive(Default)]
    struct Recorder(Vec<Step>);

    impl ReloadTarget for Recorder {
        fn release_all(&mut self) {
            self.0.push(Step::ReleaseAll);
        }

        fn replace_mapping(&mut self, mapping: MappingSet) {
            self.0.push(Step::ReplaceMapping(mapping));
        }

        fn reset_haptics(&mut self, control: HapticsControlConfig, base: HapticConfig) {
            self.0.push(Step::ResetHaptics(control, base));
        }

        fn reopen_output(&mut self, name: String) {
            self.0.push(Step::ReopenOutput(name));
        }

        fn reopen_input(&mut self, name: Option<String>) {
            self.0.push(Step::ReopenInput(name));
        }

        async fn restart_device(&mut self, connection: ConnectionConfig, haptics: HapticConfig) {
            self.0.push(Step::RestartDevice(connection, haptics));
        }
    }

    /// 入力ポートを持つ設定。
    const BASE: &str = r#"[midi]
output = "TourBox MIDI"
input = "TourBox MIDI In"
channel = 1

[map]
tall = { note = 60 }
top = { cc = 20 }
"#;

    fn parse(text: &str) -> Config {
        Config::parse(text, Path::new("config.toml"))
            .unwrap_or_else(|error| panic!("検証に通る必要があります: {error}"))
    }

    /// `input` の行だけを変えられる設定。
    fn with_input(input: Option<&str>) -> String {
        let input = input.map_or(String::new(), |name| format!("input = \"{name}\"\n"));
        format!("[midi]\noutput = \"TourBox MIDI\"\n{input}channel = 1\n[map]\ntall = {{ note = 60 }}\n")
    }

    /// 一時ディレクトリに置いた設定ファイル。
    struct ConfigFile {
        _dir: TempDir,
        path: PathBuf,
    }

    /// `text` を書いた設定ファイルを作る。None ならファイルを作らない。
    fn config_file(text: Option<&str>) -> ConfigFile {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れる必要があります。");
        let path = dir.path().join("config.toml");
        if let Some(text) = text {
            fs::write(&path, text).expect("設定ファイルを書き込める必要があります。");
        }
        ConfigFile { _dir: dir, path }
    }

    /// `current` の設定で常駐している状態で、内容が `file` の設定ファイルを再読込し、
    /// 行われた操作と再読込後の設定を返す。
    async fn reloaded(
        current: &str,
        file: Option<&str>,
        input_mode: PortMode,
    ) -> (Vec<Step>, Config) {
        let file = config_file(file);
        let mut current = parse(current);
        let mut recorder = Recorder::default();
        reload(&mut recorder, &file.path, &mut current, input_mode).await;
        (recorder.0, current)
    }

    /// 差分にかかわらず毎回行う手順 1〜3。
    fn always(new: &Config) -> Vec<Step> {
        vec![
            Step::ReleaseAll,
            Step::ReplaceMapping(new.resolve_mapping()),
            Step::ResetHaptics(new.haptics_control(), new.to_haptic_config()),
        ]
    }

    #[tokio::test]
    async fn changes_of_every_section_are_applied_in_documented_order() {
        let changed = r#"[device]
transport = "usb"

[midi]
output = "loopMIDI Port"
input = "loopMIDI In"
channel = 2

[haptics]
knob = { strength = "weak" }

[haptics.control]
master = { cc = 99 }

[map]
tall = { note = 61 }
"#;
        let (old, new) = (parse(BASE), parse(changed));
        // 古い設定の値を渡す誤りを検出できるよう、各値が変わっていることを確かめておく
        assert_ne!(old.resolve_mapping(), new.resolve_mapping());
        assert_ne!(old.haptics_control(), new.haptics_control());
        assert_ne!(old.to_haptic_config(), new.to_haptic_config());
        assert_ne!(old.to_connection_config(), new.to_connection_config());

        let (steps, current) = reloaded(BASE, Some(changed), PortMode::Existing).await;

        assert_eq!(
            steps,
            [
                Step::ReleaseAll,
                Step::ReplaceMapping(new.resolve_mapping()),
                Step::ResetHaptics(new.haptics_control(), new.to_haptic_config()),
                Step::ReopenOutput("loopMIDI Port".to_owned()),
                Step::ReopenInput(Some("loopMIDI In".to_owned())),
                Step::RestartDevice(new.to_connection_config(), new.to_haptic_config()),
            ],
            "押下の解放、割り当ての差し替え、ハプティクスの作り直し、ポートの開き直し、デバイスの再接続の順に、新しい設定で反映する必要があります。"
        );
        assert_eq!(
            current, new,
            "反映した設定を現在の設定にする必要があります。"
        );
    }

    #[tokio::test]
    async fn existing_input_is_reopened_even_when_nothing_changed() {
        let (steps, current) = reloaded(BASE, Some(BASE), PortMode::Existing).await;

        let base = parse(BASE);
        let mut expected = always(&base);
        expected.push(Step::ReopenInput(Some("TourBox MIDI In".to_owned())));
        assert_eq!(
            steps, expected,
            "差分がなくても手順 1〜3 を行い、Existing の入力ポートだけは開き直す必要があります。"
        );
        assert_eq!(
            current, base,
            "差分がなければ設定は変わらない必要があります。"
        );
    }

    #[tokio::test]
    async fn input_port_is_reopened_according_to_mode_and_change() {
        let cases = [
            (PortMode::Existing, Some("A"), Some("A"), Some(Some("A"))),
            (PortMode::Existing, Some("A"), Some("B"), Some(Some("B"))),
            (PortMode::Existing, None, Some("A"), Some(Some("A"))),
            (PortMode::Existing, Some("A"), None, Some(None)),
            (PortMode::Existing, None, None, None),
            (PortMode::Virtual, Some("A"), Some("A"), None),
            (PortMode::Virtual, Some("A"), Some("B"), Some(Some("B"))),
            (PortMode::Virtual, None, Some("A"), Some(Some("A"))),
            (PortMode::Virtual, Some("A"), None, Some(None)),
            (PortMode::Virtual, None, None, None),
        ];

        for (mode, current, new, reopened) in cases {
            let new_text = with_input(new);
            let (steps, _) = reloaded(&with_input(current), Some(&new_text), mode).await;

            let mut expected = always(&parse(&new_text));
            if let Some(name) = reopened {
                expected.push(Step::ReopenInput(name.map(str::to_owned)));
            }
            assert_eq!(
                steps, expected,
                "{mode:?} で入力ポートの名前が {current:?} から {new:?} になる再読込では、入力ポートを {reopened:?} で開き直す (None は開き直さない) 必要があります。"
            );
        }
    }

    #[tokio::test]
    async fn haptics_control_without_midi_input_is_warned_on_every_reload() {
        let control = "[haptics.control]\nknob = { cc = 100 }\n";
        for (case, file, warned) in [
            (
                "制御があり入力がない",
                format!("{}{control}", with_input(None)),
                true,
            ),
            (
                "制御も入力もある",
                format!("{}{control}", with_input(Some("A"))),
                false,
            ),
            ("制御も入力もない", with_input(None), false),
        ] {
            let logs = CapturedLogs::start();

            // 差分のない再読込でも、そのたびに警告する
            reloaded(&file, Some(&file), PortMode::Existing).await;

            let expected = if warned { "出す" } else { "出さない" };
            assert_eq!(
                logs.contains(Level::WARN, "MIDI 入力によるハプティクス制御は無効です。"),
                warned,
                "{case}設定を再読込したときは、警告を{expected}必要があります。"
            );
        }
    }

    #[tokio::test]
    async fn unreadable_or_invalid_file_keeps_current_config_and_applies_nothing() {
        let out_of_range = BASE.replace("top = { cc = 20 }", "top = { cc = 128 }");
        let cases = [
            ("空ファイル", Some("")),
            ("構文エラー", Some("[midi\noutput = \"TourBox MIDI\"\n")),
            ("検証エラー", Some(out_of_range.as_str())),
            ("ファイルがない", None),
        ];

        for (name, file) in cases {
            let (steps, current) = reloaded(BASE, file, PortMode::Existing).await;

            assert_eq!(steps, [], "{name}では何も反映しない必要があります。");
            assert_eq!(
                current,
                parse(BASE),
                "{name}では現在の設定を維持する必要があります。"
            );
        }
    }

    #[tokio::test]
    async fn partially_written_file_that_passes_validation_is_applied() {
        // top の行を書く前の、保存途中の内容
        let partial = &BASE[..BASE
            .find("top = ")
            .expect("BASE に top の行がある必要があります。")];
        let new = parse(partial);
        assert_ne!(new.map, parse(BASE).map);

        let (steps, current) = reloaded(BASE, Some(partial), PortMode::Existing).await;

        let mut expected = always(&new);
        expected.push(Step::ReopenInput(Some("TourBox MIDI In".to_owned())));
        assert_eq!(
            steps, expected,
            "保存途中でも検証に通る内容は反映する必要があります。"
        );
        assert_eq!(
            current, new,
            "検証に通った内容を現在の設定にする必要があります。"
        );
    }
}
