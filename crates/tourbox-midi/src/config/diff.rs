//! 再読込の前後で変わった設定のセクションの検出。

use std::collections::BTreeSet;
use std::fmt;

use super::Config;

/// 再読込で差分を調べる設定の区分。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    /// `[map]`。
    Map,
    /// `[haptics]` の軸の値と `[haptics.with]`。`[haptics.control]` は含まない。
    Haptics,
    /// `[haptics.control]`。
    HapticsControl,
    /// `midi.output`。
    MidiOutput,
    /// `midi.input`。
    MidiInput,
    /// `midi.channel`。
    MidiChannel,
    /// `[device]`。
    Device,
}

impl Section {
    /// 設定ファイル上の名前。
    fn name(self) -> &'static str {
        match self {
            Self::Map => "map",
            Self::Haptics => "haptics",
            Self::HapticsControl => "haptics.control",
            Self::MidiOutput => "midi.output",
            Self::MidiInput => "midi.input",
            Self::MidiChannel => "midi.channel",
            Self::Device => "device",
        }
    }
}

/// 変わったセクションの集合。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangedSections(BTreeSet<Section>);

impl ChangedSections {
    pub fn contains(&self, section: Section) -> bool {
        self.0.contains(&section)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl FromIterator<Section> for ChangedSections {
    fn from_iter<T: IntoIterator<Item = Section>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl fmt::Display for ChangedSections {
    /// 設定ファイル上の名前を「、」で区切って並べる。変わったセクションがなければ「なし」にする。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return f.write_str("なし");
        }
        let names: Vec<_> = self.0.iter().map(|section| section.name()).collect();
        f.write_str(&names.join("、"))
    }
}

/// `old` から `new` への再読込で変わったセクションを返す。
pub fn diff(old: &Config, new: &Config) -> ChangedSections {
    let (old_haptics, new_haptics) = (&old.haptics, &new.haptics);
    [
        (Section::Map, old.map != new.map),
        (
            Section::Haptics,
            old_haptics.axes != new_haptics.axes || old_haptics.with != new_haptics.with,
        ),
        (
            Section::HapticsControl,
            old_haptics.control != new_haptics.control,
        ),
        (Section::MidiOutput, old.midi.output != new.midi.output),
        (Section::MidiInput, old.midi.input != new.midi.input),
        (Section::MidiChannel, old.midi.channel != new.midi.channel),
        (Section::Device, old.device != new.device),
    ]
    .into_iter()
    .filter_map(|(section, changed)| changed.then_some(section))
    .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// 全セクションを書いた設定。各テストはこの一部を書き換える。
    const BASE: &str = r#"
[device]
transport = "auto"

[midi]
output = "TourBox MIDI"
input = "TourBox MIDI In"
channel = 1

[haptics]
knob = { strength = "strong", speed = "medium" }
[haptics.with.side]
knob = { strength = "weak" }

[haptics.control]
knob = { cc = 100 }

[map]
side = { note = 50 }
knob = { cc = 1 }
"#;

    fn parse(text: &str) -> Config {
        Config::parse(text, Path::new("config.toml"))
            .unwrap_or_else(|error| panic!("検証に通る必要があります: {error}"))
    }

    /// `BASE` の `from` (1 か所だけにある文字列) を `to` に置き換えた設定。
    fn replaced(from: &str, to: &str) -> Config {
        assert_eq!(
            BASE.matches(from).count(),
            1,
            "置き換える文字列 {from:?} は BASE に 1 か所だけある必要があります。"
        );
        parse(&BASE.replace(from, to))
    }

    fn sections(sections: &[Section]) -> ChangedSections {
        sections.iter().copied().collect()
    }

    #[test]
    fn identical_configs_have_no_changed_sections() {
        let changed = diff(&parse(BASE), &parse(BASE));

        assert!(
            changed.is_empty(),
            "同じ設定には差分がない必要があります: {changed:?}"
        );
    }

    #[test]
    fn change_of_each_section_is_detected_alone() {
        let cases = [
            (
                "map",
                "side = { note = 50 }",
                "side = { note = 51 }",
                Section::Map,
            ),
            (
                "haptics の軸の値",
                r#"knob = { strength = "strong", speed = "medium" }"#,
                r#"knob = { strength = "weak", speed = "medium" }"#,
                Section::Haptics,
            ),
            (
                "haptics.with",
                r#"knob = { strength = "weak" }"#,
                r#"knob = { strength = "off" }"#,
                Section::Haptics,
            ),
            (
                "haptics.control",
                "knob = { cc = 100 }",
                "knob = { cc = 101 }",
                Section::HapticsControl,
            ),
            (
                "midi.output",
                r#"output = "TourBox MIDI""#,
                r#"output = "loopMIDI Port""#,
                Section::MidiOutput,
            ),
            (
                "midi.input の名前",
                r#"input = "TourBox MIDI In""#,
                r#"input = "loopMIDI In""#,
                Section::MidiInput,
            ),
            (
                "midi.input の削除",
                "input = \"TourBox MIDI In\"\n",
                "",
                Section::MidiInput,
            ),
            (
                "midi.channel",
                "channel = 1",
                "channel = 2",
                Section::MidiChannel,
            ),
            (
                "device.transport",
                r#"transport = "auto""#,
                r#"transport = "usb""#,
                Section::Device,
            ),
            (
                "device.usb_port の追加",
                r#"transport = "auto""#,
                "transport = \"auto\"\nusb_port = \"COM3\"",
                Section::Device,
            ),
        ];

        for (name, from, to, expected) in cases {
            assert_eq!(
                diff(&parse(BASE), &replaced(from, to)),
                sections(&[expected]),
                "{name} の変更は {expected:?} だけとして検出される必要があります。"
            );
        }
    }

    #[test]
    fn changes_in_several_sections_are_all_detected() {
        let text = BASE
            .replace("channel = 1", "channel = 16")
            .replace(r#"transport = "auto""#, r#"transport = "ble""#)
            .replace("knob = { cc = 100 }", "knob = { cc = 100, speed_cc = 101 }");

        assert_eq!(
            diff(&parse(BASE), &parse(&text)),
            sections(&[
                Section::HapticsControl,
                Section::MidiChannel,
                Section::Device
            ]),
            "変わったセクションをすべて検出する必要があります。"
        );
    }

    #[test]
    fn changed_sections_are_displayed_with_names_in_config_file() {
        assert_eq!(
            sections(&[
                Section::Device,
                Section::Map,
                Section::MidiChannel,
                Section::Haptics,
                Section::HapticsControl,
                Section::MidiInput,
                Section::MidiOutput,
            ])
            .to_string(),
            "map、haptics、haptics.control、midi.output、midi.input、midi.channel、device",
            "設定ファイル上の名前を決まった順に「、」で区切って表示する必要があります。"
        );
        assert_eq!(
            ChangedSections::default().to_string(),
            "なし",
            "変わったセクションがなければ「なし」と表示する必要があります。"
        );
    }
}
