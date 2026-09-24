//! 設定ファイルの読込、検証、解決。

mod diff;
mod resolve;
mod schema;
mod validate;
mod watch;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::{env, fs, io};

use tourbox::protocol::HapticConfig;
use tourbox::transport::ConnectionConfig;

pub use diff::{diff, ChangedSections, Section};
pub use resolve::{ButtonAssignment, MappingSet, RotationAssignment};
pub use schema::{
    ButtonEntry, ButtonKind, Config, ControlCc, HapticOverride, HapticSetting,
    HapticsControlConfig, HapticsSection, MapLayer, MapSection, MidiSection, RelativeEncoding,
    RotationEntry, RotationMode,
};
pub use watch::{watch, ConfigWatcher};

/// 設定ファイルのエラー。
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// ファイルを読み込めない。
    #[error("設定ファイル {} を読み込めません: {error}", .path.display())]
    Read { path: PathBuf, error: io::Error },
    /// 構文エラーか検証エラー。`line` は 1 起点の行番号で、特定できない場合は None。
    #[error("{}: {message}", location(.path, .line))]
    Invalid {
        path: PathBuf,
        line: Option<usize>,
        message: String,
    },
}

/// エラーの位置の表示 (「パス の N 行目」か、行番号がなければパスだけ)。
fn location(path: &Path, line: &Option<usize>) -> String {
    match line {
        Some(line) => format!("{} の {line} 行目", path.display()),
        None => path.display().to_string(),
    }
}

/// バイト位置 `offset` を含む行の番号 (1 起点)。
fn line_at(text: &str, offset: usize) -> usize {
    text.bytes()
        .take(offset)
        .filter(|&byte| byte == b'\n')
        .count()
        + 1
}

impl Config {
    /// 設定ファイルを読み込んで検証する。
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = fs::read_to_string(path).map_err(|error| ConfigError::Read {
            path: path.to_owned(),
            error,
        })?;
        Config::parse(&text, path)
    }

    /// 設定ファイルの内容を検証する。`path` はエラーメッセージに使う。
    pub fn parse(text: &str, path: &Path) -> Result<Config, ConfigError> {
        validate::validate(text).map_err(|invalid| ConfigError::Invalid {
            path: path.to_owned(),
            line: invalid.span.map(|span| line_at(text, span.start)),
            message: invalid.message,
        })
    }

    /// `[haptics]` から、デバイスに送るハプティクス設定を作る。
    pub fn to_haptic_config(&self) -> HapticConfig {
        resolve::haptic_config(&self.haptics)
    }

    /// `[device]` から接続の設定を作る。
    pub fn to_connection_config(&self) -> ConnectionConfig {
        self.device.clone()
    }

    /// `[map]` と `midi.channel` から、チャンネルを解決した割り当てを作る。
    pub fn resolve_mapping(&self) -> MappingSet {
        MappingSet::resolve(&self.map, self.midi.channel)
    }

    /// `[haptics.control]` の割り当て。
    pub fn haptics_control(&self) -> HapticsControlConfig {
        self.haptics.control.clone()
    }
}

/// 設定ディレクトリの基点を示す環境変数。
#[cfg(windows)]
const BASE_DIR_VAR: &str = "APPDATA";
/// 基点から設定ファイルまでの相対パス。
#[cfg(windows)]
const RELATIVE_PATH: &str = r"tourbox-midi\config.toml";
#[cfg(not(windows))]
const BASE_DIR_VAR: &str = "HOME";
#[cfg(not(windows))]
const RELATIVE_PATH: &str = "Library/Application Support/tourbox-midi/config.toml";

/// 基点の環境変数がない場合に使う、カレントディレクトリからのパス。
const FALLBACK_PATH: &str = "config.toml";

/// 既定の設定ファイルのパス。
///
/// Windows は `%APPDATA%\tourbox-midi\config.toml`、それ以外は
/// `~/Library/Application Support/tourbox-midi/config.toml` (macOS の場所) で、
/// 基点の環境変数がなければカレントディレクトリの `config.toml` にする。
pub fn default_config_path() -> PathBuf {
    config_path_in(env::var_os(BASE_DIR_VAR))
}

/// 基点のディレクトリ `base_dir` (環境変数の値) の下の設定ファイルのパス。
fn config_path_in(base_dir: Option<OsString>) -> PathBuf {
    match base_dir.filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir).join(RELATIVE_PATH),
        None => PathBuf::from(FALLBACK_PATH),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::error::Error;

    use tourbox::protocol::{Axis, Button, Modifier, Speed, Strength};
    use tourbox::transport::TransportKind;

    use super::*;

    /// リポジトリに同梱する設定例のパス。
    fn example_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.example.toml")
    }

    fn note(note: u8, channel: Option<u8>) -> ButtonEntry {
        ButtonEntry {
            channel,
            kind: ButtonKind::Note {
                note,
                velocity: 127,
            },
        }
    }

    fn absolute(cc: u8, step: u8, invert: bool) -> RotationEntry {
        RotationEntry {
            channel: None,
            cc,
            invert,
            mode: RotationMode::Absolute { step, initial: 0 },
        }
    }

    /// 設定例のボタンと Note 番号。チャンネルはどれも既定 (`midi.channel`) を使う。
    const EXAMPLE_NOTES: [(Button, u8); 14] = [
        (Button::Tall, 60),
        (Button::Short, 61),
        (Button::Top, 62),
        (Button::Side, 63),
        (Button::ScrollPress, 64),
        (Button::DpadUp, 65),
        (Button::DpadDown, 66),
        (Button::DpadLeft, 67),
        (Button::DpadRight, 68),
        (Button::C1, 69),
        (Button::C2, 70),
        (Button::Tour, 71),
        (Button::KnobPress, 72),
        (Button::DialPress, 73),
    ];

    fn load_example() -> Config {
        Config::load(&example_path())
            .unwrap_or_else(|error| panic!("設定例を読み込める必要があります: {error}"))
    }

    /// 設定例の `[haptics.control]` の検証結果。
    fn example_haptics_control() -> HapticsControlConfig {
        HapticsControlConfig {
            channel: Some(15),
            master: Some(99),
            axes: HashMap::from([
                (
                    Axis::Knob,
                    ControlCc {
                        cc: Some(100),
                        speed_cc: None,
                    },
                ),
                (
                    Axis::Scroll,
                    ControlCc {
                        cc: Some(101),
                        speed_cc: Some(102),
                    },
                ),
                (
                    Axis::Dial,
                    ControlCc {
                        cc: Some(103),
                        speed_cc: Some(104),
                    },
                ),
            ]),
            with: HashMap::from([(
                (Button::Side, Axis::Knob),
                ControlCc {
                    cc: Some(110),
                    speed_cc: None,
                },
            )]),
        }
    }

    /// 設定例の検証結果。
    fn example_config() -> Config {
        Config {
            device: ConnectionConfig {
                transport: TransportKind::Auto,
                usb_port: None,
            },
            midi: MidiSection {
                output: "TourBox MIDI Out".to_owned(),
                input: Some("TourBox MIDI In".to_owned()),
                channel: 0,
            },
            haptics: HapticsSection {
                axes: Axis::ALL
                    .into_iter()
                    .map(|axis| {
                        let setting = HapticSetting {
                            strength: Strength::Strong,
                            speed: Speed::Medium,
                        };
                        (axis, setting)
                    })
                    .collect(),
                with: HashMap::from([(
                    (Button::Side, Axis::Knob),
                    HapticOverride {
                        strength: Some(Strength::Weak),
                        speed: None,
                    },
                )]),
                control: example_haptics_control(),
            },
            map: MapSection {
                base: MapLayer {
                    buttons: EXAMPLE_NOTES
                        .into_iter()
                        .map(|(button, number)| (button, note(number, None)))
                        .collect(),
                    rotations: HashMap::from([
                        (Axis::Knob, absolute(1, 1, false)),
                        (
                            Axis::Scroll,
                            RotationEntry {
                                channel: None,
                                cc: 2,
                                invert: false,
                                mode: RotationMode::Relative {
                                    step: 1,
                                    encoding: RelativeEncoding::TwosComplement,
                                },
                            },
                        ),
                        (Axis::Dial, absolute(3, 1, false)),
                    ]),
                },
                with: HashMap::from([(
                    Button::Side,
                    MapLayer {
                        buttons: HashMap::from([(Button::Top, note(74, None))]),
                        rotations: HashMap::from([(Axis::Knob, absolute(11, 1, false))]),
                    },
                )]),
            },
        }
    }

    #[test]
    fn example_file_loads_as_documented() {
        assert_eq!(
            load_example(),
            example_config(),
            "設定例を README の受け入れ確認に記載した割り当てのとおりに解釈する必要があります。"
        );
    }

    #[test]
    fn example_file_assigns_own_message_to_every_control() {
        let mapping = load_example().resolve_mapping();

        // (チャンネル、メッセージの種類、番号) の組
        let mut messages = HashSet::new();
        for button in Button::ALL {
            let assignment = mapping.button(None, button).unwrap_or_else(|| {
                panic!("設定例の基本レイヤで {button:?} に割り当てがある必要があります。")
            });
            let message = match assignment.kind {
                ButtonKind::Note { note, .. } => (assignment.channel, "Note", note),
                ButtonKind::Cc { cc } => (assignment.channel, "CC", cc),
            };
            assert!(
                messages.insert(message),
                "設定例の {button:?} には、ほかの操作と異なるメッセージを割り当てる必要があります: {message:?}"
            );
        }
        for axis in Axis::ALL {
            let assignment = mapping.rotation(None, axis).unwrap_or_else(|| {
                panic!("設定例の基本レイヤで {axis:?} に割り当てがある必要があります。")
            });
            let message = (assignment.channel, "CC", assignment.cc);
            assert!(
                messages.insert(message),
                "設定例の {axis:?} には、ほかの操作と異なるメッセージを割り当てる必要があります: {message:?}"
            );
        }
    }

    #[test]
    fn example_file_resolves_side_layer() {
        let mapping = load_example().resolve_mapping();

        assert_eq!(
            mapping.modifiers(),
            &[Button::Side],
            "設定例の修飾ボタンは Side だけである必要があります。"
        );
        assert_eq!(
            mapping.button(None, Button::Side),
            Some(&ButtonAssignment {
                channel: 0,
                kind: ButtonKind::Note {
                    note: 63,
                    velocity: 127,
                },
            }),
            "修飾ボタンの Side 自身も、基本レイヤの Note 63 を送る必要があります。"
        );
        assert_eq!(
            mapping.rotation(Some(Button::Side), Axis::Knob),
            Some(&RotationAssignment {
                channel: 0,
                cc: 11,
                invert: false,
                mode: RotationMode::Absolute {
                    step: 1,
                    initial: 0,
                },
            }),
            "Side のレイヤでは、Knob が CC 11 を送る必要があります。"
        );
        assert_eq!(
            mapping.button(Some(Button::Side), Button::Top),
            Some(&ButtonAssignment {
                channel: 0,
                kind: ButtonKind::Note {
                    note: 74,
                    velocity: 127,
                },
            }),
            "Side のレイヤでは、Top が Note 74 を送る必要があります。"
        );
    }

    #[test]
    fn example_file_resolves_haptics() {
        let config = load_example();

        let mut expected = HapticConfig::default();
        expected.set(
            Axis::Knob,
            Modifier::Button(Button::Side),
            Strength::Weak,
            Speed::Medium,
        );
        assert_eq!(
            config.to_haptic_config(),
            expected,
            "設定例のハプティクスは、Side を押している間の Knob だけを弱にし、ほかはすべて強、中速にする必要があります。"
        );
        assert_eq!(
            config.haptics_control(),
            example_haptics_control(),
            "設定例の [haptics.control] を解決できる必要があります。"
        );
    }

    #[test]
    fn missing_file_is_read_error_with_path() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("存在しない設定.toml");

        let error =
            Config::load(&path).expect_err("存在しないファイルはエラーにする必要があります。");

        assert!(
            matches!(&error, ConfigError::Read { path: actual, .. } if actual == &path),
            "読込エラーにする必要があります: {error:?}"
        );
        let prefix = format!("設定ファイル {} を読み込めません: ", path.display());
        assert!(
            error.to_string().starts_with(&prefix),
            "パスと原因を表示する必要があります: {error}"
        );
        assert!(
            error.source().is_none(),
            "原因を Display に含めるので、source として重ねて返さない必要があります。"
        );
    }

    #[test]
    fn invalid_error_shows_path_and_line() {
        let error = Config::parse(
            "[midi]\noutput = \"TourBox MIDI\"\nchannel = 1\n[map]\ntop = { cc = 128 }\n",
            Path::new("C:/設定/config.toml"),
        )
        .expect_err("範囲外の CC 番号はエラーにする必要があります。");

        assert_eq!(
            error.to_string(),
            "C:/設定/config.toml の 5 行目: `map.top.cc` は 0〜127 の範囲で指定してください (指定値: 128)。",
            "パス、行番号、原因を表示する必要があります。"
        );
    }

    #[test]
    fn invalid_error_without_line_shows_path_only() {
        let error = Config::parse("", Path::new("config.toml"))
            .expect_err("空ファイルはエラーにする必要があります。");

        assert_eq!(
            error.to_string(),
            "config.toml: 必須のセクション [midi] と [map] がありません。",
            "行番号がなければパスと原因だけを表示する必要があります。"
        );
    }

    #[test]
    fn line_numbers_count_crlf_line_endings() {
        let error = Config::parse(
            "[midi]\r\noutput = \"TourBox MIDI\"\r\nchannel = 1\r\n[map]\r\ntop = { cc = 128 }\r\n",
            Path::new("config.toml"),
        )
        .expect_err("範囲外の CC 番号はエラーにする必要があります。");

        assert!(
            matches!(error, ConfigError::Invalid { line: Some(5), .. }),
            "CRLF の改行でも 5 行目と報告する必要があります: {error}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn default_config_path_is_under_appdata_on_windows() {
        let appdata = std::env::var_os("APPDATA")
            .expect("Windows では環境変数 APPDATA が設定されている必要があります。");

        assert_eq!(
            default_config_path(),
            PathBuf::from(appdata)
                .join("tourbox-midi")
                .join("config.toml"),
            "%APPDATA%\\tourbox-midi\\config.toml にする必要があります。"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn default_config_path_is_under_application_support_on_macos() {
        let home = std::env::var_os("HOME")
            .expect("macOS では環境変数 HOME が設定されている必要があります。");

        assert_eq!(
            default_config_path(),
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("tourbox-midi")
                .join("config.toml"),
            "~/Library/Application Support/tourbox-midi/config.toml にする必要があります。"
        );
    }

    #[test]
    fn default_config_path_falls_back_to_current_directory() {
        for base_dir in [None, Some(OsString::new())] {
            assert_eq!(
                config_path_in(base_dir.clone()),
                PathBuf::from("config.toml"),
                "基点の環境変数が {base_dir:?} なら、カレントディレクトリの config.toml にする必要があります。"
            );
        }
    }
}
