//! 設定ファイルの検証と、検証済みの型への変換。
//!
//! toml のスパン付きの表を走査し、エラーには位置 (バイト範囲) を付ける。
//! メッセージではキーをドット区切りのパス (`map.with.side.knob.cc` など) で示す。

use std::collections::HashMap;
use std::ops::{Range, RangeInclusive};

use toml::de::{DeString, DeTable, DeValue};
use toml::Spanned;
use tourbox::protocol::{Axis, Button, Speed, Strength};
use tourbox::transport::{ConnectionConfig, TransportKind};

use super::schema::{
    ButtonEntry, ButtonKind, Config, ControlCc, HapticOverride, HapticSetting,
    HapticsControlConfig, HapticsSection, MapLayer, MapSection, MidiSection, RelativeEncoding,
    RotationEntry, RotationMode,
};

/// 検証エラー。`span` はエラーの位置 (バイト範囲) で、位置を特定できない場合は None。
#[derive(Debug)]
pub(super) struct Invalid {
    pub(super) span: Option<Range<usize>>,
    pub(super) message: String,
}

type Checked<T> = Result<T, Invalid>;

/// コントロール名とボタンの対応。
const BUTTONS: [(&str, Button); 14] = [
    ("tall", Button::Tall),
    ("side", Button::Side),
    ("top", Button::Top),
    ("short", Button::Short),
    ("scroll_press", Button::ScrollPress),
    ("up", Button::DpadUp),
    ("down", Button::DpadDown),
    ("left", Button::DpadLeft),
    ("right", Button::DpadRight),
    ("c1", Button::C1),
    ("c2", Button::C2),
    ("tour", Button::Tour),
    ("knob_press", Button::KnobPress),
    ("dial_press", Button::DialPress),
];

/// コントロール名と回転軸の対応。
const AXES: [(&str, Axis); 3] = [
    ("knob", Axis::Knob),
    ("scroll", Axis::Scroll),
    ("dial", Axis::Dial),
];

const TRANSPORTS: [(&str, TransportKind); 3] = [
    ("auto", TransportKind::Auto),
    ("usb", TransportKind::Usb),
    ("ble", TransportKind::Ble),
];

const STRENGTHS: [(&str, Strength); 3] = [
    ("off", Strength::Off),
    ("weak", Strength::Weak),
    ("strong", Strength::Strong),
];

const SPEEDS: [(&str, Speed); 3] = [
    ("fast", Speed::Fast),
    ("medium", Speed::Medium),
    ("slow", Speed::Slow),
];

/// 回転の `mode` の値。
#[derive(Clone, Copy)]
enum Mode {
    Absolute,
    Relative,
}

const MODES: [(&str, Mode); 2] = [("absolute", Mode::Absolute), ("relative", Mode::Relative)];

const ENCODINGS: [(&str, RelativeEncoding); 2] = [
    ("twos_complement", RelativeEncoding::TwosComplement),
    ("binary_offset", RelativeEncoding::BinaryOffset),
];

/// Note 番号、CC 番号、`initial` の範囲。
const DATA_BYTE: RangeInclusive<u8> = 0..=127;
/// 0 は Note Off と同じ意味になるので含めない。
const VELOCITY: RangeInclusive<u8> = 1..=127;
/// 設定ファイル上のチャンネルの範囲。
const CHANNEL: RangeInclusive<u8> = 1..=16;
const ABSOLUTE_STEP: RangeInclusive<u8> = 1..=127;
/// 2 の補数で +64 と -64 が同じ値になり、binary_offset では 7 ビットに収まらなくなるので 63 まで。
const RELATIVE_STEP: RangeInclusive<u8> = 1..=63;

const DEFAULT_VELOCITY: u8 = 127;
const DEFAULT_STEP: u8 = 1;
const DEFAULT_INITIAL: u8 = 0;

/// 設定ファイルの内容を検証し、検証済みの設定に変換する。
pub(super) fn validate(text: &str) -> Checked<Config> {
    let document = DeTable::parse(text).map_err(|error| Invalid {
        span: error.span(),
        message: format!("TOML の構文が正しくありません ({})。", error.message()),
    })?;
    let root = Table {
        path: String::new(),
        span: document.span(),
        entries: document.get_ref(),
    };
    root.expect_keys(&["device", "midi", "haptics", "map"])?;
    let (midi, map) = match (root.field("midi"), root.field("map")) {
        (Some(midi), Some(map)) => (midi, map),
        (midi, map) => {
            let missing: Vec<&str> = [("[midi]", midi.is_none()), ("[map]", map.is_none())]
                .into_iter()
                .filter_map(|(name, absent)| absent.then_some(name))
                .collect();
            return Err(Invalid {
                span: None,
                message: format!("必須のセクション {} がありません。", missing.join(" と ")),
            });
        }
    };
    Ok(Config {
        device: device_section(root.field("device"))?,
        midi: midi_section(&midi)?,
        haptics: haptics_section(root.field("haptics"))?,
        map: map_section(&map)?,
    })
}

fn device_section(field: Option<Field>) -> Checked<ConnectionConfig> {
    let Some(field) = field else {
        return Ok(ConnectionConfig {
            transport: TransportKind::Auto,
            usb_port: None,
        });
    };
    let table = field.table()?;
    table.expect_keys(&["transport", "usb_port"])?;
    Ok(ConnectionConfig {
        transport: table
            .optional("transport", |field| field.choice(&TRANSPORTS))?
            .unwrap_or(TransportKind::Auto),
        usb_port: table.optional("usb_port", |field| field.string())?,
    })
}

fn midi_section(field: &Field) -> Checked<MidiSection> {
    let table = field.table()?;
    table.expect_keys(&["output", "input", "channel"])?;
    Ok(MidiSection {
        output: table.required("output")?.string()?,
        input: table.optional("input", |field| field.string())?,
        channel: channel(&table.required("channel")?)?,
    })
}

fn haptics_section(field: Option<Field>) -> Checked<HapticsSection> {
    let Some(field) = field else {
        return Ok(HapticsSection::default());
    };
    let table = field.table()?;
    table.expect_keys(&axis_names_and(&["with", "control"]))?;

    let default = HapticSetting::default();
    let mut axes = HashMap::new();
    for (name, axis) in AXES {
        if let Some(field) = table.field(name) {
            let (strength, speed) = haptic_values(&field)?;
            let setting = HapticSetting {
                strength: strength.unwrap_or(default.strength),
                speed: speed.unwrap_or(default.speed),
            };
            axes.insert(axis, setting);
        }
    }

    let mut with = HashMap::new();
    for (button, layer) in modifier_layers(&table)? {
        layer.expect_keys(&axis_names_and(&[]))?;
        for (name, axis) in AXES {
            if let Some(field) = layer.field(name) {
                let (strength, speed) = haptic_values(&field)?;
                with.insert((button, axis), HapticOverride { strength, speed });
            }
        }
    }

    let control = table
        .optional("control", |field| haptics_control(&field))?
        .unwrap_or_default();
    Ok(HapticsSection {
        axes,
        with,
        control,
    })
}

/// `{ strength, speed }` の項目の値。省略した値は None。
fn haptic_values(field: &Field) -> Checked<(Option<Strength>, Option<Speed>)> {
    let table = field.table()?;
    table.expect_keys(&["strength", "speed"])?;
    Ok((
        table.optional("strength", |field| field.choice(&STRENGTHS))?,
        table.optional("speed", |field| field.choice(&SPEEDS))?,
    ))
}

fn haptics_control(field: &Field) -> Checked<HapticsControlConfig> {
    let table = field.table()?;
    table.expect_keys(&axis_names_and(&["channel", "master", "with"]))?;
    let mut used = UsedCcs::default();

    let channel = table.optional("channel", |field| channel(&field))?;
    let master = table.optional("master", |field| {
        let master = field.table()?;
        master.expect_keys(&["cc"])?;
        used.read(&master.required("cc")?)
    })?;

    let mut axes = HashMap::new();
    for (name, axis) in AXES {
        if let Some(field) = table.field(name) {
            axes.insert(axis, control_cc(&field, &mut used)?);
        }
    }

    let mut with = HashMap::new();
    for (button, layer) in modifier_layers(&table)? {
        layer.expect_keys(&axis_names_and(&[]))?;
        for (name, axis) in AXES {
            if let Some(field) = layer.field(name) {
                with.insert((button, axis), control_cc(&field, &mut used)?);
            }
        }
    }

    used.ensure_unique()?;
    Ok(HapticsControlConfig {
        channel,
        master,
        axes,
        with,
    })
}

/// `{ cc, speed_cc }` の項目の値。
fn control_cc(field: &Field, used: &mut UsedCcs) -> Checked<ControlCc> {
    let table = field.table()?;
    table.expect_keys(&["cc", "speed_cc"])?;
    Ok(ControlCc {
        cc: table.optional("cc", |field| used.read(&field))?,
        speed_cc: table.optional("speed_cc", |field| used.read(&field))?,
    })
}

/// `[haptics.control]` の中で使った CC 番号と、その位置とパス。
#[derive(Default)]
struct UsedCcs(Vec<(u8, Range<usize>, String)>);

impl UsedCcs {
    /// CC 番号を読み、使ったものとして記録する。
    fn read(&mut self, field: &Field) -> Checked<u8> {
        let cc = field.integer(DATA_BYTE)?;
        self.0.push((cc, field.span(), field.path.clone()));
        Ok(cc)
    }

    /// 同じ CC 番号が 2 回以上使われていれば、ファイル上で後に書かれた方をエラーにする。
    fn ensure_unique(mut self) -> Checked<()> {
        // 表の走査はキー名の順なので、ファイル上の順に並べ直す
        self.0.sort_by_key(|(_, span, _)| span.start);
        for (index, (cc, span, path)) in self.0.iter().enumerate() {
            if let Some((_, _, first)) = self.0[..index].iter().find(|(other, _, _)| other == cc) {
                return Err(Invalid {
                    span: Some(span.clone()),
                    message: format!(
                        "`{path}` の CC {cc} は `{first}` と重複しています。[haptics.control] では同じ CC を複数の項目に割り当てられません。"
                    ),
                });
            }
        }
        Ok(())
    }
}

fn map_section(field: &Field) -> Checked<MapSection> {
    let table = field.table()?;
    let base = map_layer(
        table.fields().filter(|(key, _)| key.get_ref() != "with"),
        None,
    )?;
    let mut with = HashMap::new();
    for (button, layer) in modifier_layers(&table)? {
        with.insert(button, map_layer(layer.fields(), Some(button))?);
    }
    Ok(MapSection { base, with })
}

/// レイヤの項目を読む。`modifier` はレイヤの修飾ボタンで、基本レイヤは None。
fn map_layer<'a, 'i: 'a>(
    entries: impl Iterator<Item = (&'a Spanned<DeString<'i>>, Field<'a, 'i>)>,
    modifier: Option<Button>,
) -> Checked<MapLayer> {
    let mut layer = MapLayer::default();
    for (key, field) in entries {
        let name = key.get_ref().as_ref();
        if let Some(button) = named(&BUTTONS, name) {
            if Some(button) == modifier {
                return Err(invalid_key(
                    key,
                    format!("修飾ボタン `{name}` 自身を [map.with.{name}] に割り当てることはできません。"),
                ));
            }
            layer.buttons.insert(button, button_entry(&field)?);
        } else if let Some(axis) = named(&AXES, name) {
            layer.rotations.insert(axis, rotation_entry(&field)?);
        } else {
            return Err(invalid_key(
                key,
                format!("`{name}` は存在しないコントロール名です。"),
            ));
        }
    }
    Ok(layer)
}

fn button_entry(field: &Field) -> Checked<ButtonEntry> {
    let table = field.table()?;
    table.expect_keys(&["note", "cc", "velocity", "channel"])?;
    let kind = match (table.field("note"), table.field("cc")) {
        (Some(note), None) => ButtonKind::Note {
            note: note.integer(DATA_BYTE)?,
            velocity: table
                .optional("velocity", |field| field.integer(VELOCITY))?
                .unwrap_or(DEFAULT_VELOCITY),
        },
        (None, Some(cc)) => {
            if let Some(velocity) = table.field("velocity") {
                return Err(velocity.invalid(format!(
                    "`{}` は note を指定した項目にだけ指定できます。",
                    velocity.path
                )));
            }
            ButtonKind::Cc {
                cc: cc.integer(DATA_BYTE)?,
            }
        }
        _ => {
            return Err(field.invalid(format!(
                "`{}` には note と cc のどちらか一方を指定してください。",
                field.path
            )));
        }
    };
    Ok(ButtonEntry {
        channel: table.optional("channel", |field| channel(&field))?,
        kind,
    })
}

fn rotation_entry(field: &Field) -> Checked<RotationEntry> {
    let table = field.table()?;
    table.expect_keys(&[
        "cc", "mode", "encoding", "step", "initial", "invert", "channel",
    ])?;
    let cc = table.required("cc")?.integer(DATA_BYTE)?;
    let mode = match table
        .optional("mode", |field| field.choice(&MODES))?
        .unwrap_or(Mode::Absolute)
    {
        Mode::Absolute => {
            reject_outside_mode(&table, "encoding", "relative")?;
            RotationMode::Absolute {
                step: table
                    .optional("step", |field| field.integer(ABSOLUTE_STEP))?
                    .unwrap_or(DEFAULT_STEP),
                initial: table
                    .optional("initial", |field| field.integer(DATA_BYTE))?
                    .unwrap_or(DEFAULT_INITIAL),
            }
        }
        Mode::Relative => {
            reject_outside_mode(&table, "initial", "absolute")?;
            RotationMode::Relative {
                step: table
                    .optional("step", |field| field.integer(RELATIVE_STEP))?
                    .unwrap_or(DEFAULT_STEP),
                encoding: table
                    .optional("encoding", |field| field.choice(&ENCODINGS))?
                    .unwrap_or(RelativeEncoding::TwosComplement),
            }
        }
    };
    Ok(RotationEntry {
        channel: table.optional("channel", |field| channel(&field))?,
        cc,
        invert: table
            .optional("invert", |field| field.boolean())?
            .unwrap_or(false),
        mode,
    })
}

/// `mode` が `mode_name` の項目にだけ指定できるキー `key` があればエラーにする。
fn reject_outside_mode(table: &Table, key: &str, mode_name: &str) -> Checked<()> {
    match table.field(key) {
        Some(field) => Err(field.invalid(format!(
            "`{}` は mode = \"{mode_name}\" の項目にだけ指定できます。",
            field.path
        ))),
        None => Ok(()),
    }
}

/// 設定ファイル上のチャンネル (1〜16) を 0 起点にして読む。
fn channel(field: &Field) -> Checked<u8> {
    Ok(field.integer(CHANNEL)? - 1)
}

/// 表の `with` にある修飾ボタンごとのレイヤの表。
fn modifier_layers<'a, 'i>(parent: &Table<'a, 'i>) -> Checked<Vec<(Button, Table<'a, 'i>)>> {
    let Some(with) = parent.field("with") else {
        return Ok(Vec::new());
    };
    let with = with.table()?;
    with.fields()
        .map(|(key, field)| Ok((modifier_button(key)?, field.table()?)))
        .collect()
}

/// `with` のキーを修飾ボタンとして読む。
fn modifier_button(key: &Spanned<DeString>) -> Checked<Button> {
    let name = key.get_ref().as_ref();
    if let Some(button) = named(&BUTTONS, name) {
        return Ok(button);
    }
    let message = if named(&AXES, name).is_some() {
        format!("`{name}` は回転軸なので修飾ボタンにできません。")
    } else {
        format!("`{name}` は存在しないボタン名です。")
    };
    Err(invalid_key(key, message))
}

/// 名前の表 `names` から `name` に対応する値を引く。
fn named<T: Copy>(names: &[(&str, T)], name: &str) -> Option<T> {
    names
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, value)| *value)
}

/// 軸名に `extra` を加えたキーの一覧。
fn axis_names_and<'k>(extra: &[&'k str]) -> Vec<&'k str> {
    AXES.iter()
        .map(|(name, _)| *name)
        .chain(extra.iter().copied())
        .collect()
}

fn invalid_key(key: &Spanned<DeString>, message: String) -> Invalid {
    Invalid {
        span: Some(key.span()),
        message,
    }
}

/// 検証中の表。`path` はメッセージに使うドット区切りのキーで、最上位は空文字列。
struct Table<'a, 'i> {
    path: String,
    span: Range<usize>,
    entries: &'a DeTable<'i>,
}

impl<'a, 'i> Table<'a, 'i> {
    fn field(&self, key: &str) -> Option<Field<'a, 'i>> {
        self.entries.get(key).map(|value| Field {
            path: self.child_path(key),
            value,
        })
    }

    /// 必須のキー。なければ表の位置でエラーにする。
    fn required(&self, key: &str) -> Checked<Field<'a, 'i>> {
        self.field(key).ok_or_else(|| Invalid {
            span: Some(self.span.clone()),
            message: format!("`{}` に `{key}` がありません。", self.path),
        })
    }

    /// 省略できるキー。あれば `read` で読む。
    fn optional<T>(
        &self,
        key: &str,
        read: impl FnOnce(Field<'a, 'i>) -> Checked<T>,
    ) -> Checked<Option<T>> {
        self.field(key).map(read).transpose()
    }

    /// すべてのキーと値。
    fn fields(&self) -> impl Iterator<Item = (&'a Spanned<DeString<'i>>, Field<'a, 'i>)> + '_ {
        self.entries.iter().map(|(key, value)| {
            let field = Field {
                path: self.child_path(key.get_ref()),
                value,
            };
            (key, field)
        })
    }

    /// `allowed` にないキーをエラーにする。
    fn expect_keys(&self, allowed: &[&str]) -> Checked<()> {
        match self
            .entries
            .keys()
            .find(|key| !allowed.contains(&key.get_ref().as_ref()))
        {
            Some(key) => Err(invalid_key(
                key,
                format!(
                    "`{}` は指定できないキーです。指定できるのは {} です。",
                    self.child_path(key.get_ref()),
                    allowed.join("、")
                ),
            )),
            None => Ok(()),
        }
    }

    fn child_path(&self, key: &str) -> String {
        if self.path.is_empty() {
            key.to_owned()
        } else {
            format!("{}.{key}", self.path)
        }
    }
}

/// 検証中の値。`path` はメッセージに使うドット区切りのキー。
struct Field<'a, 'i> {
    path: String,
    value: &'a Spanned<DeValue<'i>>,
}

impl<'a, 'i> Field<'a, 'i> {
    fn span(&self) -> Range<usize> {
        self.value.span()
    }

    fn invalid(&self, message: String) -> Invalid {
        Invalid {
            span: Some(self.span()),
            message,
        }
    }

    fn table(&self) -> Checked<Table<'a, 'i>> {
        match self.value.get_ref() {
            DeValue::Table(entries) => Ok(Table {
                path: self.path.clone(),
                span: self.span(),
                entries,
            }),
            _ => Err(self.invalid(format!("`{}` にはテーブルを指定してください。", self.path))),
        }
    }

    /// `range` の範囲の整数。
    fn integer(&self, range: RangeInclusive<u8>) -> Checked<u8> {
        let DeValue::Integer(integer) = self.value.get_ref() else {
            return Err(self.invalid(format!("`{}` には整数を指定してください。", self.path)));
        };
        i64::from_str_radix(integer.as_str(), integer.radix())
            .ok()
            .and_then(|value| u8::try_from(value).ok())
            .filter(|value| range.contains(value))
            .ok_or_else(|| {
                self.invalid(format!(
                    "`{}` は {}〜{} の範囲で指定してください (指定値: {integer})。",
                    self.path,
                    range.start(),
                    range.end()
                ))
            })
    }

    fn string(&self) -> Checked<String> {
        match self.value.get_ref() {
            DeValue::String(value) => Ok(value.to_string()),
            _ => Err(self.invalid(format!("`{}` には文字列を指定してください。", self.path))),
        }
    }

    fn boolean(&self) -> Checked<bool> {
        match self.value.get_ref() {
            DeValue::Boolean(value) => Ok(*value),
            _ => Err(self.invalid(format!(
                "`{}` には true か false を指定してください。",
                self.path
            ))),
        }
    }

    /// `options` の名前のどれかに一致する文字列を、対応する値にする。
    fn choice<T: Copy>(&self, options: &[(&str, T)]) -> Checked<T> {
        self.value
            .get_ref()
            .as_str()
            .and_then(|name| named(options, name))
            .ok_or_else(|| {
                let names: Vec<&str> = options.iter().map(|(name, _)| *name).collect();
                self.invalid(format!(
                    "`{}` には {} のいずれかを指定してください。",
                    self.path,
                    names.join("、")
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tourbox::protocol::{Axis, Button};
    use tourbox::transport::{ConnectionConfig, TransportKind};

    use crate::config::{
        ButtonEntry, ButtonKind, Config, ConfigError, HapticsSection, RelativeEncoding,
        RotationEntry, RotationMode,
    };

    /// `[midi]` と `[map]` の見出しの 4 行の後に `lines` を続けた設定。`lines` の 1 行目がファイルの 5 行目になる。
    fn with_map(lines: &str) -> String {
        format!("[midi]\noutput = \"TourBox MIDI\"\nchannel = 1\n[map]\n{lines}\n")
    }

    fn parse(text: &str) -> Result<Config, ConfigError> {
        Config::parse(text, Path::new("config.toml"))
    }

    fn parse_ok(text: &str) -> Config {
        parse(text).unwrap_or_else(|error| panic!("検証に通る必要があります: {error}"))
    }

    /// 検証エラーの行番号とメッセージ。
    fn rejected(text: &str) -> (Option<usize>, String) {
        match parse(text) {
            Err(ConfigError::Invalid { line, message, .. }) => (line, message),
            other => panic!("検証エラーになる必要があります: {other:?}"),
        }
    }

    /// (説明、`with_map` に渡す行、行番号、メッセージに含まれる語) の各行が検証エラーになることを確認する。
    fn assert_rejected_at(cases: &[(&str, &str, usize, &str)]) {
        for &(case, lines, line, fragment) in cases {
            let (actual, message) = rejected(&with_map(lines));
            assert_eq!(
                actual,
                Some(line),
                "{case}: {line} 行目と報告する必要があります (メッセージ: {message})"
            );
            assert!(
                message.contains(fragment),
                "{case}: メッセージに「{fragment}」を含む必要があります: {message}"
            );
        }
    }

    #[test]
    fn missing_required_sections_are_reported_without_line() {
        let midi = "[midi]\noutput = \"TourBox MIDI\"\nchannel = 1\n";
        for (case, text, expected) in [
            (
                "空ファイル",
                "",
                "必須のセクション [midi] と [map] がありません。",
            ),
            (
                "コメントだけ",
                "# 設定\n",
                "必須のセクション [midi] と [map] がありません。",
            ),
            ("[map] なし", midi, "必須のセクション [map] がありません。"),
            (
                "[midi] なし",
                "[map]\n",
                "必須のセクション [midi] がありません。",
            ),
        ] {
            assert_eq!(
                rejected(text),
                (None, expected.to_owned()),
                "{case}: 必須セクションの欠落を行番号なしで報告する必要があります。"
            );
        }
    }

    #[test]
    fn syntax_error_is_reported_with_line() {
        let (line, message) = rejected(&with_map("tall = { note = 60"));

        assert_eq!(line, Some(5), "構文エラーの行を報告する必要があります。");
        assert!(
            message.starts_with("TOML の構文が正しくありません"),
            "構文エラーであることを示す必要があります: {message}"
        );
    }

    #[test]
    fn out_of_range_values_are_reported_with_line() {
        assert_rejected_at(&[
            (
                "ボタンの cc",
                "top = { cc = 128 }",
                5,
                "`map.top.cc` は 0〜127 の範囲",
            ),
            (
                "回転の cc",
                "knob = { cc = 128 }",
                5,
                "`map.knob.cc` は 0〜127",
            ),
            (
                "負の cc",
                "knob = { cc = -1 }",
                5,
                "`map.knob.cc` は 0〜127",
            ),
            (
                "桁あふれの cc",
                "knob = { cc = 99999999999999999999 }",
                5,
                "`map.knob.cc`",
            ),
            (
                "note",
                "\ntall = { note = 128 }",
                6,
                "`map.tall.note` は 0〜127",
            ),
            (
                "velocity の 0",
                "tall = { note = 60, velocity = 0 }",
                5,
                "`map.tall.velocity` は 1〜127",
            ),
            (
                "velocity の 128",
                "tall = { note = 60, velocity = 128 }",
                5,
                "`map.tall.velocity` は 1〜127",
            ),
            (
                "項目の channel の 0",
                "tall = { note = 60, channel = 0 }",
                5,
                "`map.tall.channel` は 1〜16",
            ),
            (
                "項目の channel の 17",
                "knob = { cc = 1, channel = 17 }",
                5,
                "`map.knob.channel` は 1〜16",
            ),
            (
                "initial",
                "knob = { cc = 1, initial = 128 }",
                5,
                "`map.knob.initial` は 0〜127",
            ),
            (
                "レイヤの cc",
                "[map.with.side]\nknob = { cc = 128 }",
                6,
                "`map.with.side.knob.cc` は 0〜127",
            ),
            (
                "[haptics.control] の channel",
                "[haptics.control]\nchannel = 17",
                6,
                "`haptics.control.channel` は 1〜16",
            ),
            (
                "[haptics.control] の master",
                "[haptics.control]\nmaster = { cc = 128 }",
                6,
                "`haptics.control.master.cc` は 0〜127",
            ),
            (
                "[haptics.control] の speed_cc",
                "[haptics.control]\nknob = { speed_cc = 128 }",
                6,
                "`haptics.control.knob.speed_cc` は 0〜127",
            ),
        ]);
    }

    #[test]
    fn out_of_range_midi_channel_is_reported_with_line() {
        for value in [0, 17] {
            let (line, message) = rejected(&format!(
                "[midi]\noutput = \"TourBox MIDI\"\nchannel = {value}\n[map]\n"
            ));

            assert_eq!(
                line,
                Some(3),
                "channel = {value} の行を報告する必要があります。"
            );
            assert!(
                message.contains("`midi.channel` は 1〜16 の範囲"),
                "channel = {value} の範囲を示す必要があります: {message}"
            );
        }
    }

    #[test]
    fn relative_step_above_63_is_rejected_with_line() {
        assert_rejected_at(&[
            (
                "相対 CC の step 64",
                "scroll = { cc = 2, mode = \"relative\", step = 64 }",
                5,
                "`map.scroll.step` は 1〜63 の範囲",
            ),
            (
                "相対 CC の step 0",
                "scroll = { cc = 2, mode = \"relative\", step = 0 }",
                5,
                "`map.scroll.step` は 1〜63 の範囲",
            ),
        ]);

        let config = parse_ok(&with_map(
            "scroll = { cc = 2, mode = \"relative\", step = 63 }",
        ));
        assert_eq!(
            config.map.base.rotations[&Axis::Scroll].mode,
            RotationMode::Relative {
                step: 63,
                encoding: RelativeEncoding::TwosComplement
            },
            "相対 CC の step 63 は受け付ける必要があります。"
        );
    }

    #[test]
    fn absolute_step_above_127_is_rejected_with_line() {
        assert_rejected_at(&[
            (
                "絶対 CC の step 128",
                "dial = { cc = 3, step = 128 }",
                5,
                "`map.dial.step` は 1〜127 の範囲",
            ),
            (
                "絶対 CC の step 0",
                "dial = { cc = 3, step = 0 }",
                5,
                "`map.dial.step` は 1〜127 の範囲",
            ),
        ]);

        let config = parse_ok(&with_map("dial = { cc = 3, step = 127 }"));
        assert_eq!(
            config.map.base.rotations[&Axis::Dial].mode,
            RotationMode::Absolute {
                step: 127,
                initial: 0
            },
            "絶対 CC の step 127 は受け付ける必要があります。"
        );
    }

    #[test]
    fn wrong_value_types_are_reported_with_line() {
        assert_rejected_at(&[
            (
                "cc に文字列",
                "top = { cc = \"20\" }",
                5,
                "`map.top.cc` には整数を指定してください",
            ),
            (
                "cc に小数",
                "top = { cc = 20.0 }",
                5,
                "`map.top.cc` には整数を指定してください",
            ),
            (
                "項目に整数",
                "tall = 60",
                5,
                "`map.tall` にはテーブルを指定してください",
            ),
            (
                "invert に整数",
                "dial = { cc = 3, invert = 1 }",
                5,
                "`map.dial.invert` には true か false を指定してください",
            ),
            (
                "mode の値",
                "scroll = { cc = 2, mode = \"rel\" }",
                5,
                "`map.scroll.mode` には absolute、relative のいずれか",
            ),
            (
                "encoding の値",
                "scroll = { cc = 2, mode = \"relative\", encoding = \"offset\" }",
                5,
                "`map.scroll.encoding` には twos_complement、binary_offset のいずれか",
            ),
            (
                "strength の値",
                "[haptics]\nknob = { strength = \"loud\" }",
                6,
                "`haptics.knob.strength` には off、weak、strong のいずれか",
            ),
            (
                "speed の値",
                "[haptics.with.side]\nknob = { speed = \"quick\" }",
                6,
                "`haptics.with.side.knob.speed` には fast、medium、slow のいずれか",
            ),
            (
                "transport の値",
                "[device]\ntransport = \"serial\"",
                6,
                "`device.transport` には auto、usb、ble のいずれか",
            ),
            (
                "usb_port に整数",
                "[device]\nusb_port = 3",
                6,
                "`device.usb_port` には文字列を指定してください",
            ),
        ]);
        let (line, message) =
            rejected("haptics = 1\n[midi]\noutput = \"TourBox MIDI\"\nchannel = 1\n[map]\n");
        assert_eq!(
            line,
            Some(1),
            "テーブルでないセクションの行を報告する必要があります。"
        );
        assert!(
            message.contains("`haptics` にはテーブルを指定してください"),
            "テーブルが必要なことを示す必要があります: {message}"
        );
    }

    #[test]
    fn unknown_keys_are_reported_with_line() {
        assert_rejected_at(&[
            (
                "最上位",
                "[maps]",
                5,
                "`maps` は指定できないキーです。指定できるのは device、midi、haptics、map です。",
            ),
            (
                "[device]",
                "[device]\nport = \"COM3\"",
                6,
                "`device.port` は指定できないキー",
            ),
            (
                "[haptics]",
                "[haptics]\nmaster = { cc = 99 }",
                6,
                "`haptics.master` は指定できないキー",
            ),
            (
                "[haptics] の軸の項目",
                "[haptics]\nknob = { strength = \"weak\", cc = 1 }",
                6,
                "`haptics.knob.cc` は指定できないキー",
            ),
            (
                "master",
                "[haptics.control]\nmaster = { cc = 99, speed_cc = 98 }",
                6,
                "`haptics.control.master.speed_cc` は指定できないキー",
            ),
            (
                "[haptics.control] の軸の項目",
                "[haptics.control]\nknob = { cc = 100, channel = 1 }",
                6,
                "`haptics.control.knob.channel` は指定できないキー",
            ),
        ]);

        let (line, message) =
            rejected("[midi]\noutput = \"TourBox MIDI\"\nchannel = 1\nchanel = 2\n[map]\n");
        assert_eq!(
            line,
            Some(4),
            "[midi] の未知のキーの行を報告する必要があります。"
        );
        assert!(
            message.contains("`midi.chanel` は指定できないキー"),
            "未知のキーを示す必要があります: {message}"
        );
    }

    #[test]
    fn keys_of_the_other_control_kind_are_rejected_with_line() {
        assert_rejected_at(&[
            ("ボタンに mode", "tall = { note = 60, mode = \"relative\" }", 5, "`map.tall.mode` は指定できないキーです。指定できるのは note、cc、velocity、channel です。"),
            ("ボタンに step", "tall = { note = 60, step = 2 }", 5, "`map.tall.step` は指定できないキー"),
            ("ボタンに initial", "tall = { note = 60, initial = 64 }", 5, "`map.tall.initial` は指定できないキー"),
            ("ボタンに invert", "tall = { note = 60, invert = true }", 5, "`map.tall.invert` は指定できないキー"),
            ("ボタンに encoding", "tall = { note = 60, encoding = \"binary_offset\" }", 5, "`map.tall.encoding` は指定できないキー"),
            ("回転に note", "knob = { cc = 1, note = 60 }", 5, "`map.knob.note` は指定できないキー"),
            ("回転に velocity", "knob = { cc = 1, velocity = 100 }", 5, "`map.knob.velocity` は指定できないキー"),
        ]);
    }

    #[test]
    fn unknown_control_names_are_reported_with_line() {
        assert_rejected_at(&[
            ("[map] のキー", "shift = { note = 60 }", 5, "`shift` は存在しないコントロール名です。"),
            ("レイヤのキー", "[map.with.side]\nshift = { note = 60 }", 6, "`shift` は存在しないコントロール名です。"),
            ("[map.with] の名前", "[map.with.shift]\ntall = { note = 60 }", 5, "`shift` は存在しないボタン名です。"),
            ("[haptics] の軸名", "[haptics]\nwheel = { strength = \"weak\" }", 6, "`haptics.wheel` は指定できないキーです。指定できるのは knob、scroll、dial、with、control です。"),
            ("[haptics] のボタン名", "[haptics]\ntall = { strength = \"weak\" }", 6, "`haptics.tall` は指定できないキー"),
            ("[haptics.with] の名前", "[haptics.with.shift]\nknob = { strength = \"weak\" }", 5, "`shift` は存在しないボタン名です。"),
            ("[haptics.with] の軸名", "[haptics.with.side]\nwheel = { strength = \"weak\" }", 6, "`haptics.with.side.wheel` は指定できないキー"),
            ("[haptics.control] の軸名", "[haptics.control]\nwheel = { cc = 100 }", 6, "`haptics.control.wheel` は指定できないキー"),
            ("[haptics.control.with] の名前", "[haptics.control.with.shift]\nknob = { cc = 110 }", 5, "`shift` は存在しないボタン名です。"),
            ("[haptics.control.with] の軸名", "[haptics.control.with.side]\nwheel = { cc = 110 }", 6, "`haptics.control.with.side.wheel` は指定できないキー"),
        ]);
    }

    #[test]
    fn rotation_axis_cannot_be_modifier() {
        assert_rejected_at(&[
            (
                "[map.with.knob]",
                "[map.with.knob]\ntall = { note = 60 }",
                5,
                "`knob` は回転軸なので修飾ボタンにできません。",
            ),
            (
                "[haptics.with.dial]",
                "[haptics.with.dial]\nknob = { strength = \"weak\" }",
                5,
                "`dial` は回転軸なので修飾ボタンにできません。",
            ),
            (
                "[haptics.control.with.scroll]",
                "[haptics.control.with.scroll]\nknob = { cc = 110 }",
                5,
                "`scroll` は回転軸なので修飾ボタンにできません。",
            ),
        ]);
    }

    #[test]
    fn modifier_cannot_be_assigned_in_its_own_layer() {
        assert_rejected_at(&[(
            "Side のレイヤに Side",
            "[map.with.side]\ntop = { note = 70 }\nside = { note = 71 }",
            7,
            "修飾ボタン `side` 自身を [map.with.side] に割り当てることはできません。",
        )]);
    }

    #[test]
    fn button_needs_exactly_one_of_note_and_cc() {
        let message = "`map.tall` には note と cc のどちらか一方を指定してください。";
        assert_rejected_at(&[
            (
                "note と cc の両方",
                "tall = { note = 60, cc = 20 }",
                5,
                message,
            ),
            ("どちらもない", "tall = { velocity = 100 }", 5, message),
            ("空の項目", "\n\ntall = {}", 7, message),
        ]);
    }

    #[test]
    fn velocity_is_only_for_note_buttons() {
        assert_rejected_at(&[(
            "CC のボタンに velocity",
            "top = { cc = 20, velocity = 100 }",
            5,
            "`map.top.velocity` は note を指定した項目にだけ指定できます。",
        )]);
    }

    #[test]
    fn rotation_needs_cc() {
        assert_rejected_at(&[(
            "cc のない回転",
            "knob = { mode = \"relative\" }",
            5,
            "`map.knob` に `cc` がありません。",
        )]);
    }

    #[test]
    fn mode_specific_keys_are_rejected_for_the_other_mode() {
        assert_rejected_at(&[
            (
                "既定の絶対 CC に encoding",
                "knob = { cc = 1, encoding = \"binary_offset\" }",
                5,
                "`map.knob.encoding` は mode = \"relative\" の項目にだけ指定できます。",
            ),
            (
                "明示した絶対 CC に encoding",
                "knob = { cc = 1, mode = \"absolute\", encoding = \"twos_complement\" }",
                5,
                "`map.knob.encoding` は mode = \"relative\" の項目にだけ指定できます。",
            ),
            (
                "相対 CC に initial",
                "scroll = { cc = 2, mode = \"relative\", initial = 64 }",
                5,
                "`map.scroll.initial` は mode = \"absolute\" の項目にだけ指定できます。",
            ),
        ]);
    }

    #[test]
    fn duplicate_control_cc_is_reported_with_line() {
        assert_rejected_at(&[
            ("チャンネル指定ありの軸どうし", "[haptics.control]\nchannel = 16\nknob = { cc = 100 }\nscroll = { cc = 100 }", 8, "`haptics.control.scroll.cc` の CC 100 は `haptics.control.knob.cc` と重複しています。"),
            ("チャンネル省略時の軸どうし", "[haptics.control]\nknob = { cc = 100 }\nscroll = { cc = 100 }", 7, "`haptics.control.scroll.cc` の CC 100 は `haptics.control.knob.cc` と重複しています。"),
            ("master と speed_cc", "[haptics.control]\nmaster = { cc = 99 }\nknob = { cc = 100, speed_cc = 99 }", 7, "`haptics.control.knob.speed_cc` の CC 99 は `haptics.control.master.cc` と重複しています。"),
            ("同じ項目の cc と speed_cc", "[haptics.control]\nknob = { cc = 100, speed_cc = 100 }", 6, "`haptics.control.knob.speed_cc` の CC 100 は `haptics.control.knob.cc` と重複しています。"),
            ("軸と修飾の個別制御", "[haptics.control]\nknob = { cc = 100 }\n[haptics.control.with.side]\nknob = { cc = 100 }", 8, "`haptics.control.with.side.knob.cc` の CC 100 は `haptics.control.knob.cc` と重複しています。"),
        ]);
    }

    #[test]
    fn missing_required_keys_are_reported_with_line() {
        for (case, text, line, expected) in [
            (
                "midi.channel",
                "[midi]\noutput = \"TourBox MIDI\"\n[map]\n",
                1,
                "`midi` に `channel` がありません。",
            ),
            (
                "midi.output",
                "[map]\n[midi]\nchannel = 1\n",
                2,
                "`midi` に `output` がありません。",
            ),
        ] {
            assert_eq!(
                rejected(text),
                (Some(line), expected.to_owned()),
                "{case} の欠落を [midi] の行で報告する必要があります。"
            );
        }
        assert_rejected_at(&[(
            "master の cc",
            "[haptics.control]\nmaster = {}",
            6,
            "`haptics.control.master` に `cc` がありません。",
        )]);
    }

    #[test]
    fn omitted_values_use_defaults() {
        let config = parse_ok(&with_map(
            "tall = { note = 60 }\ntop = { cc = 20 }\nknob = { cc = 1 }\nscroll = { cc = 2, mode = \"relative\" }",
        ));

        assert_eq!(
            config.device,
            ConnectionConfig {
                transport: TransportKind::Auto,
                usb_port: None
            },
            "[device] の省略時は auto でポートの明示指定なしにする必要があります。"
        );
        assert_eq!(
            config.midi.input, None,
            "input は省略できる必要があります。"
        );
        assert_eq!(
            config.haptics,
            HapticsSection::default(),
            "[haptics] の省略時は軸の値も上書きも制御もない必要があります。"
        );
        assert_eq!(
            config.map.base.buttons[&Button::Tall],
            ButtonEntry {
                channel: None,
                kind: ButtonKind::Note {
                    note: 60,
                    velocity: 127
                }
            },
            "velocity の既定は 127 である必要があります。"
        );
        assert_eq!(
            config.map.base.buttons[&Button::Top],
            ButtonEntry {
                channel: None,
                kind: ButtonKind::Cc { cc: 20 }
            }
        );
        assert_eq!(
            config.map.base.rotations[&Axis::Knob],
            RotationEntry {
                channel: None,
                cc: 1,
                invert: false,
                mode: RotationMode::Absolute {
                    step: 1,
                    initial: 0
                }
            },
            "回転の既定は絶対 CC、step 1、initial 0、反転なしである必要があります。"
        );
        assert_eq!(
            config.map.base.rotations[&Axis::Scroll].mode,
            RotationMode::Relative {
                step: 1,
                encoding: RelativeEncoding::TwosComplement
            },
            "相対 CC の既定は step 1、twos_complement である必要があります。"
        );
    }

    #[test]
    fn explicit_values_are_kept_with_zero_based_channels() {
        let config = parse_ok(concat!(
            "[midi]\noutput = \"TourBox MIDI\"\nchannel = 16\n[map]\n",
            "tall = { note = 60, velocity = 100, channel = 1 }\n",
            "knob = { cc = 1, step = 5, initial = 64, invert = true, channel = 10 }\n",
            "scroll = { cc = 2, mode = \"relative\", step = 3, encoding = \"binary_offset\" }\n",
        ));

        assert_eq!(
            config.midi.channel, 15,
            "midi.channel は 0 起点で持つ必要があります。"
        );
        assert_eq!(
            config.map.base.buttons[&Button::Tall],
            ButtonEntry {
                channel: Some(0),
                kind: ButtonKind::Note {
                    note: 60,
                    velocity: 100
                }
            },
            "項目の channel は 0 起点で持つ必要があります。"
        );
        assert_eq!(
            config.map.base.rotations[&Axis::Knob],
            RotationEntry {
                channel: Some(9),
                cc: 1,
                invert: true,
                mode: RotationMode::Absolute {
                    step: 5,
                    initial: 64
                }
            }
        );
        assert_eq!(
            config.map.base.rotations[&Axis::Scroll].mode,
            RotationMode::Relative {
                step: 3,
                encoding: RelativeEncoding::BinaryOffset
            }
        );
    }

    #[test]
    fn every_control_name_maps_to_its_control() {
        let buttons = [
            ("tall", Button::Tall),
            ("side", Button::Side),
            ("top", Button::Top),
            ("short", Button::Short),
            ("scroll_press", Button::ScrollPress),
            ("up", Button::DpadUp),
            ("down", Button::DpadDown),
            ("left", Button::DpadLeft),
            ("right", Button::DpadRight),
            ("c1", Button::C1),
            ("c2", Button::C2),
            ("tour", Button::Tour),
            ("knob_press", Button::KnobPress),
            ("dial_press", Button::DialPress),
        ];
        let axes = [
            ("knob", Axis::Knob),
            ("scroll", Axis::Scroll),
            ("dial", Axis::Dial),
        ];
        let mut lines = String::new();
        for (note, (name, _)) in buttons.iter().enumerate() {
            lines.push_str(&format!("{name} = {{ note = {note} }}\n"));
        }
        for (cc, (name, _)) in axes.iter().enumerate() {
            lines.push_str(&format!("{name} = {{ cc = {cc} }}\n"));
        }

        let config = parse_ok(&with_map(&lines));

        for (note, (name, button)) in (0u8..).zip(buttons) {
            assert_eq!(
                config.map.base.buttons[&button].kind,
                ButtonKind::Note {
                    note,
                    velocity: 127
                },
                "`{name}` は {button:?} に対応する必要があります。"
            );
        }
        for (cc, (name, axis)) in (0u8..).zip(axes) {
            assert_eq!(
                config.map.base.rotations[&axis].cc, cc,
                "`{name}` は {axis:?} に対応する必要があります。"
            );
        }
    }
}
